//! `hash-object` converts as if the index were empty.
//!
//! `hash_fd()` hands `index_fd()` `the_repository->index` (builtin/hash-object.c:29),
//! and nothing in hash-object ever reads it. So `read_attr_from_index()` finds no
//! `.gitattributes` there (attr.c:791-839) and `has_crlf_in_index()` no blob
//! (convert.c:224-245): a `.gitattributes` that exists only in the index applies to
//! nothing, and `core.autocrlf` normalises a file whose committed blob already holds
//! CRLF — the "safer autocrlf" guard (convert.c:536-538) never fires. zvcs read the
//! repository's index for both.
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
    work: PathBuf,
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
    /// `crlf.txt` committed with its CRLF, and a `.gitattributes` committed and then
    /// deleted from the work tree, so the index is the only place it still exists.
    fn new(tag: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-hounread-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("r");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.stock(stock, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
        std::fs::write(f.work.join(".gitattributes"), "*.auto text=auto\n").unwrap();
        f.stock(stock, &["add", "-A"]);
        f.stock(stock, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        std::fs::remove_file(f.work.join(".gitattributes")).unwrap();
        f
    }

    fn stock(&self, stock: &str, args: &[&str]) {
        let (_, err, code) = git(stock, &self.root, &self.work, args, b"");
        assert_eq!(code, 0, "fixture `git {args:?}`: {err}");
    }

    fn both(&self, stock: &str, args: &[&str], stdin: &[u8]) -> [(String, String, i32); 2] {
        [git(stock, &self.root, &self.work, args, stdin), git(BIN, &self.root, &self.work, args, stdin)]
    }
}

#[test]
fn autocrlf_converts_a_file_whose_committed_blob_has_crlf() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("crlf", stock);
    for args in [
        &["-c", "core.autocrlf=true", "hash-object", "crlf.txt"][..],
        &["-c", "core.autocrlf=input", "hash-object", "crlf.txt"][..],
        &["-c", "core.autocrlf=true", "hash-object", "--stdin-paths"][..],
    ] {
        let [s, z] = f.both(stock, args, b"crlf.txt\n");
        // `one\ntwo\n`, not the committed blob.
        assert_eq!(s.0, "814f4a422927b82f5f8a43f8fab6d3839e3983f2\n", "{args:?}");
        assert_eq!(z, s, "{args:?}");
    }
}

#[test]
fn a_gitattributes_only_in_the_index_applies_to_nothing() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new("attrs", stock);
    let [s, z] = f.both(stock, &["hash-object", "--stdin", "--path=x.auto"], b"a\r\nb\r\n");
    // `a\r\nb\r\n` hashed as it is.
    assert_eq!(s.0, "c30dea8a3641ea99b125d04d599d843712292759\n");
    assert_eq!(z, s);
}
