//! `git commit -F -` reads its log message as bytes, not as text.
//!
//! git's stdin arm is `if (strbuf_read(&sb, 0, 0) < 0) die_errno(_("could not
//! read log from standard input"));` (builtin/commit.c:810-811) — a raw read
//! into a strbuf, with no encoding check anywhere on the way to the object.
//! That is what makes `i18n.commitEncoding` usable at all: the whole point of
//! the knob is to record a message that is *not* UTF-8, and t7102-reset.sh's
//! own setup pipes an ISO8859-1 message into `commit -a -F -` before any of its
//! reset assertions can run.
//!
//! The port read stdin with `read_to_string`, which fails the whole command on
//! the first non-UTF-8 byte (`stream did not contain valid UTF-8`) — a commit
//! git makes without complaint, and the failure cascaded into every later test
//! in that file that needed the resulting history. The `-F <file>` arm next to
//! it already read bytes, so the two spellings of the same option disagreed.
//!
//! Measured against stock git 2.55.0 in a throwaway repository under the same
//! pinned environment: the commit succeeds, exit status 0, and `HEAD` gains a
//! commit whose message byte length matches what was piped in.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `modify 2nd file (geändert)\n` in ISO8859-1 — the literal message
/// t7102-reset.sh's `commit_msg ISO8859-1` produces. 0xe4 is `ä`, which is not
/// a valid UTF-8 lead byte on its own.
const LATIN1_MSG: &[u8] = b"modify 2nd file (ge\xe4ndert)\n";

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
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-commit-f-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run `args` with `stdin` piped in verbatim, returning stderr and status.
    fn with_stdin(&self, args: &[&str], stdin: &[u8]) -> (String, i32) {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// The regression itself: a non-UTF-8 message on stdin must commit, not die.
#[test]
fn index_parity_commit_dash_f_stdin_accepts_non_utf8_message() {
    let f = Fixture::new("latin1");
    std::fs::write(f.work.join("a"), "b\n").unwrap();

    let (err, code) = f.with_stdin(
        &["-c", "i18n.commitEncoding=ISO8859-1", "commit", "-a", "-F", "-"],
        LATIN1_MSG,
    );
    assert_eq!(code, 0, "commit -F - refused a non-UTF-8 message: {err}");
    assert!(
        !err.contains("valid UTF-8"),
        "commit -F - complained about the encoding: {err}"
    );

    // The commit is real and is the new tip, so the history a caller builds on
    // top of it exists — that is what the cascade in t7102-reset.sh needed.
    assert_eq!(
        f.stdout(&["rev-list", "--count", "HEAD"]).trim(),
        "2",
        "the -F - commit did not land on HEAD"
    );
    // `i18n.commitEncoding` is recorded on the object, matching
    // `if (!encoding_is_utf8) strbuf_addf(buffer, "encoding %s\n", ...)`
    // (commit.c, `commit_tree_extended`).
    assert!(
        f.stdout(&["cat-file", "commit", "HEAD"]).contains("encoding ISO8859-1"),
        "the encoding header is missing from the -F - commit"
    );
}

/// An empty stdin is still an empty message, which git refuses — the byte read
/// must not turn "nothing to commit a message from" into a success.
#[test]
fn index_parity_commit_dash_f_stdin_still_refuses_an_empty_message() {
    let f = Fixture::new("empty");
    std::fs::write(f.work.join("a"), "b\n").unwrap();

    let (err, code) = f.with_stdin(&["commit", "-a", "-F", "-"], b"");
    assert_ne!(code, 0, "an empty -F - message was accepted: {err}");
    assert_eq!(
        f.stdout(&["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "an empty -F - message still produced a commit"
    );
}

/// A plain ASCII message through stdin is unchanged by the byte read: the
/// subject is recorded verbatim.
#[test]
fn index_parity_commit_dash_f_stdin_keeps_an_ascii_message_verbatim() {
    let f = Fixture::new("ascii");
    std::fs::write(f.work.join("a"), "b\n").unwrap();

    let (err, code) = f.with_stdin(&["commit", "-a", "-F", "-"], b"subject line\n\nbody\n");
    assert_eq!(code, 0, "commit -F - failed: {err}");
    assert_eq!(
        f.stdout(&["log", "-1", "--format=%s%n%b"]),
        "subject line\nbody\n\n",
        "the -F - message was reshaped"
    );
}
