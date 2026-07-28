//! What a command does when the reader of its stdout goes away.
//!
//! Stock git leaves `SIGPIPE` at its default disposition, so `git <cmd> | head`
//! dies from the signal: the wait status is a signal death (which a shell
//! reports as 141) and nothing is written to stderr. Rust ignores `SIGPIPE`
//! before `main`, so the same write returned `EPIPE` here and surfaced as
//! `zvcs: <cmd>: Broken pipe (os error 32)` with exit 1 — a spurious diagnostic
//! and a failing status for what git treats as a normal stop.
//!
//! Every expectation below was taken from stock git 2.55.0 run the same way.
//!
//! The reader is closed *before* the child can finish writing, so the payload
//! has to be far larger than any pipe buffer the kernel might hand us — macOS
//! starts a pipe at 16 KiB and grows it to 64 KiB, and Linux defaults to 64 KiB.
//! The blob written here is 4 MiB so the child is guaranteed to still be writing.
//!
//! Unix-only: the assertion is about signal death, which has no Windows analogue.
#![cfg(unix)]

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `SIGPIPE`. Hard-coded rather than pulled from `libc` so the test crate needs
/// no dependency of its own; the number is fixed by POSIX on every Unix.
const SIGPIPE: i32 = 13;

/// Bigger than any pipe buffer the kernel will give us, so the child is still
/// writing when the reader disappears.
const BLOB_BYTES: usize = 4 * 1024 * 1024;

/// A scratch repository holding one large blob, served by the binary under test.
struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sigpipe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };

        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        // Distinct lines: a run of identical bytes would let a filter or the
        // object store collapse the payload and shrink what reaches the pipe.
        let mut big = String::with_capacity(BLOB_BYTES + 64);
        let mut n = 0u64;
        while big.len() < BLOB_BYTES {
            big.push_str(&format!("line {n} of the payload under test\n"));
            n += 1;
        }
        std::fs::write(f.work.join("big"), &big).unwrap();
        f.git(&["add", "big"]);
        f.git(&["commit", "-q", "-m", "big"]);
        f
    }

    /// Run a verb to completion, asserting it succeeded. Used for setup only.
    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// A command against the fixture, isolated from the invoking user's config
    /// so CI and a developer's machine agree.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }
}

/// Start `args`, read a little of the output, then close the pipe and report how
/// the child died and what it said on stderr.
fn close_pipe_early(f: &Fixture, args: &[&str]) -> (Option<i32>, Option<i32>, String) {
    let mut child = f
        .cmd(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Take a token amount so the child has certainly started writing, then drop
    // the handle — that closes our end and is what `| head -1` does on exit.
    let mut out = child.stdout.take().unwrap();
    let mut buf = [0u8; 64];
    let _ = out.read(&mut buf);
    drop(out);

    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let status = child.wait().unwrap();
    (status.signal(), status.code(), err)
}

/// The central path: an error carrying `EPIPE` reaches the top-level handler,
/// which used to print it. It must now die the way git dies, saying nothing.
#[test]
fn closed_stdout_kills_the_command_like_git() {
    let f = Fixture::new("plain");
    let (signal, code, err) = close_pipe_early(&f, &["cat-file", "blob", "HEAD:big"]);
    assert_eq!(
        signal,
        Some(SIGPIPE),
        "expected death by SIGPIPE like git, got signal={signal:?} code={code:?} stderr={err:?}"
    );
    assert!(err.is_empty(), "git says nothing when its pipe closes, but got: {err:?}");
}

/// `log` caught `BrokenPipe` itself and returned success, which is the same bug
/// wearing the opposite sign: git reports 141 there, not 0.
#[test]
fn log_does_not_swallow_a_closed_pipe() {
    let f = Fixture::new("log");
    let (signal, code, err) = close_pipe_early(&f, &["log", "-p"]);
    assert_eq!(
        signal,
        Some(SIGPIPE),
        "log must not report success for a closed pipe, got signal={signal:?} code={code:?} stderr={err:?}"
    );
    assert!(err.is_empty(), "log must stay silent on a closed pipe, but got: {err:?}");
}

/// The guard against over-reaching: a reader that consumes everything is not a
/// broken pipe, and the command still exits 0 with its output intact.
#[test]
fn fully_consumed_output_still_succeeds() {
    let f = Fixture::new("whole");
    let out = f.cmd(&["cat-file", "blob", "HEAD:big"]).output().unwrap();
    assert!(out.status.success(), "expected success, got {:?}", out.status);
    assert!(out.stdout.len() >= BLOB_BYTES, "payload truncated: {} bytes", out.stdout.len());
    assert!(out.stderr.is_empty(), "unexpected stderr: {:?}", out.stderr);
}
