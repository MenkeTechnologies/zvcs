//! Staged changes outside the current directory, seen from a subdirectory.
//!
//! git's tree-vs-index comparisons with an empty pathspec cover the whole
//! repository wherever the command runs: `git diff --cached` (`run_diff_index()`
//! over an empty `revs->prune_data`) and `repo_index_has_changes()`
//! (read-cache.c:2518-2544), which merge-ort's `merge_start()`, `stash create`'s
//! `check_changes_tracked_files()` and `describe --dirty` lean on.
//!
//! gitoxide's `tree_index_status()` given no pathspec built one with
//! `empty_patterns_match_prefix = true`, which narrows an empty pattern list to the
//! cwd. From `a/`, zvcs's `diff --cached` showed nothing staged under `b/`,
//! `stash create` printed nothing, `describe --dirty` dropped `-dirty`, and
//! `merge` went ahead over the staged changes instead of refusing.
//!
//! Expectations measured from stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    /// `main` changed `a/x`, `side` changed `t`; on `main`, `b/y` is modified and
    /// `b/new` added in the index only.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cached-cwd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("a/x", "1\n");
        f.write("b/y", "1\n");
        f.write("t", "1\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.git(&["tag", "v1"]);
        f.git(&["checkout", "-q", "-b", "side"]);
        f.write("t", "2\n");
        f.git(&["commit", "-q", "-am", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("a/x", "3\n");
        f.git(&["commit", "-q", "-am", "main"]);
        f.write("b/y", "staged\n");
        f.write("b/new", "n\n");
        f.git(&["add", "b/y", "b/new"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn run_in(&self, dir: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.root.join(dir))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = self.run_in("", args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// stdout of a command run from `a/`, which must exit 0 with no stderr.
    fn from_a(&self, args: &[&str]) -> String {
        let out = self.run_in("a", args);
        assert!(out.status.success() && out.stderr.is_empty(), "`git {args:?}` from a/: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn diff_cached_from_a_subdirectory_shows_the_whole_index() {
    let f = Fixture::new("diff");
    assert_eq!(f.from_a(&["diff", "--cached", "--name-status"]), "A\tb/new\nM\tb/y\n");
    assert_eq!(
        f.from_a(&["diff", "--cached", "--stat", "HEAD"]),
        " b/new | 1 +\n b/y   | 2 +-\n 2 files changed, 2 insertions(+), 1 deletion(-)\n"
    );
    assert_eq!(f.from_a(&["diff-index", "--cached", "--name-only", "HEAD"]), "b/new\nb/y\n");
}

#[test]
fn describe_dirty_and_stash_create_see_changes_outside_the_cwd() {
    let f = Fixture::new("dirty");
    assert_eq!(f.from_a(&["describe", "--tags", "--dirty"]), "v1-1-g10545bd-dirty\n");
    let stash = f.from_a(&["stash", "create"]);
    assert_eq!(stash.trim_end().len(), 40, "stash create made no commit: {stash:?}");
    assert_eq!(
        f.from_a(&["diff", "--name-only", &format!("{}^1", stash.trim_end()), stash.trim_end()]),
        "b/new\nb/y\n",
        "the stash's worktree commit holds the staged changes"
    );
}

#[test]
fn merge_from_a_subdirectory_refuses_staged_changes_elsewhere() {
    let f = Fixture::new("merge");
    let out = f.run_in("a", &["merge", "side"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: Your local changes to the following files would be overwritten by merge:\n  \
         b/new b/y\n\
         Merge with strategy ort failed.\n"
    );
    assert_eq!(f.from_a(&["status", "--porcelain"]), "A  b/new\nM  b/y\n");
}
