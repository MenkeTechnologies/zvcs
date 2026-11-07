//! `git shortlog` reading `git log` output from stdin when the input ends inside
//! a record.
//!
//! `read_from_stdin()` (builtin/shortlog.c:119-167) reads through two strbufs
//! that live across the whole loop, and `strbuf_getwholeline()`
//! (strbuf.c:642-685) returns EOF without touching the buffer once the stream is
//! at end of file. A record whose header block runs into the end of the input
//! therefore keeps the last header line as its subject, and a record whose ident
//! line is the input's last line keeps the subject the previous record read.
//! zvcs started every record from an empty subject.
//!
//! `insert_one_record()` also takes the group key and the record as C strings
//! (builtin/shortlog.c:57-59), so a NUL a format expands to ends either one.
//!
//! Every expectation is stock git's output for the same input.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-shortlog-eof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let out = run(fx.stock, &fx.work, &fx.root, &["init", "-q", "."], b"");
        assert!(out.status.success());
        Some(fx)
    }

    fn same(&self, args: &[&str], input: &[u8]) {
        let want = run(self.stock, &self.work, &self.root, args, input);
        let got = run(BIN, &self.work, &self.root, args, input);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?} <<< {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn a_record_cut_short_keeps_the_last_line_read() {
    let Some(fx) = Fixture::new("cut") else { return };
    let inputs: [&[u8]; 8] = [
        b"Author: X <x@example.com>\nDate: no blank line and no newline",
        b"Author: X <x@example.com>\nDate: no blank line\n",
        b"Author: A <a@example.com>\n\n    first\nAuthor: B <b@example.com>",
        b"Author: A <a@example.com>\n\n    first\nAuthor: B <b@example.com>\n",
        b"Author: A <a@example.com>\n\n    [PATCH 1/2] first\nAuthor: B <b@example.com>\nDate: x",
        b"Author: X <x@example.com>\n\n\n",
        b"author X <x@example.com> 1 +0000\n\n    raw subject",
        b"Commit: C <c@example.com>\nAuthor: X <x@example.com>",
    ];
    for input in inputs {
        fx.same(&["shortlog"], input);
        fx.same(&["shortlog", "-e", "-n"], input);
        fx.same(&["shortlog", "-c"], input);
        fx.same(&["shortlog", "-w20,2,4"], input);
    }
}

/// `insert_one_record()` takes the group key and the record as C strings
/// (builtin/shortlog.c:57-59), so a `%x00` in `--format` or `--group=format:`
/// ends either one there.
#[test]
fn a_nul_from_a_format_ends_the_record() {
    let Some(fx) = Fixture::new("nul") else { return };
    for msg in ["one", "two"] {
        let out = run(fx.stock, &fx.work, &fx.root, &["commit", "-q", "--allow-empty", "-m", msg], b"");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    fx.same(&["shortlog", "HEAD", "--format=%x00%s"], b"");
    fx.same(&["shortlog", "HEAD", "--format=%s%x00tail"], b"");
    fx.same(&["shortlog", "HEAD", "--group=format:%an%x00%s", "-s"], b"");
    fx.same(&["shortlog", "HEAD", "--group=format:%x00%s"], b"");
}
