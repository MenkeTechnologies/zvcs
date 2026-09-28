//! format-patch writes each message in `get_log_output_encoding()`.
//!
//! `pretty_print_commit()` re-codes the whole commit buffer with
//! `repo_logmsg_reencode(commit, get_log_output_encoding())` (pretty.c:2298-2320),
//! so the author the `From:` line names, the subject and the body all come out
//! in that encoding, and `add_rfc2047()` / `pp_email_subject()` label the encoded
//! words and the 8-bit `Content-Type:` with its name (pretty.c:395-436,
//! 2109-2152). The encoding is `--encoding=<name>` (revision.c:2701-2707, where
//! `none` stores the empty string), else `i18n.logOutputEncoding`, else
//! `i18n.commitEncoding`, else UTF-8 (environment.c:189-198). Notes are re-coded
//! from UTF-8 by `format_note()` (notes.c:1305-1312), the cover letter's
//! shortlog by `shortlog_add_commit()`'s `ctx.output_encoding`
//! (builtin/shortlog.c:251), while the cover letter itself stays labelled
//! `UTF-8` (builtin/log.c:1402).
//!
//! zvcs built every message from UTF-8 strings: `--encoding` was an
//! unrecognized argument, the two config keys were ignored, and a commit stored
//! in ISO-8859-1 was fatal ("author name is not valid UTF-8").
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
    /// `main` is `init` plus one commit stored in ISO-8859-1 — author `Jörg`,
    /// subject `résumé`, body `body ä` — written as a raw object so the bytes
    /// are exactly what a Latin-1 `i18n.commitEncoding` produces. It carries a
    /// UTF-8 note `nöte`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-encoding-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-q", "-m", "init"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f.ok(&["add", "f"]);
        let tree = f.ok(&["write-tree"]);
        let parent = f.ok(&["rev-parse", "HEAD"]);
        let mut raw = format!("tree {}\nparent {}\n", tree.trim(), parent.trim()).into_bytes();
        raw.extend_from_slice(b"author J\xf6rg <author@example.com> 1700000000 +0000\n");
        raw.extend_from_slice(b"committer C O Mitter <committer@example.com> 1700000000 +0000\n");
        raw.extend_from_slice(b"encoding ISO-8859-1\n\nr\xe9sum\xe9\n\nbody \xe4\n");
        let obj = f.root.join("commit-object");
        std::fs::write(&obj, raw).unwrap();
        let id = f.ok(&["hash-object", "-t", "commit", "-w", obj.to_str().unwrap()]);
        f.ok(&["update-ref", "refs/heads/main", id.trim()]);
        f.ok(&["notes", "add", "-m", "n\u{f6}te", "HEAD"]);
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        String::from_utf8(out).unwrap()
    }

    /// The patch message up to and including the `---` separator.
    fn head(&self, args: &[&str]) -> Vec<u8> {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        let end = out
            .windows(5)
            .position(|w| w == b"\n---\n")
            .expect("a three-dash separator")
            + 5;
        out[..end].to_vec()
    }
}

const FROM_LINE: &[u8] = b"From 43590490bf4a10047962bc77647866737b4b8ec1 Mon Sep 17 00:00:00 2001\n";

fn message(from: &[u8], subject: &[u8], charset: &[u8], body: &[u8]) -> Vec<u8> {
    let mut m = FROM_LINE.to_vec();
    m.extend_from_slice(b"From: ");
    m.extend_from_slice(from);
    m.extend_from_slice(b" <author@example.com>\nDate: Tue, 14 Nov 2023 22:13:20 +0000\nSubject: [PATCH] ");
    m.extend_from_slice(subject);
    m.extend_from_slice(b"\nMIME-Version: 1.0\nContent-Type: text/plain; charset=");
    m.extend_from_slice(charset);
    m.extend_from_slice(b"\nContent-Transfer-Encoding: 8bit\n\n");
    m.extend_from_slice(body);
    m.extend_from_slice(b"\n---\n");
    m
}

