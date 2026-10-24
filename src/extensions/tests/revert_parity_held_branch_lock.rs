//! `revert` while another process holds the checked-out branch's lock.
//!
//! The merge lands in the index first; only then does `try_to_commit()` move
//! `HEAD`, and a failed `update_head_with_reflog()` is
//! `res = error("%s", err.buf)` (sequencer.c:1693-1696), folded to `res = !!res`
//! by `do_pick_commit()` (sequencer.c:2570). So stock prints the lock message as
//! an `error:`, exits 1 without `fatal: revert failed`, and leaves the merged
//! index and `MERGE_MSG` behind. zvcs handed the lock failure back to the
//! dispatcher as contention, which re-ran the revert against the index the first
//! attempt had already written and refused it as `your local changes would be
//! overwritten by revert.` at 128.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// Two commits on `main`, the second adding a line the revert takes out.
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-revert-held-branch-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("lib.rs"), "one\n").unwrap();
        f.run(&["add", "lib.rs"]);
        f.run(&["commit", "-q", "-m", "init"]);
        std::fs::write(f.work.join("lib.rs"), "one\ntwo\n").unwrap();
        f.run(&["commit", "-q", "-am", "add two"]);
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
}

#[test]
fn revert_under_a_held_branch_lock_is_the_sequencers_error_at_one() {
    let f = Fixture::new();
    let head = f.run(&["rev-parse", "HEAD"]).0;
    let lock = f.work.join(".git/refs/heads/main.lock");
    std::fs::write(&lock, "").unwrap();

    let (out, err, code) = f.run(&["revert", "--no-edit", "HEAD"]);
    let want = format!(
        "error: cannot lock ref 'HEAD': Unable to create '{}': File exists.\n\n\
         Another git process seems to be running in this repository, or the lock file may be stale\n",
        lock.display()
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 1));

    // The branch did not move, the merged index stays staged, and the message
    // `do_commit()` only unlinks on success is still there.
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0, head);
    assert_eq!(f.run(&["status", "--porcelain"]).0, "M  lib.rs\n");
    assert!(f.work.join(".git/MERGE_MSG").exists());
}
