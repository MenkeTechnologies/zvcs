//! The long `git status` announces what is in progress before anything else.
//!
//! `wt_status_get_state()` reads the state files under `$GIT_DIR` — `rebase-apply/`
//! (an `am` session when `applying` is there, a patch-based rebase otherwise),
//! `rebase-merge/`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG` — and
//! `wt_longstatus_print_state()` prints one banner from that chain, plus the bisect
//! one on top of it, each followed by a blank line.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
    /// A `main` with two commits and a `side` that rewrote the same line, so any
    /// replay of `side` onto `main` conflicts.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-progress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", b"base\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Runs a command that is expected to stop with a conflict.
    fn git_expect_failure(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(!out.status.success(), "`git {args:?}` was supposed to stop: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn status(&self) -> String {
        let out = self.cmd(&["status"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Two branches whose tips both rewrote `f.txt`, with `main` checked out.
    fn diverged(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["checkout", "-q", "-b", "side"]);
        f.write("f.txt", b"side\n");
        f.git(&["commit", "-q", "-am", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("f.txt", b"main\n");
        f.git(&["commit", "-q", "-am", "main"]);
        f
    }
}

#[test]
fn a_stopped_cherry_pick_names_the_commit_and_its_ways_out() {
    let f = Fixture::diverged("cherry");
    f.git_expect_failure(&["cherry-pick", "side"]);
    let status = f.status();
    let head = String::from_utf8_lossy(
        &f.cmd(&["rev-parse", "--short", "side"]).output().unwrap().stdout,
    )
    .trim()
    .to_string();
    assert!(
        status.contains(&format!("You are currently cherry-picking commit {head}.\n")),
        "{status}"
    );
    assert!(status.contains("  (fix conflicts and run \"git cherry-pick --continue\")\n"), "{status}");
    assert!(status.contains("  (use \"git cherry-pick --skip\" to skip this patch)\n"), "{status}");
    assert!(
        status.contains("  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)\n"),
        "{status}"
    );
    // `CHERRY_PICK_HEAD` moves `whence` off `FROM_COMMIT`, which drops the unstage hint.
    assert!(!status.contains("to unstage"), "{status}");
}

#[test]
fn a_stopped_revert_names_the_commit_and_keeps_the_unstage_hint() {
    let f = Fixture::new("revert");
    f.write("f.txt", b"two\n");
    f.git(&["commit", "-q", "-am", "two"]);
    f.git(&["revert", "--no-commit", "HEAD"]);
    let head = String::from_utf8_lossy(
        &f.cmd(&["rev-parse", "--short", "HEAD"]).output().unwrap().stdout,
    )
    .trim()
    .to_string();
    let status = f.status();
    assert!(
        status.contains(&format!("You are currently reverting commit {head}.\n")),
        "{status}"
    );
    assert!(status.contains("  (use \"git revert --abort\" to cancel the revert operation)\n"), "{status}");
    // Only `MERGE_HEAD`/`CHERRY_PICK_HEAD` suppress it, so a revert still offers it.
    assert!(status.contains("to unstage"), "{status}");
}

#[test]
fn a_bisect_reports_the_branch_it_started_from() {
    let f = Fixture::new("bisect");
    f.write("f.txt", b"two\n");
    f.git(&["commit", "-q", "-am", "two"]);
    f.git(&["bisect", "start"]);
    f.git(&["bisect", "bad"]);
    f.git(&["bisect", "good", "HEAD~1"]);
    let status = f.status();
    assert!(
        status.contains("You are currently bisecting, started from branch 'main'.\n"),
        "{status}"
    );
    assert!(
        status.contains("  (use \"git bisect reset\" to get back to the original branch)\n"),
        "{status}"
    );
}

#[test]
fn a_stopped_am_session_is_announced() {
    let f = Fixture::new("am");
    f.write("f.txt", b"two\n");
    f.git(&["commit", "-q", "-am", "two"]);
    let patch = String::from_utf8_lossy(
        &f.cmd(&["format-patch", "-1", "--stdout"]).output().unwrap().stdout,
    )
    .into_owned();
    std::fs::write(f.root.join("p.patch"), patch.as_bytes()).unwrap();
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.write("f.txt", b"conflicting\n");
    f.git(&["commit", "-q", "-am", "conflicting"]);
    f.git_expect_failure(&["am", f.root.join("p.patch").to_str().unwrap()]);

    let status = f.status();
    assert!(status.contains("You are in the middle of an am session.\n"), "{status}");
    assert!(status.contains("  (fix conflicts and then run \"git am --continue\")\n"), "{status}");
    assert!(status.contains("  (use \"git am --skip\" to skip this patch)\n"), "{status}");
    assert!(
        status.contains("  (use \"git am --abort\" to restore the original branch)\n"),
        "{status}"
    );
}

/// The header names what the rebase is onto, and the todo list is summarized above
/// the banner.
#[test]
fn an_interactive_rebase_shows_its_todo_list_and_target() {
    let f = Fixture::new("rebase-i");
    f.write("f.txt", b"two\n");
    f.git(&["commit", "-q", "-am", "two"]);
    f.write("f.txt", b"three\n");
    f.git(&["commit", "-q", "-am", "three"]);
    let onto = String::from_utf8_lossy(
        &f.cmd(&["rev-parse", "--short", "HEAD~2"]).output().unwrap().stdout,
    )
    .trim()
    .to_string();
    // Stop at the first commit by turning its `pick` into an `edit`.
    let out = f
        .cmd(&["rebase", "-i", "HEAD~2"])
        .env(
            "GIT_SEQUENCE_EDITOR",
            "perl -i -pe 's/^pick/edit/ if $. == 1'",
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "rebase -i failed: {out:?}");

    let status = f.status();
    assert!(
        status.starts_with(&format!("interactive rebase in progress; onto {onto}\n")),
        "{status}"
    );
    assert!(status.contains("Last command done (1 command done):\n"), "{status}");
    assert!(status.contains("Next command to do (1 remaining command):\n"), "{status}");
    assert!(status.contains("  (use \"git rebase --edit-todo\" to view and edit)\n"), "{status}");
    assert!(
        status.contains(&format!("You are currently editing a commit while rebasing branch 'main' on '{onto}'.\n")),
        "{status}"
    );
    assert!(status.contains("  (use \"git commit --amend\" to amend the current commit)\n"), "{status}");
    // The todo lines carry abbreviated object ids, not full ones.
    assert!(
        !status.lines().any(|line| line.trim_start().starts_with("edit ")
            && line.split_whitespace().nth(1).is_some_and(|id| id.len() > 20)),
        "{status}"
    );
}

/// A conflicted non-interactive rebase reports the conflict, not an edit.
#[test]
fn a_conflicted_rebase_reports_the_conflict() {
    let f = Fixture::diverged("rebase-conflict");
    f.git(&["checkout", "-q", "side"]);
    f.git_expect_failure(&["rebase", "main"]);
    let status = f.status();
    assert!(status.contains("You are currently rebasing branch 'side' on "), "{status}");
    assert!(status.contains("  (fix conflicts and then run \"git rebase --continue\")\n"), "{status}");
    assert!(status.contains("  (use \"git rebase --abort\" to check out the original branch)\n"), "{status}");
}