#[test]
fn a_latin1_commit_is_recoded_to_utf8_by_default() {
    let f = Fixture::new("default");
    assert_eq!(
        f.head(&["format-patch", "--stdout", "-1"]),
        message(
            b"=?UTF-8?q?J=C3=B6rg?=",
            b"=?UTF-8?q?r=C3=A9sum=C3=A9?=",
            b"UTF-8",
            "body \u{e4}".as_bytes(),
        )
    );
}

#[test]
fn encoding_option_labels_headers_and_recodes_the_note() {
    let f = Fixture::new("option");
    let (out, err, code) =
        f.run(&["format-patch", "--stdout", "-1", "--encoding=ISO-8859-1", "--notes"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let mut want = message(
        b"=?ISO-8859-1?q?J=F6rg?=",
        b"=?ISO-8859-1?q?r=E9sum=E9?=",
        b"ISO-8859-1",
        b"body \xe4",
    );
    want.extend_from_slice(b"\nNotes:\n    n\xf6te\n\n f | 2 +-\n");
    assert_eq!(&out[..want.len()], &want[..]);

    // The separate-slot spelling, and the value it requires.
    let separate = f.head(&["format-patch", "--stdout", "-1", "--encoding", "ISO-8859-1"]);
    assert_eq!(separate, message(b"=?ISO-8859-1?q?J=F6rg?=", b"=?ISO-8859-1?q?r=E9sum=E9?=", b"ISO-8859-1", b"body \xe4"));
    let (out, err, code) = f.run(&["format-patch", "--stdout", "-1", "--encoding"]);
    assert_eq!(
        (out.as_slice(), err.as_str(), code),
        (&b""[..], "fatal: Option '--encoding' requires a value\n", 128)
    );
}

#[test]
fn log_output_encoding_config_reaches_unencoded_headers() {
    let f = Fixture::new("config");
    assert_eq!(
        f.head(&[
            "-c",
            "i18n.logOutputEncoding=ISO-8859-1",
            "format-patch",
            "--stdout",
            "-1",
            "--no-encode-email-headers",
        ]),
        message(b"J\xf6rg", b"r\xe9sum\xe9", b"ISO-8859-1", b"body \xe4")
    );
    // `i18n.commitEncoding` is the fallback `get_commit_output_encoding()` gives.
    assert_eq!(
        f.head(&["-c", "i18n.commitEncoding=ISO-8859-1", "format-patch", "--stdout", "-1"]),
        message(b"=?ISO-8859-1?q?J=F6rg?=", b"=?ISO-8859-1?q?r=E9sum=E9?=", b"ISO-8859-1", b"body \xe4")
    );
}

#[test]
fn encoding_none_keeps_the_stored_bytes_and_an_empty_label() {
    let f = Fixture::new("none");
    assert_eq!(
        f.head(&["format-patch", "--stdout", "-1", "--encoding=none"]),
        message(b"=??q?J=F6rg?=", b"=??q?r=E9sum=E9?=", b"", b"body \xe4")
    );
}

#[test]
fn cover_letter_stays_utf8_while_its_shortlog_is_recoded() {
    let f = Fixture::new("cover");
    let (out, err, code) =
        f.run(&["format-patch", "--stdout", "-1", "--cover-letter", "--encoding=ISO-8859-1"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let mut want = FROM_LINE.to_vec();
    want.extend_from_slice(
        b"From: C O Mitter <committer@example.com>\n\
          Date: Tue, 14 Nov 2023 22:13:20 +0000\n\
          Subject: [PATCH 0/1] *** SUBJECT HERE ***\n\
          MIME-Version: 1.0\n\
          Content-Type: text/plain; charset=UTF-8\n\
          Content-Transfer-Encoding: 8bit\n\
          \n\
          *** BLURB HERE ***\n\
          \n\
          J\xf6rg (1):\n  r\xe9sum\xe9\n\n",
    );
    assert_eq!(&out[..want.len()], &want[..]);
}
