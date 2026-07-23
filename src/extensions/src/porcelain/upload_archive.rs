//! `git upload-archive` — serve an archive to `git archive --remote` over the
//! Git protocol.
//!
//! This is a port of git's `builtin/upload-archive.c`, including the split
//! between the protocol front end (this function) and the archiver back end
//! (git's `upload-archive--writer`, here the `archive` subcommand of this same
//! binary, spawned as a child so its stdout and stderr stay on separate fds and
//! can be multiplexed onto the two sidebands the protocol defines).
//!
//! Wire format, verified byte-for-byte against stock `git upload-archive`
//! (2.55.0) on a fixture repository:
//!   * `0008ACK\n` followed by a flush packet, written before anything is read
//!     from stdin.
//!   * client `argument <arg>\n` pkt-lines, terminated by a flush packet.
//!   * the archive on sideband 1, the archiver's diagnostics on sideband 2, and
//!     a fatal handoff on sideband 3.
//!   * on success a trailing flush packet and exit 0; on failure sideband 3
//!     carries `git upload-archive: archiver died with error` (no newline),
//!     there is no trailing flush, `fatal: sent error to the client: ...` goes
//!     to this process's stderr, and the exit code is 128.
//!
//! Covered:
//!   * `git upload-archive <repository>`, with git's non-strict `enter_repo`
//!     path expansion (a worktree root, a `.git` directory, or a bare repo).
//!   * `-h` as the only argument: `usage: git upload-archive <repository>` on
//!     stdout, exit 129.
//!   * The argument-stream errors, each on sideband 2 with git's exact text:
//!     end of input before a flush (`the remote end hung up unexpectedly`), a
//!     pkt-line that is not an `argument` token (`'argument' token or flush
//!     expected`), and more than 64 client arguments (`Too many options
//!     (>63)`).
//!   * git's `upload-archive` reachability rules: the part of the tree-ish
//!     before any `:` must name a ref under git's dwim rules, otherwise
//!     `no such ref: <name>`. Raw object ids and revision expressions are
//!     rejected exactly as git rejects them. `uploadArchive.allowUnreachable`
//!     turns the check off.
//!
//! Not covered:
//!   * The `NACK unable to spawn subprocess` pkt-line. git spawns its writer
//!     before sending ACK; here the child cannot be spawned until the argument
//!     stream has been read, which is necessarily after ACK. A spawn failure
//!     therefore reports on sideband 3 like any other archiver failure. The
//!     bytes on the wire are otherwise unaffected.
//!   * Byte-exact sideband packet boundaries for archives over 16 KiB. git
//!     frames one packet per `read()` of a 16 KiB buffer from a pipe, so the
//!     split depends on scheduling and is not stable between two stock runs
//!     either. This port frames at the same 16 KiB, which reproduces stock's
//!     framing whenever the pipe stays ahead of the reader — always the case
//!     for a single-packet archive.
//!   * Whatever the `archive` subcommand of this binary does not cover: its
//!     diagnostics are forwarded verbatim on sideband 2, so its usage text and
//!     its unsupported-format errors differ from stock git's in the same way
//!     they differ when `git archive` is run directly.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{channel, Sender};

/// git's `MAX_ARGS`: the writer refuses a client argument list longer than this
/// counting the program name it pushes first.
const MAX_ARGS: usize = 64;

/// The read size of git's `process_input` buffer; one sideband packet per read.
const READ_CHUNK: usize = 16384;

/// `LARGE_PACKET_MAX` minus the 4-byte length and the 1-byte band.
const SIDEBAND_MAX: usize = 65515;

/// The message git's parent hands to the client on band 3 when the archiver
/// exits non-zero. Sent without a trailing newline, exactly as git sends it.
const DIED: &str = "git upload-archive: archiver died with error";

/// One pkt-line read from the client.
enum Pkt {
    /// A data packet, with any single trailing newline chomped.
    Line(Vec<u8>),
    /// A flush packet: the argument list is complete.
    Flush,
    /// End of input before a flush packet.
    Eof,
    /// A malformed length header, carrying git's message for it.
    Bad(String),
}

