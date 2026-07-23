//! `git upload-archive--writer` — the archiver half of the `upload-archive`
//! protocol pair.
//!
//! This is a port of `cmd_upload_archive_writer` in git's
//! `builtin/upload-archive.c`. It is the child process git's `upload-archive`
//! front end spawns: it reads the client's `argument <arg>` pkt-lines from
//! stdin, then produces the archive on **raw stdout** with no sideband framing
//! at all — the parent (`super::upload_archive`) is what multiplexes this
//! process's stdout and stderr onto bands 1 and 2. Nothing here writes `ACK`,
//! a flush packet, or a band byte.
//!
//! Behaviour verified against stock git 2.55.0 on a fixture repository:
//!   * `-h` as the only argument: `usage: git upload-archive <repository>` on
//!     **stdout**, exit 129.
//!   * any other argument count: the same line on **stderr**, exit 129. There
//!     is no option parsing whatsoever — `git upload-archive--writer --foo`
//!     treats `--foo` as the repository path and fails the repo check, so no
//!     flag can be "unrecognised" here and none is rejected as such.
//!   * a path that is not a repository: `fatal: '<path>' does not appear to be
//!     a git repository`, exit 128.
//!   * argument-stream errors, each `fatal: ` on stderr with exit 128 — end of
//!     input before a flush packet (`the remote end hung up unexpectedly`), a
//!     malformed length header (`protocol error: bad line length character:
//!     <hdr>`), a pkt-line that is not an `argument` token (`'argument' token
//!     or flush expected`), and more than 64 client arguments (`Too many
//!     options (>63)`). The count check runs before the token check, as in git.
//!   * the remote-mode option rejections git's `parse_archive_args` performs,
//!     in git's order: `Unexpected option --remote`, then `the option '--exec'
//!     requires '--remote'`, then `Unexpected option --output` (which `-o`
//!     also trips), each exit 128.
//!   * git's reachability rule: the text before any `:` in the tree-ish must
//!     name a ref under git's dwim rules, else `no such ref: <name>`, exit 128.
//!     Raw object ids and revision expressions therefore fail even when they
//!     resolve. `uploadArchive.allowUnreachable` disables the check.
//!
//! Once the protocol layer is done the run is handed to
//! [`super::archive::archive`] with the client's argument list, executed from
//! the entered repository — that is exactly git's `write_archive(..., remote=1)`
//! minus the remote-specific checks already applied above.
//!
//! Not covered:
//!   * Whatever `super::archive::archive` does not cover is not covered here
//!     either, and its diagnostics differ from stock git's in the same way they
//!     differ when `git archive` is run directly. Concretely: `--format=zip`,
//!     `tgz` and `tar.gz` are rejected instead of produced; the usage text for
//!     an empty argument list is git's first two lines only; and an unknown
//!     option reports in this tree's style rather than git's
//!     `error: unknown option ...` plus full usage.
//!   * git's parse-options unique-prefix abbreviations. `--rem`, `--exe`,
//!     `--out` are accepted by stock git for `--remote`, `--exec`, `--output`;
//!     the scan here matches the spelled-out forms and `-o` only, so an
//!     abbreviated `--out=...` reaches the archiver rather than being rejected.
//!
//! The pkt-line reader, the `enter_repo` search, the tree-ish scan and the dwim
//! ref test are duplicated from `super::upload_archive` rather than shared: the
//! two commands are separate modules and those helpers are private to it, so
//! factoring them out would mean editing that module.

use anyhow::Result;
use std::io::Read;
use std::process::ExitCode;

/// git's `MAX_ARGS`, counting the program name the writer pushes first.
const MAX_ARGS: usize = 64;

/// The usage line git prints for both halves of the pair.
const USAGE: &str = "usage: git upload-archive <repository>";

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

/// The option spellings that are illegal when the archiver runs on behalf of a
/// remote client, in the order git tests them.
#[derive(Default)]
struct Rejected {
    remote: bool,
    exec: bool,
    output: bool,
}

