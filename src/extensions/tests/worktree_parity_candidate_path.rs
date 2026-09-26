//! `check_candidate_path()` and the `-f` count for `worktree add` / `worktree move`.
//!
//! `check_candidate_path()` (builtin/worktree.c:317-342) refuses a destination that
//! is still registered to a worktree whose checkout has gone missing:
//! `'<path>' is a missing but already registered worktree` unless `-f`, and
//! `'<path>' is a missing but locked worktree` unless `-f -f`. When forced it
//! deletes the stale `worktrees/<id>` so the new registration takes that id.
//! `add_worktree()` runs it before `die_if_checked_out()` (worktree.c:478-488),
//! so an occupied path is reported ahead of a branch in use elsewhere.
//! `move_worktree()` counts `-f` too: `if (force < 2) reason =
//! worktree_lock_reason(wt);` (worktree.c:1291-1292), so a single `-f` does not
//! move a locked worktree. zvcs registered a second worktree at the same path
//! (`gone1`), and moved a locked worktree on one `-f`.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-worktree-candidate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
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

    fn admin_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(self.work.join(".git/worktrees"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        ids
    }

    /// `worktree add -q --detach <path>` and then delete the checkout.
    fn orphan_registration(&self, path: &str, lock: bool) {
        let mut args = vec!["worktree", "add", "-q", "--detach"];
        if lock {
            args.push("--lock");
        }
        args.push(path);
        assert_eq!(self.run(&args).2, 0);
        std::fs::remove_dir_all(self.work.join(path)).unwrap();
    }
}

#[test]
fn add_refuses_a_missing_registered_path_until_forced() {
    let f = Fixture::new("add");
    f.orphan_registration("../gone", false);
    let (out, err, code) = f.run(&["worktree", "add", "--detach", "../gone"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "Preparing worktree (detached HEAD 699945f)\n\
             fatal: '../gone' is a missing but already registered worktree;\n\
             use 'add -f' to override, or 'prune' or 'remove' to clear\n",
            128
        )
    );
    assert_eq!(f.admin_ids(), ["gone"]);
    assert!(!f.root.join("gone").exists());

    let (out, err, code) = f.run(&["worktree", "add", "-f", "--detach", "../gone"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("HEAD is now at 699945f one\n", "Preparing worktree (detached HEAD 699945f)\n", 0)
    );
    // The stale registration was replaced, not shadowed by `gone1`.
    assert_eq!(f.admin_ids(), ["gone"]);
    assert_eq!(std::fs::read_to_string(f.root.join("gone/a")).unwrap(), "a\n");
}

#[test]
fn add_needs_two_forces_for_a_missing_locked_path() {
    let f = Fixture::new("locked");
    f.orphan_registration("../gone", true);
    let (_, err, code) = f.run(&["worktree", "add", "-f", "--detach", "../gone"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "Preparing worktree (detached HEAD 699945f)\n\
             fatal: '../gone' is a missing but locked worktree;\n\
             use 'add -f -f' to override, or 'unlock' and 'prune' or 'remove' to clear\n",
            128
        )
    );
    assert!(f.work.join(".git/worktrees/gone/locked").exists());

    let (_, _, code) = f.run(&["worktree", "add", "-f", "-f", "--detach", "../gone"]);
    assert_eq!(code, 0);
    assert_eq!(f.admin_ids(), ["gone"]);
    // The new registration is not the old, locked one.
    assert!(!f.work.join(".git/worktrees/gone/locked").exists());
}

#[test]
fn add_reports_an_occupied_path_before_a_branch_in_use() {
    let f = Fixture::new("order");
    assert_eq!(f.run(&["worktree", "add", "-q", "../w1"]).2, 0);
    // `w1` exists as a branch (checked out in ../w1) and as a non-empty path.
    let (out, err, code) = f.run(&["worktree", "add", "../w1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "Preparing worktree (checking out 'w1')\nfatal: '../w1' already exists\n", 128)
    );
}

#[test]
fn move_refuses_a_missing_registered_destination_until_forced() {
    let f = Fixture::new("move");
    assert_eq!(f.run(&["worktree", "add", "-q", "--detach", "../m1"]).2, 0);
    f.orphan_registration("../m2", false);
    let (out, err, code) = f.run(&["worktree", "move", "../m1", "../m2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: '../m2' is a missing but already registered worktree;\n\
             use 'move -f' to override, or 'prune' or 'remove' to clear\n",
            128
        )
    );
    assert!(f.root.join("m1/.git").exists());

    let (out, err, code) = f.run(&["worktree", "move", "-f", "../m1", "../m2"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.admin_ids(), ["m1"]);
    assert!(f.root.join("m2/.git").exists());
}

#[test]
fn move_needs_two_forces_for_a_locked_worktree() {
    let f = Fixture::new("movelock");
    assert_eq!(
        f.run(&["worktree", "add", "-q", "--detach", "--lock", "--reason", "why", "../m1"]).2,
        0
    );
    let (out, err, code) = f.run(&["worktree", "move", "-f", "../m1", "../m2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: cannot move a locked working tree, lock reason: why\n\
             use 'move -f -f' to override or unlock first\n",
            128
        )
    );
    assert!(f.root.join("m1/.git").exists());
    assert!(!f.root.join("m2").exists());

    let (_, err, code) = f.run(&["worktree", "move", "-f", "-f", "../m1", "../m2"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(f.root.join("m2/.git").exists());
}
