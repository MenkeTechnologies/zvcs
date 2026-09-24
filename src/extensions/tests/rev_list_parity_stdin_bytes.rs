//! `git rev-list --stdin` reads bytes, not text.
//!
//! ```c
//! while (strbuf_getline(&sb, stdin) != EOF) {
//!         if (!sb.len)
//!                 break;
//!         …
//!         if (handle_revision_arg(sb.buf, revs, flags,
//!                                 REVARG_CANNOT_BE_FILENAME))
//!                 die("bad revision '%s'", sb.buf);
//! }
//! ```
//! (revision.c:2951-2977, v2.55.0). A line is whatever `strbuf_getline()` hands
//! back — LF and one CR stripped — and past the length check it is only read as
//! the C string `sb.buf`. The port read stdin with `read_to_string()`, so any
//! non-UTF-8 byte was `stream did not contain valid UTF-8` at exit 1.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// One commit on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rl-stdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("path"), "a\n").unwrap();
        f.git(&["add", "path"]);
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

    fn run_stdin(&self, args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }
}

/// A line that starts with NUL is non-empty to the length check and the empty
/// string to everything after it.
#[test]
fn a_line_starting_with_nul_is_the_empty_revision() {
    let f = Fixture::new("nul");
    let (out, err, code) = f.run_stdin(&["rev-list", "--stdin", "HEAD"], b"\0abc\xff\n");
    assert_eq!((out.as_slice(), err.as_slice(), code), (&b""[..], &b"fatal: bad revision ''\n"[..], 128));
}

/// Only one CR is stripped, so the second stays part of the name.
#[test]
fn only_one_carriage_return_is_stripped() {
    let f = Fixture::new("cr");
    let (out, err, code) = f.run_stdin(&["rev-list", "--stdin"], b"HEAD\r\r\n");
    assert_eq!((out.as_slice(), err.as_slice(), code), (&b""[..], &b"fatal: bad revision 'HEAD\r'\n"[..], 128));
    let (out, err, code) = f.run_stdin(&["rev-list", "--stdin"], b"HEAD\r\n");
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert_eq!(out, b"3d45988a7d5b295f1de59a48e10c279d4234391d\n");
}

/// A pathspec line after `--` is a C string too: `path\0nope` limits to `path`.
#[test]
fn a_pathspec_line_ends_at_nul() {
    let f = Fixture::new("path");
    let (out, err, code) = f.run_stdin(&["rev-list", "--stdin"], b"HEAD\n--\npath\0nope\n");
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert_eq!(out, b"3d45988a7d5b295f1de59a48e10c279d4234391d\n");
}
