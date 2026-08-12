//! The reflog a rebase leaves on `HEAD`.
//!
//! `sequencer_reflog_action()` prefixes every entry with `GIT_REFLOG_ACTION`
//! when the caller set one — `pull` sets it to its own command line — and with
//! `rebase` otherwise. The run ends with `(finish): returning to <ref>`, which
//! is what tells `git reflog` where the rebase put you; the vendored `gix-ref`
//! drops reflog lines for symbolic-target updates, so that entry has to be
//! written explicitly.
//!
//! Also here, because it shares the fixture: `--rebase-merges` recreates a
//! merge in the replayed range rather than flattening it.
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
    /// `topic` with one commit, forked from a `main` that has moved on.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rbreflog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(f.work.join("t.txt"), "t\n").unwrap();
        f.git(&["add", "t.txt"]);
        f.git(&["commit", "-q", "-m", "topicwork"]);
        f.git(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("m.txt"), "m\n").unwrap();
        f.git(&["add", "m.txt"]);
        f.git(&["commit", "-q", "-m", "mainwork"]);
        f.git(&["checkout", "-q", "topic"]);
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

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The reflog messages on `HEAD`, newest first.
    fn head_reflog(&self) -> Vec<String> {
        let out = self.cmd(&["reflog", "show", "HEAD", "--format=%gs"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }
}

/// A plain rebase: `rebase (start)`, one `(pick)` per replayed commit, and the
/// `(finish)` that re-attaches `HEAD`.
#[test]
fn a_rebase_records_start_pick_and_finish() {
    let f = Fixture::new("plain");
    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    let log = f.head_reflog();
    assert_eq!(
        &log[..3],
        [
            "rebase (finish): returning to refs/heads/topic",
            "rebase (pick): topicwork",
            "rebase (start): checkout main",
        ],
        "reflog: {log:?}"
    );
}

/// The caller's `GIT_REFLOG_ACTION` replaces the `rebase` prefix, which is how a
/// `pull --rebase` is distinguishable from a hand-run rebase afterwards.
#[test]
fn git_reflog_action_replaces_the_prefix() {
    let f = Fixture::new("action");
    let out = f
        .cmd(&["rebase", "main"])
        .env("GIT_REFLOG_ACTION", "pull --rebase")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log = f.head_reflog();
    assert_eq!(
        &log[..3],
        [
            "pull --rebase (finish): returning to refs/heads/topic",
            "pull --rebase (pick): topicwork",
            "pull --rebase (start): checkout main",
        ],
        "reflog: {log:?}"
    );
}

/// `--rebase-merges` recreates the merge rather than flattening it: the rebased
/// tip is still a two-parent commit, its second parent still carries `side`'s
/// work, and the whole thing now sits on `main`.
///
/// This is what `make_script_with_merges()`'s `label`/`reset`/`merge`
/// instructions buy — a plain rebase of the same range replays only the
/// non-merge commits and leaves a linear branch.
#[test]
fn rebase_merges_recreates_the_merge_instead_of_flattening_it() {
    let f = Fixture::new("rebase-merges");
    // topic: topicwork, then a real merge of `side`.
    f.git(&["checkout", "-q", "-b", "side"]);
    std::fs::write(f.work.join("s.txt"), "s\n").unwrap();
    f.git(&["add", "s.txt"]);
    f.git(&["commit", "-q", "-m", "swork"]);
    f.git(&["checkout", "-q", "topic"]);
    f.git(&["merge", "--no-edit", "--no-ff", "-q", "side"]);
    let before = f.run(&["rev-parse", "HEAD"]).1;

    let (ok, out, err) = f.run(&["rebase", "--rebase-merges", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    // The tip moved (it was replayed onto `main`) and is still a merge.
    let after = f.run(&["rev-parse", "HEAD"]).1;
    assert_ne!(after, before, "the branch should have been replayed onto main");
    let parents = f.run(&["rev-list", "--parents", "-n1", "HEAD"]).1;
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "the rebased tip must still be a two-parent merge: {parents}"
    );
    // `mainwork` is now an ancestor, and both sides of the merge survived.
    for spec in ["HEAD^{/mainwork}", "HEAD^{/swork}", "HEAD^{/topicwork}"] {
        let (ok, _, err) = f.run(&["rev-parse", "--verify", spec]);
        assert!(ok, "{spec} is missing after the rebase: {err}");
    }
    // `refs/rewritten/*` is scratch state and must not outlive the rebase.
    let (_, refs, _) = f.run(&["for-each-ref", "--format=%(refname)", "refs/rewritten/"]);
    assert!(refs.trim().is_empty(), "rewritten refs left behind: {refs}");
    assert!(!f.work.join(".git/rebase-merge").exists(), "no rebase state may be left behind");

    // A linear range still replays, with the merge backend selected.
    f.git(&["checkout", "-q", "-B", "linear", "topic~1"]);
    let (ok, out, err) = f.run(&["rebase", "--rebase-merges", "main"]);
    assert!(ok, "a linear --rebase-merges should work: {out}{err}");
}
