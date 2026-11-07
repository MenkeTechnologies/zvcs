//! Extra blanks between an ident's `>` and its timestamp.
//!
//! `fsck_ident()` requires one space after the email and then, "since we have
//! traditionally allowed extra whitespace", skips any further spaces and tabs before
//! it looks for a digit (fsck.c:915-931). zvcs took the first character after that
//! one space as the date, so `mktag` refused a tagger line stock accepts with
//! `badDate`, and `fsck` reported the same commit stock finds clean.
//!
//! `fsck_tag_standalone()` does not act on the ident's verdict where it gets it
//! (fsck.c:1094-1095): it skips a `gpgsig` header (:1097-1112) and still checks for
//! an extra header, whose `ret = report(…)` replaces that verdict (:1114-1125).
//! zvcs stopped at the first bad tagger line and called a `gpgsig` header extra.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, home: &Path, dir: &Path, args: &[&str], stdin: &[u8]) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

impl Fixture {
    /// Two repositories, one per binary, each with the same commit.
    fn new(tag: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-mktagws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for which in ["stock", "zvcs"] {
            let dir = root.join(which);
            std::fs::create_dir_all(&dir).unwrap();
            for args in [
                &["init", "-q", "-b", "main", "."][..],
                &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"][..],
            ] {
                let (_, err, code) = git(stock, &root, &dir, args, b"");
                assert_eq!(code, 0, "fixture `git {args:?}`: {err}");
            }
        }
        Fixture { root }
    }

    fn both(&self, stock: &str, args: &[&str], stdin: &[u8]) -> [(String, String, i32); 2] {
        [
            git(stock, &self.root, &self.root.join("stock"), args, stdin),
            git(BIN, &self.root, &self.root.join("zvcs"), args, stdin),
        ]
    }
}

#[test]
fn mktag_accepts_blanks_before_the_timestamp() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("mktag", stock);
    let [head, _] = f.both(stock, &["rev-parse", "HEAD"], b"");
    let head = head.0.trim();
    for (gap, strict) in [("  ", "--strict"), (" \t ", "--strict"), ("  ", "--no-strict")] {
        let tag = format!("object {head}\ntype commit\ntag v1\ntagger T <t@example.com>{gap}1700000000 +0000\n\nmsg\n");
        let [s, z] = f.both(stock, &["mktag", strict], tag.as_bytes());
        assert_eq!(s.2, 0, "{gap:?}: {s:?}");
        assert_eq!(z, s, "{gap:?}");
    }
    // A newline is not such a blank: the date is bad, and the line after the
    // tagger is an extra header, reported too. Its verdict replaces the ident's, so
    // without --strict, where it is a warning, the tag is written.
    let tag = format!("object {head}\ntype commit\ntag v1\ntagger T <t@example.com> \n1700000000 +0000\n\nmsg\n");
    for strict in ["--strict", "--no-strict"] {
        let [s, z] = f.both(stock, &["mktag", strict], tag.as_bytes());
        assert!(s.1.contains("badDate") && s.1.contains("extraHeaderEntry"), "{s:?}");
        assert_eq!(z, s, "{strict}");
    }
    // A signature header and its continuation lines are no extra header.
    let tag = format!(
        "object {head}\ntype commit\ntag v1\ntagger T <t@example.com> 1700000000 +0000\ngpgsig abc\n more\n\nmsg\n"
    );
    let [s, z] = f.both(stock, &["mktag"], tag.as_bytes());
    assert_eq!(s.2, 0, "{s:?}");
    assert_eq!(z, s);
}

#[test]
fn fsck_finds_such_a_commit_clean() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("fsck", stock);
    let body = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                author A <a@example.com>   1700000000 +0000\n\
                committer C <c@example.com> \t1700000000 +0000\n\nm\n";
    let [s, z] = f.both(stock, &["hash-object", "-t", "commit", "-w", "--stdin"], body.as_bytes());
    assert_eq!(s.2, 0, "{s:?}");
    assert_eq!(z, s);
    let id = s.0.trim().to_owned();
    let [s, z] = f.both(stock, &["fsck", "--no-dangling", &id], b"");
    assert_eq!(z, s);
}
