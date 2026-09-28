//! A commit message that is not UTF-8 was rewritten into U+FFFD.
//!
//! git treats the message as bytes end to end: `-F` is `strbuf_read_file()`
//! (builtin/commit.c:814), `-m` is the raw argv string, the editor's
//! `COMMIT_EDITMSG` is read back with `strbuf_read_file()`, and `-C` copies the
//! stored message. `commit_tree_extended()` then writes
//! `encoding <i18n.commitEncoding>` when that is not UTF-8 (commit.c:1723-1724)
//! and otherwise runs `ensure_utf8()` over the buffer (commit.c:1770-1772), which
//! transcribes each stray byte as Latin-1 and prints `commit_utf8_warn`.
//! `print_commit_summary()` (sequencer.c:1413) formats the commit it just wrote,
//! so its subject is re-coded to `i18n.logOutputEncoding`.
//!
//! zvcs decoded `-F`/stdin with `from_utf8_lossy`, `-C`/`--amend` with
//! `to_string()`, died reading back a non-UTF-8 `COMMIT_EDITMSG`, panicked on a
//! non-UTF-8 `-m`, never ran `ensure_utf8()`, and printed the summary subject
//! from its own buffer.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const WARN: &[u8] = b"Warning: commit message did not conform to UTF-8.\n\
You may want to amend it after fixing the message, or set the config\n\
variable i18n.commitEncoding to the encoding your project uses.\n";

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
        let root = std::env::temp_dir()
            .join(format!("zvcs-commit-non-utf8-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn run<S: Into<OsString> + Clone>(&self, args: &[S]) -> (Vec<u8>, Vec<u8>, i32) {
        self.run_env(args, &[])
    }

    fn run_env<S: Into<OsString> + Clone>(
        &self,
        args: &[S],
        env: &[(&str, &std::path::Path)],
    ) -> (Vec<u8>, Vec<u8>, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args.iter().cloned().map(Into::into))
            .current_dir(&self.work)
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
            .env("TZ", "UTC");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }

    fn head_object(&self) -> Vec<u8> {
        self.run(&["cat-file", "commit", "HEAD"]).0
    }
}

fn raw(b: &[u8]) -> OsString {
    OsString::from_vec(b.to_vec())
}

fn tail(object: &[u8]) -> &[u8] {
    let p = object.windows(2).position(|w| w == b"\n\n").unwrap();
    // The `encoding` header, if any, sits right before the blank line.
    let start = object[..p].iter().rposition(|&b| b == b'\n').map_or(0, |n| n + 1);
    &object[start..]
}

#[test]
fn a_utf8_repository_transcribes_a_stray_byte_as_latin1_and_warns() {
    let f = Fixture::new("m");
    let (out, err, code) = f.run(&[raw(b"commit"), raw(b"--allow-empty"), raw(b"-m"), raw(b"caf\xe9")]);
    assert_eq!(code, 0);
    assert_eq!(err, WARN);
    assert_eq!(out, b"[main (root-commit) cb1bdd0] caf\xc3\xa9\n");
    assert_eq!(tail(&f.head_object()), b"committer A U Thor <author@example.com> 1700000000 +0000\n\ncaf\xc3\xa9\n");
}

#[test]
fn a_latin1_repository_keeps_the_bytes_from_every_source() {
    let f = Fixture::new("f");
    f.run(&["config", "i18n.commitEncoding", "ISO-8859-1"]);
    let msg = f.root.join("msg");
    std::fs::write(&msg, b"caf\xe9 message\n\nbody \xe9\n").unwrap();
    let want = b"encoding ISO-8859-1\n\ncaf\xe9 message\n\nbody \xe9\n";

    // `-F`, with the summary re-coded to the log output encoding.
    let (out, err, code) = f.run(&[
        OsString::from("-c"),
        "i18n.logOutputEncoding=UTF-8".into(),
        "commit".into(),
        "--allow-empty".into(),
        "-F".into(),
        msg.clone().into(),
    ]);
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert!(out.ends_with(b"] caf\xc3\xa9 message\n"), "{}", String::from_utf8_lossy(&out));
    assert_eq!(tail(&f.head_object()), want);

    // `-C HEAD` copies the stored bytes; the summary is in the commit encoding.
    let (out, _, code) = f.run(&["commit", "--allow-empty", "-q", "-C", "HEAD"]);
    assert_eq!((out.as_slice(), code), (&b""[..], 0));
    assert_eq!(tail(&f.head_object()), want);

    // `-m` straight from argv.
    let (_, err, code) = f.run(&[raw(b"commit"), raw(b"--allow-empty"), raw(b"-q"), raw(b"-m"), raw(b"m\xe9")]);
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert_eq!(tail(&f.head_object()), b"encoding ISO-8859-1\n\nm\xe9\n");

    // The editor's `COMMIT_EDITMSG`, read back as bytes.
    let editor = f.root.join("ed.sh");
    std::fs::write(&editor, b"#!/bin/sh\nprintf 'ed\\351it\\n' > \"$1\"\n").unwrap();
    let mut perm = std::fs::metadata(&editor).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&editor, perm).unwrap();
    let (out, err, code) = f.run_env(&["commit", "--allow-empty"], &[("GIT_EDITOR", &editor)]);
    assert_eq!((err.as_slice(), code), (&b""[..], 0));
    assert!(out.ends_with(b"] ed\xe9it\n"), "{}", String::from_utf8_lossy(&out));
    assert_eq!(tail(&f.head_object()), b"encoding ISO-8859-1\n\ned\xe9it\n");
}