pub fn upload_archive(args: &[String]) -> Result<ExitCode> {
    // git checks `-h` before anything else and prints its usage to stdout.
    if args.len() == 2 && args[1] == "-h" {
        println!("usage: git upload-archive <repository>");
        return Ok(ExitCode::from(129));
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // ACK goes out before a single byte of stdin is consumed; the client sends
    // its whole argument list before waiting for it, so this cannot deadlock.
    out.write_all(b"0008ACK\n0000")?;
    out.flush()?;

    // The writer git spawns takes exactly one argument, the repository.
    if args.len() != 2 {
        return die_clnt(&mut out, "usage: git upload-archive <repository>");
    }

    let Some(repo) = enter_repo(&args[1]) else {
        return die_clnt(
            &mut out,
            &format!("'{}' does not appear to be a git repository", args[1]),
        );
    };

    // Collect `argument <arg>` pkt-lines up to the flush packet.
    let mut sent: Vec<String> = Vec::new();
    let mut stdin = std::io::stdin().lock();
    loop {
        match read_pkt(&mut stdin)? {
            Pkt::Flush => break,
            Pkt::Eof => return die_clnt(&mut out, "the remote end hung up unexpectedly"),
            Pkt::Bad(msg) => return die_clnt(&mut out, &msg),
            Pkt::Line(buf) => {
                // The count git compares against MAX_ARGS includes the program
                // name it pushed first, so the 65th client argument is the one
                // that trips it.
                if sent.len() + 1 > MAX_ARGS {
                    return die_clnt(&mut out, &format!("Too many options (>{})", MAX_ARGS - 1));
                }
                let Some(arg) = buf.strip_prefix(b"argument ") else {
                    return die_clnt(&mut out, "'argument' token or flush expected");
                };
                sent.push(String::from_utf8_lossy(arg).into_owned());
            }
        }
    }

    // Reachability: the client may only name a ref, optionally with a `:<path>`
    // suffix. This is git's `parse_treeish_arg` remote path, and it runs before
    // the tree-ish is resolved, so an unknown ref reports as such even when the
    // name would resolve to a reachable object.
    let allow_unreachable = repo
        .config_snapshot()
        .boolean("uploadArchive.allowUnreachable")
        .unwrap_or(false);
    if !allow_unreachable {
        if let Some(treeish) = treeish_arg(&sent) {
            let refname = treeish.split(':').next().unwrap_or("");
            if !dwim_ref_exists(&repo, refname) {
                return die_clnt(&mut out, &format!("no such ref: {refname}"));
            }
        }
    }

    // Run the archiver where git runs it: inside the repository it entered.
    let workdir = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("archive")
        .args(&sent)
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return fail_clnt(&mut out),
    };

    // Multiplex the archiver's two streams onto bands 1 and 2 as they arrive,
    // which is what git's poll loop over the child's two pipes amounts to.
    let (tx, rx) = channel::<(u8, Vec<u8>)>();
    let child_out = child.stdout.take().expect("stdout piped");
    let child_err = child.stderr.take().expect("stderr piped");
    let tx_err = tx.clone();
    let pump_out = std::thread::spawn(move || pump(child_out, 1, &tx));
    let pump_err = std::thread::spawn(move || pump(child_err, 2, &tx_err));
    for (band, payload) in rx {
        sideband(&mut out, band, &payload)?;
    }
    let _ = pump_out.join();
    let _ = pump_err.join();

    let status = child.wait()?;
    if !status.success() {
        return fail_clnt(&mut out);
    }

    out.write_all(b"0000")?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Report an archiver-side fatal exactly as git's writer plus parent do: the
/// message on band 2 with git's `fatal: ` prefix and a newline, then the band 3
/// handoff, then this process's own `die`.
fn die_clnt(out: &mut impl Write, msg: &str) -> Result<ExitCode> {
    sideband(out, 2, format!("fatal: {msg}\n").as_bytes())?;
    fail_clnt(out)
}

/// The band 3 handoff and `die` alone, for a failure with no writer-side
/// message of its own (a non-zero archiver exit that already spoke on band 2).
fn fail_clnt(out: &mut impl Write) -> Result<ExitCode> {
    sideband(out, 3, DIED.as_bytes())?;
    out.flush()?;
    // Note: no trailing flush packet on this path, matching git, whose `die`
    // runs before `packet_flush`.
    eprintln!("fatal: sent error to the client: {DIED}");
    Ok(ExitCode::from(128))
}

/// Write `payload` as sideband `band` packets, splitting at git's
/// `LARGE_PACKET_MAX`. An empty payload writes nothing, as in `send_sideband`.
fn sideband(out: &mut impl Write, band: u8, payload: &[u8]) -> Result<()> {
    for chunk in payload.chunks(SIDEBAND_MAX) {
        write!(out, "{:04x}", chunk.len() + 5)?;
        out.write_all(&[band])?;
        out.write_all(chunk)?;
    }
    Ok(())
}

/// Forward everything readable from `src` to `tx`, one message per `read()`.
fn pump(mut src: impl Read, band: u8, tx: &Sender<(u8, Vec<u8>)>) {
    let mut buf = [0u8; READ_CHUNK];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send((band, buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
}

/// Read one pkt-line, chomping a single trailing newline as git's
/// `packet_read_line` does.
fn read_pkt(src: &mut impl Read) -> Result<Pkt> {
    let mut head = [0u8; 4];
    if fill(src, &mut head)? != 4 {
        return Ok(Pkt::Eof);
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let Ok(len) = usize::from_str_radix(&text, 16) else {
        return Ok(Pkt::Bad(format!(
            "protocol error: bad line length character: {text}"
        )));
    };
    if len == 0 {
        return Ok(Pkt::Flush);
    }
    if len < 4 {
        return Ok(Pkt::Bad(format!("protocol error: bad line length {len}")));
    }
    let mut body = vec![0u8; len - 4];
    if fill(src, &mut body)? != body.len() {
        return Ok(Pkt::Eof);
    }
    if body.last() == Some(&b'\n') {
        body.pop();
    }
    Ok(Pkt::Line(body))
}

/// Read until `buf` is full or input ends; returns how many bytes were read.
fn fill(src: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match src.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// git's non-strict `enter_repo`: the argument may name a worktree, a `.git`
/// directory, or a bare repository, with `.git` appended if that is what makes
/// it a repository.
fn enter_repo(path: &str) -> Option<gix::Repository> {
    let candidates = [
        path.to_string(),
        format!("{path}/.git"),
        format!("{path}.git"),
        format!("{path}.git/.git"),
    ];
    candidates.iter().find_map(|c| gix::open(c).ok())
}

/// The tree-ish positional in a client argument list, skipping options and the
/// values of the options that take a separate one. Mirrors the parser in this
/// tree's `archive` subcommand, which is what actually consumes these.
///
/// Returns `None` when there is no positional, or when `--list` short-circuits
/// the run before a tree-ish is ever looked at — in both cases git's
/// reachability check never runs either.
fn treeish_arg(args: &[String]) -> Option<&str> {
    let mut i = 0;
    let mut literal = false;
    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            return Some(a);
        }
        match a {
            "--" => literal = true,
            "-l" | "--list" => return None,
            "--format" | "--prefix" | "-o" | "--output" => i += 1,
            _ if a.len() > 1 && a.starts_with('-') => {}
            _ => return Some(a),
        }
        i += 1;
    }
    None
}

/// Whether `name` resolves to a ref under git's `ref_rev_parse_rules`, the test
/// `repo_dwim_ref` applies. A revision expression or a raw object id matches
/// nothing here, which is what makes rule 3 of the protocol's security model
/// hold.
fn dwim_ref_exists(repo: &gix::Repository, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let candidates = [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    candidates
        .iter()
        .any(|c| matches!(repo.try_find_reference(c.as_str()), Ok(Some(_))))
}
