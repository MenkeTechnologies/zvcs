//! `fast-import` of an ident with an empty name, `<email> <when>`.
//!
//! `parse_ident()` opens with `if (*buf == '<') --buf;`
//! (builtin/fast-import.c:2007-2009): `buf` points just past the
//! `committer `/`author `/`tagger ` keyword, so stepping back takes that
//! keyword's space into the ident. The object therefore stores
//! `committer  <a@x> …` with two spaces — which changes its id — and each
//! diagnostic quotes the ident from that space. zvcs stored one space.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
//! Stock also prints `fast-import: dumping crash report to …` after a fatal,
//! which this port does not write (see the module header of fast_import.rs),
//! so the fatal case checks the `fatal:` line and the exit code.

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fast-import-empty-name-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."], "");
        f
    }

    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn commit_body(&self, rev: &str) -> String {
        self.run(&["cat-file", "commit", rev], "").0
    }
}

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";


#[test]
fn an_empty_name_keeps_the_keyword_space_in_commits_and_tags() {
    let f = Fixture::new("store");
    let stream = "commit refs/heads/v\n\
                  committer <a@x> 1700000000 +0000\n\
                  data 0\n\n\
                  tag t\nfrom refs/heads/v\n\
                  tagger <a@x> 1700000000 +0000\n\
                  data 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet"], stream);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        f.commit_body("v"),
        format!(
            "tree {EMPTY_TREE}\n\
             author  <a@x> 1700000000 +0000\n\
             committer  <a@x> 1700000000 +0000\n\n"
        )
    );
    assert_eq!(f.run(&["rev-parse", "v"], "").0, "5ec66643941810288cfccd82bd74c7695fee2629\n");
    assert_eq!(
        f.run(&["cat-file", "tag", "t"], "").0,
        "object 5ec66643941810288cfccd82bd74c7695fee2629\ntype commit\ntag t\n\
         tagger  <a@x> 1700000000 +0000\n\n"
    );
}

#[test]
fn an_empty_author_name_beside_a_named_committer() {
    let f = Fixture::new("author");
    let stream = "commit refs/heads/q\n\
                  author <a@x> 1700000000 +0000\n\
                  committer C <c@x> 1700000000 +0000\n\
                  data 0\n\n";
    let (_, err, code) = f.run(&["fast-import", "--quiet"], stream);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(
        f.commit_body("q"),
        format!(
            "tree {EMPTY_TREE}\n\
             author  <a@x> 1700000000 +0000\n\
             committer C <c@x> 1700000000 +0000\n\n"
        )
    );
}

#[test]
fn diagnostics_quote_the_ident_from_the_keyword_space() {
    let f = Fixture::new("diag");
    for (line, want) in [
        (
            "committer <a@x 1700000000 +0000",
            "fatal: missing > in ident string:  <a@x 1700000000 +0000",
        ),
        (
            "committer <a@x> 17000x +0000",
            "fatal: invalid raw date \"17000x +0000\" in ident:  <a@x> 17000x +0000",
        ),
    ] {
        let stream = format!("commit refs/heads/x\n{line}\ndata 0\n\n");
        let (out, err, code) = f.run(&["fast-import", "--quiet"], &stream);
        assert_eq!((out.as_str(), code), ("", 128), "{line}");
        assert_eq!(err.lines().next(), Some(want), "{line}");
    }
}
