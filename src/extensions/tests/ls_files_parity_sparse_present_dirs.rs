//! `repo_read_index()` runs `clear_skip_worktree_from_present_files()` after every read
//! (sparse-index.c:643-685): a sparse directory whose path exists in the working tree makes
//! git expand the index, and a `SKIP_WORKTREE` entry whose file is on disk loses the bit.
//! `sparse.expectFilesOutsideOfPatterns` switches the scan off.
//!
//! `ls-files --sparse` shows the difference: with `index.sparse=true` it lists
//! `outside/` collapsed only while nothing named `outside` exists on disk.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git")
}

/// A cone-mode sparse repository whose only cone is `inside/`, built by stock git.
fn fixture(tag: &str, n: usize) -> PathBuf {
    let stock = stock_git().expect("stock git");
    let dir = std::env::temp_dir().join(format!("zvcs-lsf-sparse-{tag}-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["inside/keep.txt", "outside/drop.txt", "outside/nested/deep.txt", "root.txt", "src/lib.rs"] {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x\n").unwrap();
    }
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-qm", "seed"],
        &["sparse-checkout", "init", "--cone"],
        &["sparse-checkout", "set", "inside"],
    ] {
        let o = run(stock, &dir, args);
        assert!(o.status.success(), "stock {args:?}: {o:?}");
    }
    dir
}

/// Run `args` with stock and zvcs on identical fixtures and require identical output.
fn same(case: &str, prepare: impl Fn(&Path), args: &[&str]) -> String {
    let stock = stock_git().expect("stock git");
    let (a, b) = (fixture(case, 0), fixture(case, 1));
    prepare(&a);
    prepare(&b);
    let want = run(stock, &a, args);
    let got = run(BIN, &b, args);
    assert_eq!(String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&want.stdout), "{case} {args:?} stdout");
    assert_eq!(String::from_utf8_lossy(&got.stderr), String::from_utf8_lossy(&want.stderr), "{case} {args:?} stderr");
    assert_eq!(got.status.code(), want.status.code(), "{case} {args:?} exit");
    let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    String::from_utf8_lossy(&want.stdout).into_owned()
}

const SPARSE: [&str; 4] = ["-c", "index.sparse=true", "ls-files", "--sparse"];

#[test]
fn no_directory_on_disk_stays_collapsed() {
    if stock_git().is_none() {
        return;
    }
    let out = same("collapsed", |_| {}, &SPARSE);
    assert!(out.contains("outside/\n"), "the fixture must collapse: {out}");
}

#[test]
fn an_existing_sparse_directory_expands_the_index() {
    if stock_git().is_none() {
        return;
    }
    let mkdir = |p: &Path| std::fs::create_dir_all(p.join("outside")).unwrap();
    let out = same("emptydir", mkdir, &SPARSE);
    assert!(out.contains("outside/drop.txt\n"), "{out}");
    same("emptydir-t", mkdir, &["-c", "index.sparse=true", "ls-files", "-t"]);
    let stray = |p: &Path| {
        std::fs::create_dir_all(p.join("outside")).unwrap();
        std::fs::write(p.join("outside/stray.txt"), "u\n").unwrap();
    };
    same("stray", stray, &SPARSE);
}

#[test]
fn present_file_loses_skip_worktree() {
    if stock_git().is_none() {
        return;
    }
    let present = |p: &Path| {
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/lib.rs"), "back\n").unwrap();
    };
    let out = same("present-t", present, &["ls-files", "-t"]);
    assert!(out.contains("H src/lib.rs\n"), "{out}");
}

#[test]
fn expect_files_outside_of_patterns_disables_the_scan() {
    if stock_git().is_none() {
        return;
    }
    let mkdir = |p: &Path| std::fs::create_dir_all(p.join("outside")).unwrap();
    let out = same(
        "expect",
        mkdir,
        &["-c", "index.sparse=true", "-c", "sparse.expectFilesOutsideOfPatterns=true", "ls-files", "--sparse"],
    );
    assert!(out.contains("outside/\n"), "{out}");
}
