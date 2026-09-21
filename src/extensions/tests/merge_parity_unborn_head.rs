//! `git merge` into a branch yet to be born.
//!
//! `cmd_merge` does not refuse an unborn `HEAD` (builtin/merge.c:1532-1562):
//! with no `head_commit` it checks the single named commit out with
//! `read_empty()` — `git read-tree -m -u <empty tree> <oid>`
//! (builtin/merge.c:378-389) — and moves `HEAD` with the reflog message
//! `initial pull`. That is the path `git pull` into a fresh, empty repository
//! takes, so refusing it breaks the first pull of every clone-by-hand.
//!
//! The arm also carries three `die()`s of its own: `--squash`, `--no-ff` and
//! more than one surviving head are each rejected before anything is written.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
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
    /// `main` with two commits (`c0`, `c1` as tags) and an unborn `kid` checked
    /// out over an emptied index and worktree — `t7600-merge.sh`'s `merge from
    /// unborn branch` shape.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-merge-unborn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "commit 0"]);
        f.git(&["tag", "c0"]);
        std::fs::write(f.work.join("file"), "two\n").unwrap();
        std::fs::write(f.work.join("other"), "other\n").unwrap();
        f.git(&["add", "file", "other"]);
        f.git(&["commit", "-q", "-m", "commit 1"]);
        f.git(&["tag", "c1"]);
        f
    }

    /// Leave `HEAD` on an unborn branch with an empty index and worktree.
    fn orphan(&self) {
        self.git(&["checkout", "-q", "--orphan", "kid"]);
        self.git(&["rm", "-q", "-fr", "."]);
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }
}

/// The whole operation: the commit's tree lands in the index and the worktree,
/// `HEAD` reaches the commit, the unborn branch is created by the deref, and
/// the reflog entry reads `initial pull` — not `merge …: Fast-forward`, which
/// is what the born-`HEAD` path would have written.
#[test]
fn merge_into_unborn_branch_checks_the_commit_out_and_logs_initial_pull() {
    let f = Fixture::new("ff");
    let c1 = f.oid("c1");
    f.orphan();

    let (out, err, code) = f.run(&["merge", "--ff-only", "c1"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));

    assert_eq!(f.oid("HEAD"), c1);
    assert_eq!(f.oid("refs/heads/kid"), c1);
    assert_eq!(std::fs::read_to_string(f.work.join("file")).unwrap(), "two\n");
    assert_eq!(std::fs::read_to_string(f.work.join("other")).unwrap(), "other\n");
    // Index and worktree agree with the new HEAD: nothing staged, nothing dirty.
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");

    let reflog = f.stdout(&["reflog", "-1"]);
    assert!(reflog.trim_end().ends_with("HEAD@{0}: initial pull"), "{reflog:?}");
}

/// `if (squash) die(…)` and `if (fast_forward == FF_NO) die(…)`
/// (builtin/merge.c:1539-1543) are checked before the operands are resolved, so
/// both leave `HEAD` unborn and the worktree empty.
#[test]
fn squash_and_no_ff_are_refused_before_an_unborn_head_is_touched() {
    let f = Fixture::new("refuse");
    f.orphan();

    let (out, err, code) = f.run(&["merge", "--squash", "c1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: Squash commit into empty head not supported yet\n", 128)
    );

    let (out, err, code) = f.run(&["merge", "--no-ff", "c1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: Non-fast-forward commit does not make sense into an empty head\n", 128)
    );

    let (out, _, code) = f.run(&["rev-parse", "--verify", "HEAD"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(!f.work.join("file").exists());
}

/// `if (remoteheads->next) die(_("Can merge only exactly one commit into empty
/// head"))` (builtin/merge.c:1548-1549) — but only after `reduce_parents()`, so
/// naming a commit and one of its own ancestors is still a single head and
/// succeeds.
#[test]
fn an_unborn_head_takes_exactly_one_independent_commit() {
    let f = Fixture::new("count");
    let c0 = f.oid("c0");
    let c1 = f.oid("c1");
    f.orphan();

    // `c0` is reachable from `c1`, so the reduction leaves one head.
    let (out, err, code) = f.run(&["merge", "c1", "c0"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.oid("HEAD"), c1);

    // Two independent tips do not reduce; the refusal names neither.
    let f = Fixture::new("count2");
    f.git(&["checkout", "-q", "-b", "side", "c0"]);
    std::fs::write(f.work.join("side"), "side\n").unwrap();
    f.git(&["add", "side"]);
    f.git(&["commit", "-q", "-m", "side"]);
    let side = f.oid("HEAD");
    assert_ne!(side, c0);
    f.orphan();
    let (out, err, code) = f.run(&["merge", "c1", "side"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: Can merge only exactly one commit into empty head\n", 128)
    );
}
