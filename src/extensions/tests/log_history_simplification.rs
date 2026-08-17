//! `--full-history` and `--simplify-merges`, the two modes a file-history view asks
//! for.
//!
//! The default walk prunes: a commit TREESAME to a parent over the pathspec is not
//! shown, and only that parent is followed — which drops a merge and the entire side
//! whose change it took. `--full-history` clears `simplify_history`, so every parent
//! is followed and a commit is shown when *some* parent differs; the merge and the
//! side both come back. `--simplify-merges` then rewrites each commit to its
//! simplification, so a merge that contributed nothing of its own collapses into the
//! parent it matched.
//!
//! The case that matters is nested: `I -> {side, A} -> M1` and `M1, B -> M2`, where
//! `side` never touches the path. `M1`'s `side` parent rewrites to `I`, `I` is
//! redundant beside `A`, and dropping it leaves `M1` TREESAME to the `A` that
//! remains — so `M1` becomes `A`, and `M2`'s parent list is rewritten from `M1` to
//! `A`. Getting that wrong leaves `M1` in the log, which is what a first attempt at
//! this did.
//!
//! Expectations measured against stock git.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-histsimp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Fixture { root, repo }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.repo.join(name), body).unwrap();
    }

    /// Subjects of `git log <args>`, in order.
    fn subjects(&self, args: &[&str]) -> Vec<String> {
        let mut a = vec!["log", "--format=%s"];
        a.extend_from_slice(args);
        let out = self.cmd(&a).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {a:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    /// `I` (creates `f.txt`), a `side` that never touches it, `A` that does, the
    /// merge `M1` of those two, `B` off `I`, and `M2` merging `M1` with `B`, then
    /// `C`.
    fn nested_merges(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", "i\n");
        f.write("g.txt", "other\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "I"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        f.write("f.txt", "a\n");
        f.git(&["commit", "-qam", "A"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("g.txt", "other\nunrelated\n");
        f.git(&["commit", "-qam", "side-nopath"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "M1", "topic"]);
        f.git(&["checkout", "-q", "-b", "topic2", "HEAD~2"]);
        f.write("f.txt", "b\n");
        f.git(&["commit", "-qam", "B"]);
        f.git(&["checkout", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "M2", "-X", "theirs", "topic2"]);
        f.write("f.txt", "c\n");
        f.git(&["commit", "-qam", "C"]);
        f
    }
}

/// The default walk follows one TREESAME parent and drops the other side.
#[test]
fn the_default_walk_prunes_the_side_it_did_not_take() {
    const TAG: &str = "default";
    let f = Fixture::nested_merges(TAG);
    let seen = f.subjects(&["--", "f.txt"]);
    // Neither merge is shown, and the walk never reaches the branch whose change
    // the merge did not take.
    assert!(!seen.contains(&"M1".to_owned()), "{seen:?}");
    assert!(!seen.contains(&"M2".to_owned()), "{seen:?}");
    assert!(seen.contains(&"C".to_owned()) && seen.contains(&"I".to_owned()), "{seen:?}");
}

/// `--full-history` follows every parent, so both merges and both sides are shown.
#[test]
fn full_history_keeps_the_merges_and_every_side() {
    const TAG: &str = "full";
    let f = Fixture::nested_merges(TAG);
    let seen = f.subjects(&["--full-history", "--", "f.txt"]);
    for want in ["C", "M2", "M1", "B", "A", "I"] {
        assert!(seen.contains(&want.to_owned()), "{want} missing from {seen:?}");
    }
    // `side-nopath` never touched the path, so it is not shown even here.
    assert!(!seen.contains(&"side-nopath".to_owned()), "{seen:?}");
}

/// `--simplify-merges` collapses the merge that contributed nothing of its own.
#[test]
fn simplify_merges_collapses_a_merge_into_the_parent_it_matched() {
    const TAG: &str = "simp";
    let f = Fixture::nested_merges(TAG);
    let seen = f.subjects(&["--full-history", "--simplify-merges", "--", "f.txt"]);
    // `M1` took `A`'s content and its other parent is redundant, so it becomes `A`.
    // `M2` is a real merge of two differing sides and stays.
    assert_eq!(seen, ["C", "M2", "B", "A", "I"], "{seen:?}");

    // And the ancestry it prints is the rewritten one: `M2`'s parents are `A` and
    // `B`, not `M1` and `B`.
    let out = f
        .cmd(&["log", "--full-history", "--simplify-merges", "--parents", "--format=%s [%p]", "--", "f.txt"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let m2 = text.lines().find(|l| l.starts_with("M2 ")).expect("M2 shown");
    let parents: Vec<&str> = m2
        .split_once('[')
        .and_then(|(_, rest)| rest.strip_suffix(']'))
        .expect("`%s [%p]`")
        .split_whitespace()
        .collect();
    assert_eq!(parents.len(), 2, "M2 keeps two parents: {m2}");
    // Neither is `M1`: it was rewritten to the commit it simplified to.
    let m1 = f.cmd(&["rev-parse", "HEAD~1^1"]).output().unwrap();
    let m1 = String::from_utf8_lossy(&m1.stdout).trim_end().to_owned();
    assert!(
        !parents.iter().any(|p| m1.starts_with(*p)),
        "M2 still points at the simplified-away merge: {m2}"
    );
}

/// A path no commit ever touched simplifies to nothing at all.
#[test]
fn a_path_nothing_touched_shows_nothing() {
    const TAG: &str = "none";
    let f = Fixture::nested_merges(TAG);
    assert!(f.subjects(&["--full-history", "--simplify-merges", "--", "nosuch.txt"]).is_empty());
}

/// `revs->simplify_history` has three sources beyond `--full-history`, and each is
/// a plain `= 0` that nothing puts back.
///
/// ```c
/// static void set_separate(struct rev_info *revs)
/// {
///         common_setup(revs);
///         revs->separate_merges = 1;
///         revs->simplify_history = 0;
/// }
/// ```
///
/// (diff-merges.c:34-39.) So `-m` and every `--diff-merges` value that routes
/// through `set_separate()` turn history simplification off as a side effect of
/// choosing what a merge's diff shows — while `-c`, `--cc` and `--diff-merges=off`,
/// which run `set_combined()` / `set_dense_combined()` / `set_none()`, leave it on.
/// `cmd_whatchanged()` sets it directly (`rev.simplify_history = 0`,
/// builtin/log.c:620), which is the whole difference between `git whatchanged` and
/// `git log --raw` on a merge a pathspec makes TREESAME.
///
/// A single merge is enough to see it: `I -> {side-nopath, A} -> M`, where `M` took
/// `A`'s `f.txt` and is therefore TREESAME to `A` over that path.
#[test]
fn diff_merges_separate_and_whatchanged_clear_simplify_history() {
    const TAG: &str = "simplify-sources";
    let f = Fixture::nested_merges(TAG);

    // The default walk collapses `M1` onto the parent it matched.
    let base = f.subjects(&["--", "f.txt"]);
    assert!(!base.contains(&"M1".to_owned()), "{base:?}");

    for spelling in [
        vec!["-m"],
        vec!["--diff-merges=separate"],
        vec!["--diff-merges=1"],
        vec!["--diff-merges=on"],
        // `set_none()` runs only `suppress()`, which does not touch
        // `simplify_history`, so an `--diff-merges=off` *after* `-m` does not undo it.
        vec!["-m", "--diff-merges=off"],
        vec!["-m", "--no-diff-merges"],
    ] {
        let mut args = spelling.clone();
        args.extend_from_slice(&["--", "f.txt"]);
        let seen = f.subjects(&args);
        assert!(seen.contains(&"M1".to_owned()), "{spelling:?} simplified M1 away: {seen:?}");
    }

    // The modes that do not route through `set_separate()` leave it simplifying.
    for spelling in [vec!["-c"], vec!["--cc"], vec!["--diff-merges=off"]] {
        let mut args = spelling.clone();
        args.extend_from_slice(&["--format=%s", "--", "f.txt"]);
        let mut a = vec!["log"];
        a.extend_from_slice(&args);
        let out = f.cmd(&a).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {a:?}`: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            !text.lines().any(|l| l == "M1"),
            "{spelling:?} kept M1 in a simplifying walk: {text}"
        );
    }
}

/// `git whatchanged` keeps the merge its `simplify_history = 0` saved, but only
/// where something still prints a record for it: its `always_show_header` is off,
/// so a merge with an empty diff queue is dropped again on the way out. `-c` is
/// what makes it visible, because `diff_tree_combined()` prints the header itself
/// before it scans a path:
///
/// ```c
/// show_log_first = !!rev->loginfo && !rev->no_commit_id;
/// if (show_log_first) {
///         show_log(rev);
///         …
/// }
/// ```
///
/// (combine-diff.c:1512-1522.)
#[test]
fn whatchanged_keeps_a_treesame_merge_that_log_simplifies_away() {
    const TAG: &str = "wc-simplify";
    let f = Fixture::nested_merges(TAG);
    let subjects = |args: &[&str]| -> Vec<String> {
        let out = f.cmd(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| matches!(*l, "M1" | "M2" | "A" | "B" | "C" | "I"))
            .map(str::to_owned)
            .collect()
    };

    let logged = subjects(&["log", "-c", "--format=%s", "--", "f.txt"]);
    assert!(!logged.contains(&"M1".to_owned()), "log -c kept M1: {logged:?}");

    let changed = subjects(&[
        "whatchanged",
        "--i-still-use-this",
        "-c",
        "--format=%s",
        "--",
        "f.txt",
    ]);
    assert!(changed.contains(&"M1".to_owned()), "whatchanged -c dropped M1: {changed:?}");
}
