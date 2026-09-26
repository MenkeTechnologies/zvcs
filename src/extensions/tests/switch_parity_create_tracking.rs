//! `switch -c`/`-C` sets up tracking through `create_branch()`.
//!
//! `update_refs_for_switch()` creates the new branch with `create_branch()`
//! (builtin/checkout.c:1300-1308) *after* `merge_working_tree()` has moved the
//! index and worktree. That resolves the start-point again with
//! `dwim_branch_start()` (branch.c:539-594) — dying on an ambiguous name, and
//! under `--track` on a start-point that is no branch — and then hands
//! `real_ref` to `setup_tracking()` (branch.c:252-351), whose `inherit_tracking()`
//! warns when the start branch has no upstream and copies every `merge` line
//! when it does. `opts->track` falls back to `branch.autoSetupMerge` only when no
//! `--track`/`--no-track` was given (builtin/checkout.c:1702-1703).
//!
//! zvcs had its own upstream guesser: `--track HEAD~1` created the branch, the
//! inherit warnings were never printed, only the last `merge` line was copied,
//! `branch.autoSetupMerge=inherit` overrode an explicit `--track=direct`, and the
//! ambiguity die came before the worktree moved.
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
    /// `file` is `base` then `second` on `main`; `origin/gx` points at the first
    /// commit and `origin` maps `refs/heads/*` onto `refs/remotes/origin/*`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-switch-create-tracking-{tag}-{}", std::process::id()));
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
        f.run(&["update-ref", "refs/remotes/origin/gx", "HEAD~1"]);
        f.run(&["config", "remote.origin.url", "/nowhere"]);
        f.run(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
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

    fn branch_config(&self, name: &str) -> String {
        self.run(&["config", "--get-regexp", &format!("^branch\\.{name}\\.")]).0
    }

    /// What a die after `merge_working_tree()` leaves: the worktree and index at
    /// the start-point, `HEAD` still on `main`, and no new branch.
    fn assert_moved_but_not_created(&self) {
        assert_eq!(std::fs::read_to_string(self.work.join("file")).unwrap(), "base\n");
        assert_eq!(self.run(&["status", "--porcelain"]).0, "M  file\n");
        assert_eq!(self.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/main\n");
        assert_eq!(self.run(&["rev-parse", "-q", "--verify", "refs/heads/b"]).2, 1);
    }
}

#[test]
fn explicit_track_from_a_commit_dies_after_the_worktree_moved() {
    let f = Fixture::new("notbranch");
    let got = f.run(&["switch", "-c", "b", "--track", "HEAD~1"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "fatal: cannot set up tracking information; starting point 'HEAD~1' is not a branch\n",
            128
        )
    );
    f.assert_moved_but_not_created();
}

#[test]
fn an_ambiguous_start_point_dies_after_the_worktree_moved() {
    let f = Fixture::new("ambiguous");
    f.run(&["branch", "amb", "HEAD~1"]);
    f.run(&["tag", "amb", "HEAD~1"]);
    let got = f.run(&["switch", "-c", "b", "amb"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "warning: refname 'amb' is ambiguous.\n\
             warning: refname 'amb' is ambiguous.\n\
             fatal: ambiguous object name: 'amb'\n",
            128
        )
    );
    f.assert_moved_but_not_created();
}

#[test]
fn inherit_warns_when_the_start_branch_has_no_upstream() {
    let f = Fixture::new("inheritwarn");
    let got = f.run(&["switch", "-c", "b", "--track=inherit", "main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "warning: asked to inherit tracking from 'main', but no remote is set\n\
             Switched to a new branch 'b'\n",
            0
        )
    );
    // Only `refs/heads/` is stripped for the message, and a remote-tracking
    // start has no `branch.*` of its own, so nothing is recorded there either.
    let f = Fixture::new("inheritremote");
    let got = f.run(&["switch", "-c", "b", "--track=inherit", "origin/gx"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "warning: asked to inherit tracking from 'refs/remotes/origin/gx', but no remote is set\n\
             Switched to a new branch 'b'\n",
            0
        )
    );
    assert_eq!(f.branch_config("b"), "");
}

#[test]
fn inherit_copies_every_merge_line() {
    let f = Fixture::new("inheritmany");
    f.run(&["config", "branch.main.remote", "origin"]);
    f.run(&["config", "branch.main.merge", "refs/heads/main"]);
    f.run(&["config", "--add", "branch.main.merge", "refs/heads/x"]);
    let got = f.run(&["switch", "-c", "b", "--track=inherit", "main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "branch 'b' set up to track:\n  origin/main\n  origin/x\n",
            "Switched to a new branch 'b'\n",
            0
        )
    );
    assert_eq!(
        f.branch_config("b"),
        "branch.b.remote origin\nbranch.b.merge refs/heads/main\nbranch.b.merge refs/heads/x\n"
    );
}

#[test]
fn an_explicit_direct_track_beats_autosetupmerge_inherit() {
    let f = Fixture::new("direct");
    let got = f.run(&[
        "-c",
        "branch.autoSetupMerge=inherit",
        "switch",
        "-c",
        "b",
        "--track=direct",
        "main",
    ]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        ("branch 'b' set up to track 'main'.\n", "Switched to a new branch 'b'\n", 0)
    );
    assert_eq!(f.branch_config("b"), "branch.b.remote .\nbranch.b.merge refs/heads/main\n");
}

#[test]
fn a_branch_is_never_its_own_upstream() {
    let f = Fixture::new("self");
    let got = f.run(&["switch", "-C", "main", "--track", "main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "warning: not setting branch 'main' as its own upstream\nReset branch 'main'\n",
            0
        )
    );
    assert_eq!(f.branch_config("main"), "");
}
