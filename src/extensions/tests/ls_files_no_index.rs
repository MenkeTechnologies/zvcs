//! A repository that has never staged anything has no index file, and every
//! `ls-files` mode still has to work.
//!
//! `do_read_index()` (read-cache.c:2216-2224) only dies on an `open()` failure
//! when the caller demanded the file. `ls-files` reaches it through
//! `read_index_from()` with `must_exist == 0`, so `ENOENT` yields an
//! initialized-but-empty index: the cached list prints nothing and exits 0, while
//! `-o` still walks the worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

/// A freshly initialized repository with two untracked files and no index.
fn scratch() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-lsfnoidx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo/sub")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    let init = run(&repo, &home, &["init", "-q", "-b", "main"]);
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::write(repo.join("sub/b.txt"), "b\n").unwrap();
    (root, repo, home)
}

#[test]
fn ls_files_treats_a_missing_index_as_empty() {
    let (root, repo, home) = scratch();
    assert!(!repo.join(".git/index").exists(), "the fixture must have no index");

    // The index-reading modes print nothing and exit 0 — not an IO error.
    for args in [
        &["ls-files"][..],
        &["ls-files", "-c"][..],
        &["ls-files", "-s"][..],
        &["ls-files", "-d"][..],
        &["ls-files", "-m"][..],
        &["ls-files", "-u"][..],
        &["ls-files", "-t"][..],
        &["ls-files", "--resolve-undo"][..],
    ] {
        let o = run(&repo, &home, args);
        assert_eq!(o.status.code(), Some(0), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "", "git {args:?} stdout");
        assert_eq!(String::from_utf8_lossy(&o.stderr), "", "git {args:?} stderr");
    }

    // `-o` walks the worktree, which the missing index does not stop.
    let o = run(&repo, &home, &["ls-files", "-o"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "a.txt\nsub/b.txt\n");

    // `--error-unmatch` still reports an unmatched pathspec against the empty
    // index, with git's message and exit 1.
    let o = run(&repo, &home, &["ls-files", "--error-unmatch", "a.txt"]);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "error: pathspec 'a.txt' did not match any file(s) known to git\n\
         Did you forget to 'git add'?\n"
    );

    // Reading an absent index must not create one.
    assert!(!repo.join(".git/index").exists(), "ls-files wrote an index");

    let _ = std::fs::remove_dir_all(root);
}
