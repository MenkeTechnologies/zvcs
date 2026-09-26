//! `switch -m -c <new> <start>` labels the stash re-apply with `<start>`.
//!
//! When the two-way merge refuses a dirty path under `-m`, `switch_branches()`
//! stashes the local changes as `autostash while switching to '<name>'` and
//! re-applies them with `new_branch_info->name` as the `ours` label
//! (builtin/checkout.c:1216-1242). For `-c`/`-C` that name is the start-point
//! the caller typed — `HEAD~1` — not the branch being created. zvcs used the
//! new branch's name, so the conflict read `<<<<<<< b1` and the stash entry
//! `autostash while switching to 'b1'`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
    /// `file` is `base` then `second`; the worktree then rewrites it to `dirty`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-switch-merge-label-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("file"), "second\n").unwrap();
        f.run(&["commit", "-q", "-am", "second"]);
        std::fs::write(f.work.join("file"), "dirty\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.work.join(path)).unwrap()
    }
}

#[test]
fn create_labels_the_conflict_and_stash_with_the_start_point() {
    let f = Fixture::new("create");
    let (out, _, code) = f.run(&["switch", "-m", "-c", "b1", "HEAD~1"]);
    assert_eq!(code, 0);
    assert_eq!(out, "The following paths have local changes:\nM\tfile\n");
    assert_eq!(f.read("file"), "<<<<<<< HEAD~1\nbase\n=======\ndirty\n>>>>>>> local\n");
    assert_eq!(
        f.run(&["stash", "list"]).0,
        "stash@{0}: autostash while switching to 'HEAD~1'\n"
    );
    assert_eq!(f.run(&["symbolic-ref", "--short", "HEAD"]).0, "b1\n");
}

#[test]
fn diff3_and_force_create_use_the_same_label() {
    let f = Fixture::new("diff3");
    let (_, _, code) = f.run(&["switch", "--conflict=diff3", "-C", "b2", "main~1"]);
    assert_eq!(code, 0);
    assert_eq!(
        f.read("file"),
        "<<<<<<< main~1\nbase\n||||||| main\nsecond\n=======\ndirty\n>>>>>>> local\n"
    );
    assert_eq!(
        f.run(&["stash", "list"]).0,
        "stash@{0}: autostash while switching to 'main~1'\n"
    );
}
