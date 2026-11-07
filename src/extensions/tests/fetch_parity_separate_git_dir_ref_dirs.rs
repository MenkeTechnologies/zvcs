//! The empty `refs/<kind>/` directories of a git directory that lies outside its
//! work tree — `clone --separate-git-dir`, a submodule's `.git/modules/<name>` —
//! after `fetch`, `branch -d`, `tag -d` and `update-ref -d`.
//!
//! A ref deletion takes the directories between `refs/<kind>/` and the ref that it
//! leaves empty and stops there (`try_remove_empty_parents()` under
//! `REMOVE_EMPTY_PARENTS_REF`, refs/files-backend.c:3430-3471), its reflog's the
//! same way below `logs/refs/<kind>/` (`REMOVE_EMPTY_PARENTS_REFLOG`, :3402-3410),
//! and a lock file's rollback removes no leading directory at all. So `refs/tags/`,
//! empty from the clone on, survives a fetch and a `tag -d`; `refs/heads/nested/`
//! goes with `nested/one`; `refs/foo/` and `logs/refs/foo/` stay when `refs/foo/a/b`
//! goes. zvcs's ref locks removed every empty directory up to the git directory
//! when they were released, which only showed when that path was absolute.
//!
//! Each case runs stock git and zvcs on identical fixtures (built with stock git)
//! and compares the exit status and the directory tree under the git directory's
//! `refs/`.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
#[path = "support/stock_git.rs"]
mod stock_git;

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> i32 {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git")
        .status
        .code()
        .expect("no signal")
}

/// Every directory and file under `dir`, relative to it, sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries {
            let path = entry.unwrap().path();
            out.push(path.strip_prefix(base).unwrap().display().to_string());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Builds `up` (one commit on `main`, a second branch `n/one`) with stock git, then
/// runs `steps` — `(binary is the one under test, directory, args)` — and returns
/// their exit codes and the trees under `refs_of`'s `refs/` and `logs/`.
fn outcome(
    stock: &str,
    bin: &str,
    tag: &str,
    setup: &[(&str, &[&str])],
    steps: &[(&str, &[&str])],
    refs_of: &str,
) -> (Vec<i32>, Vec<String>) {
    let base = std::env::temp_dir().join(format!("zvcs-fetch-sep-refs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home")).unwrap();
    let fx = Fixture { root: base.canonicalize().unwrap() };
    let home = fx.root.join("home");
    for args in [
        &["init", "-q", "-b", "main", "up"][..],
        &["-C", "up", "commit", "-q", "--allow-empty", "-m", "a"],
        &["-C", "up", "branch", "n/one"],
    ] {
        assert_eq!(run(stock, &fx.root, &home, args), 0, "{args:?}");
    }
    for (dir, args) in setup {
        assert_eq!(run(stock, &fx.root.join(dir), &home, args), 0, "{args:?}");
    }
    // The upstream moves on, so the fetch below has refs to write.
    assert_eq!(run(stock, &fx.root, &home, &["-C", "up", "commit", "-q", "--allow-empty", "-m", "b"]), 0);
    let codes = steps.iter().map(|(dir, args)| run(bin, &fx.root.join(dir), &home, args)).collect();
    let git_dir = fx.root.join(refs_of);
    let mut trees = tree(&git_dir.join("refs"));
    trees.extend(tree(&git_dir.join("logs")).into_iter().map(|p| format!("logs/{p}")));
    (codes, trees)
}

fn compare(case: &str, setup: &[(&str, &[&str])], steps: &[(&str, &[&str])], refs_of: &str) {
    let Some(stock) = stock_git::stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let want = outcome(stock, stock, &format!("{case}-stock"), setup, steps, refs_of);
    let got = outcome(stock, ZVCS, &format!("{case}-zvcs"), setup, steps, refs_of);
    assert_eq!(got, want, "{case}");
}

#[test]
fn fetch_into_a_separate_git_dir_keeps_the_empty_tags_directory() {
    let setup: &[(&str, &[&str])] = &[(".", &["clone", "-q", "--separate-git-dir=gb", "up", "b"])];
    compare("sep-fetch", setup, &[("b", &["fetch", "-q"])], "gb");
    compare("sep-fetch-prune", setup, &[("b", &["fetch", "-q", "--prune"])], "gb");
}

#[test]
fn fetch_inside_a_submodule_keeps_the_empty_tags_directory() {
    let setup: &[(&str, &[&str])] = &[
        (".", &["init", "-q", "-b", "main", "e"]),
        ("e", &["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../up", "sub"]),
    ];
    compare("submodule-fetch", setup, &[("e/sub", &["fetch", "-q"])], "e/.git/modules/sub");
}

#[test]
fn deletions_in_a_separate_git_dir_prune_only_below_the_kind() {
    let setup: &[(&str, &[&str])] = &[
        (".", &["clone", "-q", "--separate-git-dir=gb", "up", "b"]),
        ("b", &["branch", "nested/one"]),
        ("b", &["tag", "t1"]),
        ("b", &["update-ref", "--create-reflog", "refs/foo/a/b", "HEAD"]),
    ];
    compare(
        "sep-deletes",
        setup,
        &[
            ("b", &["branch", "-q", "-d", "nested/one"]),
            ("b", &["tag", "-d", "t1"]),
            ("b", &["update-ref", "-d", "refs/remotes/origin/n/one"]),
            ("b", &["update-ref", "-d", "refs/foo/a/b"]),
        ],
        "gb",
    );
}

/// A fetch whose transaction locks a ref it leaves alone (`n/one`, packed by the
/// clone) releases the lock through `files_transaction_cleanup()`, whose
/// `try_remove_empty_parents()` (refs/files-backend.c:2919-2929) takes the
/// `refs/remotes/origin/n/` the lock created. zvcs compared a relative git
/// directory with the absolute lock path and left it behind.
#[test]
fn a_released_lock_prunes_below_the_kind_in_a_relative_git_dir() {
    let setup: &[(&str, &[&str])] = &[(".", &["clone", "-q", "up", "c"])];
    compare("in-tree-fetch", setup, &[("c", &["fetch", "-q"])], "c/.git");
}

/// A `mustexist` lock on a missing ref fails inside `lock_raw_ref()`, whose
/// `error_return:` unlocks without pruning (refs/files-backend.c:885-893, 993-995),
/// so `refs/heads/q/` and `refs/heads/q2/` stay, for an update and a deletion alike.
#[test]
fn a_failed_lock_keeps_the_directories_it_created() {
    let setup: &[(&str, &[&str])] = &[(".", &["clone", "-q", "up", "c"])];
    compare(
        "failed-locks",
        setup,
        &[
            ("c", &["update-ref", "refs/heads/q/r", "HEAD", "0000000000000000000000000000000000000001"]),
            ("c", &["update-ref", "-d", "refs/heads/q2/r", "0000000000000000000000000000000000000001"]),
        ],
        "c/.git",
    );
}
