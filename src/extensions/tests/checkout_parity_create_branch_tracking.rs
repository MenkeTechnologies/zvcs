//! `git checkout -b <new> <start>` creates the branch through `create_branch()`.
//!
//! `update_refs_for_switch()` calls `create_branch(…, new_branch_info->name, …,
//! opts->track, 0)` (builtin/checkout.c:979-986) *after* `merge_working_tree()`
//! has moved the worktree (builtin/checkout.c:1215-1253). `create_branch()`
//! resolves the start-point again in `dwim_branch_start()` (branch.c:539-594) —
//! a second `refname … is ambiguous` warning, and `ambiguous object name` dies
//! there, with the worktree already moved — and records the upstream through
//! `setup_tracking()` (branch.c:645-646), which is where `BRANCH_TRACK_INHERIT`
//! copies the start branch's own upstream. `opts->track` is `cfg->branch_track`
//! whenever no `--track`/`--no-track` was given (builtin/checkout.c:1702-1703),
//! which includes the `--guess` DWIM (its `dwim_ok` requires
//! `track == BRANCH_TRACK_UNSPECIFIED`, builtin/checkout.c:1992-1996).
//!
//! zvcs kept a private copy of the tracking decision: `--track=inherit` was
//! refused as unsupported, the ambiguity die came before the worktree moved,
//! `<a>...<b>` warned once, and the DWIM path always tracked.
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
    /// `main` has `f = a`; `other` has `f = b`. `amb` is both a branch and a tag
    /// at `other`; `up` is a branch at `main` whose upstream is `origin/x`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-create-tracking-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["checkout", "-q", "-b", "other"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["branch", "amb", "other"]);
        f.run(&["tag", "amb", "other"]);
        f.run(&["branch", "up"]);
        f.run(&["config", "branch.up.remote", "origin"]);
        f.run(&["config", "branch.up.merge", "refs/heads/x"]);
        f.run(&["remote", "add", "origin", "/nonexistent"]);
        f.run(&["update-ref", "refs/remotes/origin/rb", "main"]);
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
}

#[test]
fn track_inherit_copies_the_start_branchs_upstream() {
    let f = Fixture::new("inherit");
    let (out, err, code) = f.run(&["checkout", "-b", "b", "--track=inherit", "up"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("branch 'b' set up to track 'origin/x'.\n", "Switched to a new branch 'b'\n", 0)
    );
    assert_eq!(f.branch_config("b"), "branch.b.remote origin\nbranch.b.merge refs/heads/x\n");
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/b\n");

    // A later bare `--track` is `BRANCH_TRACK_EXPLICIT` again: the start branch
    // itself becomes the upstream.
    f.run(&["checkout", "-q", "main"]);
    let (_, _, code) = f.run(&["checkout", "-q", "-b", "i", "--track=inherit", "--track", "up"]);
    assert_eq!(code, 0);
    assert_eq!(f.branch_config("i"), "branch.i.remote .\nbranch.i.merge refs/heads/up\n");

    // `inherit_tracking()` (branch.c:217-243) on a start branch with no upstream
    // warns and records nothing; the branch is still created.
    f.run(&["checkout", "-q", "main"]);
    let (out, err, code) = f.run(&["checkout", "-q", "-b", "j", "--track=inherit", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "warning: asked to inherit tracking from 'main', but no remote is set\n", 0)
    );
    assert_eq!(f.branch_config("j"), "");
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/j\n");
}

#[test]
fn an_ambiguous_start_point_dies_after_the_worktree_moved() {
    let f = Fixture::new("ambiguous");
    let (out, err, code) = f.run(&["checkout", "-b", "e", "amb"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "warning: refname 'amb' is ambiguous.\n\
             warning: refname 'amb' is ambiguous.\n\
             fatal: ambiguous object name: 'amb'\n",
            128
        )
    );
    // `merge_working_tree()` already ran: worktree and index are `other`'s,
    // `HEAD` is still `main`, and no branch was written.
    assert_eq!(std::fs::read_to_string(f.work.join("f")).unwrap(), "b\n");
    assert_eq!(f.run(&["status", "--short"]).0, "M  f\n");
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/main\n");
    assert_eq!(f.run(&["rev-parse", "-q", "--verify", "refs/heads/e"]).2, 1);
}

#[test]
fn a_merge_base_start_point_is_resolved_twice() {
    let f = Fixture::new("mergebase");
    let (out, err, code) = f.run(&["checkout", "-b", "d", "amb...main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "warning: refname 'amb' is ambiguous.\n\
             warning: refname 'amb' is ambiguous.\n\
             Switched to a new branch 'd'\n",
            0
        )
    );
    assert_eq!(f.run(&["rev-parse", "d"]).0, f.run(&["rev-parse", "main"]).0);
}

#[test]
fn the_guess_dwim_follows_branch_autosetupmerge() {
    let f = Fixture::new("dwim");
    let (out, err, code) = f.run(&["-c", "branch.autoSetupMerge=false", "checkout", "rb"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "Switched to a new branch 'rb'\n", 0)
    );
    assert_eq!(f.branch_config("rb"), "");
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/rb\n");
}
