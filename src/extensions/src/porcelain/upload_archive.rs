//! `git upload-archive` — serve an archive to `git archive --remote` over the
//! Git protocol.
//!
//! This is a port of `cmd_upload_archive` in git's `builtin/upload-archive.c`:
//! the protocol front end only. It does no option parsing, no repository
//! lookup and no pkt-line reading of its own — every one of those lives in the
//! archiver back end, git's `upload-archive--writer`, which this process spawns
//! as a child and whose stdout and stderr it multiplexes onto the two sidebands
//! the protocol defines. See [`super::upload_archive__writer`] for that half.
//!
//! Wire format, verified byte-for-byte against stock `git upload-archive`
//! (2.55.0) on a fixture repository:
//!   * the writer is spawned first, then `0008ACK\n` and a flush packet.
//!   * the writer's stdout on sideband 1 and its stderr on sideband 2, framed
//!     at `LARGE_PACKET_MAX`.
//!   * on a clean writer exit a trailing flush packet and exit 0.
//!   * on a non-zero writer exit sideband 3 carries `git upload-archive:
//!     archiver died with error` (no newline), there is no trailing flush,
//!     `fatal: sent error to the client: ...` goes to this process's stderr,
//!     and the exit code is 128.
//!
//! Because the front end forwards rather than interprets, every diagnostic the
//! client sees is the writer's, on band 2, with git's exact text: the usage line
//! for a bad argument count, `'<path>' does not appear to be a git repository`,
//! `the remote end hung up unexpectedly` for a truncated argument stream,
//! `'argument' token or flush expected`, `Too many options (>63)`, and the
//! reachability refusal `no such ref: <name>`.
//!
//! Covered here in the front end itself:
//!   * `-h` as the only argument, git's `show_usage_if_asked`: `usage: git
//!     upload-archive <repository>` on **stdout**, exit 129, before the writer
//!     is spawned and before `ACK`. `-h` in any other position is passed
//!     through to the writer like any other argument, as git passes it.
//!   * a writer that cannot be spawned: the `NACK unable to spawn subprocess`
//!     pkt-line, then `fatal: upload-archive: <reason>` on stderr and exit 128,
//!     with no `ACK` — git's `start_command` failure path.
//!
//! Not covered:
//!   * Byte-exact sideband packet boundaries for archives over 16 KiB. git
//!     frames one packet per `read()` of a 16 KiB buffer from a pipe, so the
//!     split depends on scheduling and is not stable between two stock runs
//!     either. This port frames at the same 16 KiB, which reproduces stock's
//!     framing whenever the pipe stays ahead of the reader — always the case
//!     for a single-packet archive.
//!   * git's poll loop drains the writer's stderr before its stdout within one
//!     iteration; the two reader threads here interleave by arrival instead. No
//!     path that writes to both streams exists in the back end, so the two
//!     orders only differ for a diagnostic emitted mid-archive.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{channel, Sender};

/// The read size of git's `process_input` buffer; one sideband packet per read.
const READ_CHUNK: usize = 16384;

/// `LARGE_PACKET_MAX` minus the 4-byte length and the 1-byte band.
const SIDEBAND_MAX: usize = 65515;

/// The message git's front end hands to the client on band 3 when the archiver
/// exits non-zero. Sent without a trailing newline, exactly as git sends it.
const DEADCHILD: &str = "git upload-archive: archiver died with error";

/// The usage line git prints for both halves of the pair.
const USAGE: &str = "usage: git upload-archive <repository>";

pub fn upload_archive(args: &[String]) -> Result<ExitCode> {
    // git's `show_usage_if_asked`: `-h` or `--help-all` as the sole argument
    // prints to stdout and exits 129, before anything protocol-shaped happens.
    // There is no hidden option to add, so both print the same line.
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help-all") {
        println!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // git spawns the writer before sending ACK, so a spawn failure is the one
    // error the client learns about as a NACK rather than on a sideband. The
    // writer inherits this process's stdin: it, not this process, reads the
    // client's argument pkt-lines.
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("upload-archive--writer")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(err) => {
            let line = "NACK unable to spawn subprocess\n";
            write!(out, "{:04x}{line}", line.len() + 4)?;
            out.flush()?;
            eprintln!("fatal: upload-archive: {err}");
            return Ok(ExitCode::from(128));
        }
    };

    out.write_all(b"0008ACK\n0000")?;
    out.flush()?;

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

    // git's `finish_command` non-zero branch: `error_clnt(deadchild)`.
    if !child.wait()?.success() {
        sideband(&mut out, 3, DEADCHILD.as_bytes())?;
        out.flush()?;
        // No trailing flush packet on this path: git's `die` runs first.
        eprintln!("fatal: sent error to the client: {DEADCHILD}");
        return Ok(ExitCode::from(128));
    }

    out.write_all(b"0000")?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
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
