//! `fetch` refuses a destination some worktree has checked out, by name.
//!
//! `do_fetch()` runs `check_not_current_branch(ref_map)` right after
//! `get_ref_map()` unless `--update-head-ok` was given (builtin/fetch.c:1970-1973):
//! every mapping whose destination is under `refs/heads/` is looked up with
//! `branch_checked_out()`, and the first hit is a `die()` before anything is
//! fetched (:1495-1505). The test is on the name alone, so a branch `HEAD`
//! names that has no commit yet counts, and so does the `HEAD` of a linked
//! worktree. zvcs relied on gitoxide's own guard, which only fires for a ref
//! that exists and would change — `git fetch <url> main:main` in a fresh
//! `git init` wrote the branch and filled the object store.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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

impl Fixture {
    /// `src` with one commit, and `w`, an empty repository whose `main` is unborn.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-checked-out-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.run("src", &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("src/a"), "a\n").unwrap();
        f.run("src", &["add", "a"]);
        f.run("src", &["commit", "-q", "-m", "a"]);
        f.run(".", &["init", "-q", "-b", "main", "w"]);
        f
    }

    fn loose_and_packed(&self) -> usize {
        std::fs::read_dir(self.root.join("w/.git/objects")).unwrap().count()
    }

    fn run(&self, dir: &str, args: &[&str]) -> (String, String, i32) {
        let cwd: &Path = &self.root.join(dir);
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
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
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn an_unborn_checked_out_branch_is_refused_before_any_object_arrives() {
    let f = Fixture::new("unborn");
    let before = f.loose_and_packed();
    let want = format!(
        "fatal: refusing to fetch into branch 'refs/heads/main' checked out at '{}'\n",
        f.root.join("w").display()
    );
    for spec in ["main:main", "+refs/heads/*:refs/heads/*"] {
        let (out, err, code) = f.run("w", &["fetch", "../src", spec]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{spec}");
    }
    assert_eq!(f.loose_and_packed(), before);
    // `truncate_fetch_head()` ran first (builtin/fetch.c:1507-1516), so the file is
    // there, and empty.
    assert_eq!(std::fs::read(f.root.join("w/.git/FETCH_HEAD")).unwrap(), b"");
    assert_eq!(f.run("w", &["rev-parse", "-q", "--verify", "main"]).2, 1);

    // `-u` lifts it, and the fetch writes the branch.
    let (_, _, code) = f.run("w", &["fetch", "-q", "-u", "../src", "main:main"]);
    assert_eq!(code, 0);
    assert_eq!(f.run("w", &["rev-parse", "main"]).0, f.run("src", &["rev-parse", "main"]).0);
}

#[test]
fn a_linked_worktree_orphan_branch_is_refused_too() {
    let f = Fixture::new("worktree");
    f.run("w", &["worktree", "add", "-q", "--orphan", "-b", "other", "../wt"]);
    let want = format!(
        "fatal: refusing to fetch into branch 'refs/heads/other' checked out at '{}'\n",
        f.root.join("wt").display()
    );
    for dir in ["w", "wt"] {
        let (out, err, code) = f.run(dir, &["fetch", "../src", "main:other"]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{dir}");
    }
}
