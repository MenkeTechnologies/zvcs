//! `rev-list` printed a non-UTF-8 commit as stored and refused `--encoding`.
//!
//! `show_commit()` (builtin/rev-list.c) sets `ctx.output_encoding =
//! get_log_output_encoding()` before `pretty_print_commit()`, which renders from
//! `repo_logmsg_reencode()` (pretty.c:2315-2316); a user format is expanded
//! against the commit re-coded to UTF-8 and the record converted afterwards
//! (pretty.c:1734, 2026-2046). `--encoding[=]<enc>` is `setup_revisions()`'s
//! option (revision.c:2701-2707): any name, `none` meaning "as stored", and a
//! missing value is `Option '--encoding' requires a value`. zvcs rendered the
//! stored ISO-8859-1 bytes under a `charset=UTF-8` header and refused every
//! `--encoding` but UTF-8 and `none`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const OID: &str = "73f5e687ca3411e85359b4a9aab891be666e9d2f";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit whose message is ISO-8859-1, with the header saying so.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rev-list-enc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q"]);
        std::fs::write(f.root.join(".git/msg"), b"caf\xe9\n\nbody \xe9\n").unwrap();
        f.run(&["-c", "i18n.commitEncoding=ISO-8859-1", "commit", "-q", "--allow-empty", "-F", ".git/msg"]);
        assert_eq!(f.run(&["rev-parse", "HEAD"]).0, format!("{OID}\n").into_bytes());
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "A U Thor")
            .env("GIT_COMMITTER_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }
}

fn with_header(body: &[u8]) -> Vec<u8> {
    let mut v = format!("commit {OID}\n").into_bytes();
    v.extend_from_slice(body);
    v
}

#[test]
fn email_is_re_coded_to_the_default_utf8() {
    let f = Fixture::new("email");
    let (out, err, code) = f.run(&["rev-list", "--pretty=email", "HEAD"]);
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert_eq!(
        out,
        with_header(
            b"From: A U Thor <author@example.com>\n\
Date: Tue, 14 Nov 2023 22:13:20 +0000\n\
Subject: caf\xc3\xa9\n\
MIME-Version: 1.0\n\
Content-Type: text/plain; charset=UTF-8\n\
Content-Transfer-Encoding: 8bit\n\
\n\
body \xc3\xa9\n\n"
        )
    );
}

#[test]
fn every_encoding_spelling_is_taken() {
    let f = Fixture::new("spell");
    let (out, _, code) = f.run(&["rev-list", "--encoding=ISO-8859-1", "--format=%s|%an", "HEAD"]);
    assert_eq!((out, code), (with_header(b"caf\xe9|A U Thor\n"), 0));

    let (out, _, code) = f.run(&["rev-list", "--encoding", "ISO-8859-1", "--oneline", "HEAD"]);
    assert_eq!((out.as_slice(), code), (&b"73f5e68 caf\xe9\n"[..], 0));

    // `none` prints the object as stored, `encoding` header and all.
    let (out, _, code) = f.run(&["rev-list", "--encoding=none", "--pretty=raw", "HEAD"]);
    assert_eq!(code, 0);
    assert!(
        out.ends_with(b"encoding ISO-8859-1\n\n    caf\xe9\n    \n    body \xe9\n\n"),
        "{}",
        String::from_utf8_lossy(&out)
    );

    // An encoding iconv does not know leaves the UTF-8 rendering alone.
    let (out, _, code) = f.run(&["rev-list", "--encoding=bogus", "--format=%s", "HEAD"]);
    assert_eq!((out, code), (with_header(b"caf\xc3\xa9\n"), 0));

    let (out, err, code) = f.run(&["rev-list", "HEAD", "--encoding"]);
    assert_eq!(
        (out.as_slice(), err.as_slice(), code),
        (&b""[..], &b"fatal: Option '--encoding' requires a value\n"[..], 128)
    );
}
