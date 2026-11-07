//! A backslash in a path `update-index` is given.
//!
//! `prefix_path()` normalises `.` and `..` components and nothing else: outside
//! Windows a `\` is an ordinary byte of the name. zvcs rebuilt the resolved path
//! through `to_string_lossy().replace('\\', "/")`, so `a\b` was staged as `a/b` and
//! the error for a missing `q\tx` named `q/tx`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

impl Twin {
    fn new(tag: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uibslash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for which in ["stock", "zvcs"] {
            let dir = root.join(which);
            std::fs::create_dir_all(&dir).unwrap();
            let (_, err, code) = git(stock, &root, &dir, &["init", "-q", "-b", "main", "."], b"");
            assert_eq!(code, 0, "{err}");
            std::fs::write(dir.join("a\\b"), "ab\n").unwrap();
        }
        Twin { root }
    }

    fn both(&self, stock: &str, args: &[&str], stdin: &[u8]) -> [(String, String, i32); 2] {
        [
            git(stock, &self.root, &self.root.join("stock"), args, stdin),
            git(BIN, &self.root, &self.root.join("zvcs"), args, stdin),
        ]
    }
}

#[test]
fn a_backslash_is_part_of_the_name() {
    let Some(stock) = stock_git() else { return };
    for (tag, args, stdin) in [
        ("arg", &["update-index", "--add", "--verbose", "a\\b"][..], &b""[..]),
        ("stdin", &["update-index", "--add", "--verbose", "--stdin"][..], &b"a\\b\n"[..]),
        ("nul", &["update-index", "--add", "--verbose", "-z", "--stdin"][..], &b"a\\b\0"[..]),
    ] {
        let t = Twin::new(tag, stock);
        let [s, z] = t.both(stock, args, stdin);
        assert_eq!(s, ("add 'a\\b'\n".to_owned(), String::new(), 0), "{tag}");
        assert_eq!(z, s, "{tag}");
        let [s, z] = t.both(stock, &["ls-files", "-s"], b"");
        assert_eq!(z, s, "{tag}");
    }
    let t = Twin::new("missing", stock);
    let [s, z] = t.both(stock, &["update-index", "-z", "--stdin"], b"\"q\\tx\"\n");
    assert_eq!(s.2, 128, "{s:?}");
    assert_eq!(z, s);
}
