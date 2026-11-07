//! `format-patch -b`/`-w`/`--ignore-blank-lines` keep a binary change.
//!
//! A whitespace option makes `diff_flush()` decide each pair from the rendered
//! body (`diff_from_contents`), and builtin_diff() sets `o->found_changes` for
//! any binary pair whose contents differ (diff.c:3943-3989), whatever whitespace
//! is ignored. zvcs dropped every in-place edit with no added or deleted lines,
//! which took the binary file out of the diffstat and the patch.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in identical
//! repositories.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// One commit edits a binary file, re-indents a text file and adds a line to
/// another; the next only re-indents.
fn repo(label: &str, stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fp-ws-binary-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(stock, &work, &["init", "-q", "-b", "main", "."]);
    std::fs::write(work.join("bin.dat"), b"\0\x01\x02binary\xff\n").unwrap();
    std::fs::write(work.join("ws.txt"), "a b\nc\n").unwrap();
    std::fs::write(work.join("t.txt"), "one\n").unwrap();
    git(stock, &work, &["add", "."]);
    git(stock, &work, &["commit", "-q", "-m", "base"]);
    std::fs::write(work.join("bin.dat"), b"\0\x01\x02binary\xff\nmore\n").unwrap();
    std::fs::write(work.join("ws.txt"), "a  b\nc\n").unwrap();
    std::fs::write(work.join("t.txt"), "one\ntwo\n").unwrap();
    git(stock, &work, &["commit", "-q", "-am", "mixed"]);
    std::fs::write(work.join("ws.txt"), "a   b\nc  \n").unwrap();
    git(stock, &work, &["commit", "-q", "-am", "whitespace only"]);
    work
}

fn same(args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let label = args.join("");
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), stock);
    let want = git(stock, &s, args);
    let got = git(ZVCS, &z, args);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn a_binary_edit_survives_ignored_whitespace() {
    for ws in ["-b", "-w", "--ignore-space-at-eol", "--ignore-blank-lines", "--ignore-cr-at-eol"] {
        same(&["format-patch", "--no-signature", "--stdout", ws, "-2"]);
        same(&["format-patch", "--no-signature", "--stdout", ws, "--binary", "--numstat", "-2"]);
    }
    same(&["format-patch", "--no-signature", "--stdout", "-b", "--no-binary", "--stat", "-2"]);
    same(&["format-patch", "--no-signature", "--stdout", "-w", "--raw", "--name-status", "-2"]);
}
