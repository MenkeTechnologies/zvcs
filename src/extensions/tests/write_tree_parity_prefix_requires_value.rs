//! `write-tree --prefix` with nothing after it.
//!
//! `get_arg()` reports `option \`prefix' requires a value` with a plain `error()`
//! (parse-options.c:60), which is `PARSE_OPT_ERROR`, and `parse_options()` turns that
//! into `exit(129)` without the usage block (parse-options.c:1210-1211) — the usage
//! is only for an unknown option. zvcs printed the usage after the error.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn a_missing_prefix_value_is_an_error_without_usage() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-wtprefix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let f = Fixture { root };
    let (_, err, code) = git(stock, &f.root, &f.root, &["init", "-q", "-b", "main", "."]);
    assert_eq!(code, 0, "{err}");
    for args in [&["write-tree", "--prefix"][..], &["write-tree", "--missing-ok", "--prefix"][..]] {
        let s = git(stock, &f.root, &f.root, args);
        assert_eq!(s, (String::new(), "error: option `prefix' requires a value\n".to_owned(), 129));
        assert_eq!(git(BIN, &f.root, &f.root, args), s, "{args:?}");
    }
}