pub fn upload_archive__writer(args: &[String]) -> Result<ExitCode> {
    // `-h` alone prints to stdout; every other bad argument count to stderr.
    if args.len() == 1 && args[0] == "-h" {
        println!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    if args.len() != 1 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let Some(repo) = enter_repo(&args[0]) else {
        return fatal(&format!(
            "'{}' does not appear to be a git repository",
            args[0]
        ));
    };

    // Collect `argument <arg>` pkt-lines up to the flush packet.
    let mut sent: Vec<String> = Vec::new();
    let mut stdin = std::io::stdin().lock();
    loop {
        match read_pkt(&mut stdin)? {
            Pkt::Flush => break,
            Pkt::Eof => return fatal("the remote end hung up unexpectedly"),
            Pkt::Bad(msg) => return fatal(&msg),
            Pkt::Line(buf) => {
                // git compares the vector length — which already holds the
                // pushed program name — against MAX_ARGS before pushing, so the
                // 65th client argument is the one that trips it. The check runs
                // before the token check, so an over-long list reports as such
                // even when the offending line is malformed.
                if sent.len() + 1 > MAX_ARGS {
                    return fatal(&format!("Too many options (>{})", MAX_ARGS - 1));
                }
                let Some(arg) = buf.strip_prefix(b"argument ") else {
                    return fatal("'argument' token or flush expected");
                };
                sent.push(String::from_utf8_lossy(arg).into_owned());
            }
        }
    }

    // git's `parse_archive_args` refuses the three options that only make sense
    // on the client side, in this order and before anything is resolved.
    let (rejected, treeish) = scan_args(&sent);
    if rejected.remote {
        return fatal("Unexpected option --remote");
    }
    if rejected.exec {
        return fatal("the option '--exec' requires '--remote'");
    }
    if rejected.output {
        return fatal("Unexpected option --output");
    }

    // Reachability: the client may only name a ref, optionally with a `:<path>`
    // suffix. This is git's `parse_treeish_arg` remote path; it runs before the
    // tree-ish is resolved, so an unknown ref reports as an unknown ref even
    // when the name would otherwise resolve to a reachable object.
    let allow_unreachable = repo
        .config_snapshot()
        .boolean("uploadArchive.allowUnreachable")
        .unwrap_or(false);
    if !allow_unreachable {
        if let Some(spec) = treeish {
            let refname = spec.split(':').next().unwrap_or("");
            if !dwim_ref_exists(&repo, refname) {
                return fatal(&format!("no such ref: {refname}"));
            }
        }
    }

    // git's `enter_repo` has already chdir'd by this point, and the archiver
    // reads its repository from the current directory.
    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let workdir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    std::env::set_current_dir(&workdir)?;

    super::archive::archive(&sent)
}

/// Report a git `die` verbatim and hand back git's exit code for one.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
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

/// Walk the client argument list the way `super::archive::archive` parses it,
/// noting the options that are illegal in remote mode and the tree-ish.
///
/// The tree-ish is `None` when there is no positional, or when `--list`
/// short-circuits the run before one is looked at — in both cases git's
/// reachability check never runs either.
fn scan_args(args: &[String]) -> (Rejected, Option<&str>) {
    let mut bad = Rejected::default();
    let mut treeish = None;
    let mut literal = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();
        if literal {
            if treeish.is_none() {
                treeish = Some(a);
            }
            i += 1;
            continue;
        }
        match a {
            "--" => literal = true,
            "-l" | "--list" => return (bad, None),
            "--remote" => {
                bad.remote = true;
                i += 1;
            }
            "--exec" => {
                bad.exec = true;
                i += 1;
            }
            "-o" | "--output" => {
                bad.output = true;
                i += 1;
            }
            "--format" | "--prefix" => i += 1,
            _ if a.starts_with("--remote=") => bad.remote = true,
            _ if a.starts_with("--exec=") => bad.exec = true,
            _ if a.starts_with("--output=") => bad.output = true,
            _ if a.len() > 1 && a.starts_with('-') => {}
            _ if treeish.is_none() => treeish = Some(a),
            _ => {}
        }
        i += 1;
    }
    (bad, treeish)
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
