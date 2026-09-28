//! `log`/`show --pretty=email` label the mail with the output encoding.
//!
//! `pretty_print_commit()` hands `get_log_output_encoding()` to `pp_user_info()`
//! and `pp_email_subject()` (pretty.c:2298-2320), so the RFC2047 words and the
//! 8-bit `Content-Type:` name the charset the commit was re-coded into:
//! `--encoding=<name>`, `i18n.logOutputEncoding`, or the empty string
//! `--encoding=none` stores. zvcs re-coded the message but labelled it `UTF-8`
//! whatever the encoding was.
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
            .join(format!("zvcs-log-email-encoding-{tag}-{}", std::process::id()));
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

}

fn email(label: &[u8]) -> Vec<u8> {
    let mut m = b"From 43590490bf4a10047962bc77647866737b4b8ec1 Mon Sep 17 00:00:00 2001\nFrom: =?".to_vec();
    m.extend_from_slice(label);
    m.extend_from_slice(b"?q?J=F6rg?= <author@example.com>\nDate: Tue, 14 Nov 2023 22:13:20 +0000\nSubject: [PATCH] =?");
    m.extend_from_slice(label);
    m.extend_from_slice(b"?q?r=E9sum=E9?=\nMIME-Version: 1.0\nContent-Type: text/plain; charset=");
    m.extend_from_slice(label);
    m.extend_from_slice(b"\nContent-Transfer-Encoding: 8bit\n\nbody \xe4\n");
    m
}

#[test]
fn log_encoding_option_labels_the_mail() {
    let f = Fixture::new("log");
    let (out, err, code) = f.run(&["log", "--pretty=email", "-1", "--encoding=ISO-8859-1"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, email(b"ISO-8859-1"));
}

#[test]
fn show_takes_the_config_and_none_is_an_empty_label() {
    let f = Fixture::new("show");
    let (out, _, _) = f.run(&["-c", "i18n.logOutputEncoding=ISO-8859-1", "show", "--pretty=email", "-s"]);
    assert_eq!(out, email(b"ISO-8859-1"));
    let (out, _, _) = f.run(&["log", "--pretty=mboxrd", "--encoding=none", "-1"]);
    assert_eq!(out, email(b""));
}
