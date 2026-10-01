//! git 2.56's `git branch --delete-merged`, `--dry-run` and `--forked`, and the
//! `delete_branches()` they share with `-d`.
//!
//! `delete_merged_branches()` (builtin/branch.c:824-912) takes every local
//! branch matching the `<branch-pattern>`s whose upstream matches one of the
//! `--delete-merged` patterns (`ref_filter_forked_add()` /
//! `filter_forked_match()`, ref-filter.c:2747-2814), skips the ones a worktree
//! holds, whose upstream is gone, that push to their own upstream, that are not
//! merged into it, or that set `branch.<name>.deleteMerged=false`, then keeps
//! back any branch another surviving branch is stacked on. The rest go through
//! `delete_branches()` in `strset` (hashmap) order — which is why the report
//! lines below are not sorted.
//!
//! `delete_branches()` itself judges `-d` against the branch's upstream when it
//! has one (`branch_merged()`), refuses everything it is going to refuse before
//! deleting anything, and suggests `--remote` for a name only
//! `refs/remotes/` has. Every expectation was captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-branch-delete-merged-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn run(dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = git(dir, args);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `main` at c2 with `origin/main` there too; `a`..`e` forked at c1 and
/// tracking `origin/main`, except `d`, which tracks `origin/d` (so it pushes to
/// its own upstream). `b` carries an unmerged commit, `e` opts out, and `f`
/// is stacked on `c`.
fn mixed(name: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["config", "remote.origin.url", "."]);
    git(&dir, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    for b in ["a", "b", "c", "d", "e"] {
        git(&dir, &["branch", "-q", b]);
    }
    git(&dir, &["checkout", "-q", "b"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "bwork"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c2"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&dir, &["update-ref", "refs/remotes/origin/d", "HEAD"]);
    for b in ["a", "b", "c", "e"] {
        git(&dir, &["branch", "-q", "-u", "origin/main", b]);
    }
    git(&dir, &["branch", "-q", "-u", "origin/d", "d"]);
    git(&dir, &["config", "branch.e.deleteMerged", "false"]);
    git(&dir, &["branch", "-q", "-t", "f", "c"]);
    dir
}

#[test]
fn only_merged_unprotected_branches_go_and_a_dry_run_says_so() {
    let dir = mixed("mixed");
    let skip = "Skipping 'e' (branch.e.deleteMerged is false)\n";

    let dry = run(&dir, &["branch", "--delete-merged", "origin/main", "--dry-run"]);
    assert_eq!(dry, (Some(0), "Would delete branch a (was 8279be4).\n".into(), skip.into()));
    // A glob is matched against the upstream with `refs/remotes/` stripped.
    let glob = run(&dir, &["branch", "--delete-merged", "origin/*", "--dry-run"]);
    assert_eq!(glob, dry);

    let real = run(&dir, &["branch", "--delete-merged", "origin/main"]);
    assert_eq!(real, (Some(0), "Deleted branch a (was 8279be4).\n".into(), skip.into()));
    assert_eq!(
        run(&dir, &["branch"]).1,
        "  b\n  c\n  d\n  e\n  f\n* main\n"
    );
}

#[test]
fn forked_lists_by_upstream() {
    let dir = mixed("forked");
    assert_eq!(
        run(&dir, &["branch", "--forked", "origin/main"]),
        (Some(0), "  a\n  b\n  c\n  e\n".into(), String::new())
    );
    assert_eq!(
        run(&dir, &["branch", "--forked", "origin/*"]).1,
        "  a\n  b\n  c\n  d\n  e\n"
    );
}

#[test]
fn bad_patterns_and_a_lone_dry_run_are_refused() {
    let dir = mixed("refusals");
    for args in [
        &["branch", "--delete-merged", "nosuch"][..],
        &["branch", "--forked", "nosuch"][..],
    ] {
        assert_eq!(
            run(&dir, args),
            (Some(128), String::new(), "fatal: 'nosuch' is not a valid branch or pattern\n".into()),
            "{args:?}"
        );
    }
    assert_eq!(
        run(&dir, &["branch", "--dry-run"]),
        (Some(128), String::new(), "fatal: --dry-run requires --delete-merged\n".into())
    );
}

/// Twelve candidates make the `strset` iteration order visible; `<branch-pattern>`s
/// narrow the candidates; and a branch something else is stacked on survives
/// unless that something is deleted with it.
#[test]
fn deletions_follow_hashmap_order_and_respect_stacks() {
    let dir = scratch("order");
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["config", "remote.origin.url", "."]);
    git(&dir, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    for b in [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        "topic/one", "topic/two",
    ] {
        git(&dir, &["branch", "-q", "-t", b, "origin/main"]);
    }
    git(&dir, &["branch", "-q", "-t", "stack1", "alpha"]);
    git(&dir, &["branch", "-q", "-t", "stack2", "stack1"]);

    let would = |names: &[&str]| -> String {
        names
            .iter()
            .map(|n| format!("Would delete branch {n} (was 8279be4).\n"))
            .collect()
    };
    // `alpha` is held back: `stack1` stays and is stacked on it.
    assert_eq!(
        run(&dir, &["branch", "--delete-merged", "origin/main", "--dry-run"]).1,
        would(&[
            "theta", "epsilon", "zeta", "beta", "delta", "kappa", "topic/two", "gamma", "eta",
            "topic/one", "iota"
        ])
    );
    assert_eq!(
        run(&dir, &["branch", "--delete-merged", "origin/main", "--dry-run", "topic/*", "g*"]).1,
        would(&["topic/two", "gamma", "topic/one"])
    );
    assert_eq!(
        run(
            &dir,
            &[
                "branch", "--delete-merged", "origin/main", "--delete-merged", "alpha",
                "--delete-merged", "stack1", "--dry-run"
            ]
        )
        .1,
        would(&[
            "stack1", "stack2", "theta", "epsilon", "zeta", "beta", "alpha", "delta", "kappa",
            "topic/two", "gamma", "eta", "topic/one", "iota"
        ])
    );
    assert_eq!(
        run(&dir, &["branch", "--delete-merged", "alpha", "--delete-merged", "stack1"]),
        (
            Some(0),
            "Deleted branch stack1 (was 8279be4).\nDeleted branch stack2 (was 8279be4).\n".into(),
            String::new()
        )
    );
    assert_eq!(run(&dir, &["branch", "-q", "--delete-merged", "origin/main"]).1, "");
    assert_eq!(run(&dir, &["branch"]).1, "* main\n");
}

/// A protected branch whose own upstream was deleted keeps the branch but loses
/// the upstream configuration — the whole `[branch "c"]` section here.
#[test]
fn a_protected_branch_loses_a_deleted_upstream() {
    let dir = scratch("clear");
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["config", "remote.origin.url", "."]);
    git(&dir, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&dir, &["branch", "-q", "-t", "a", "origin/main"]);
    git(&dir, &["branch", "-q", "-t", "c", "a"]);
    git(&dir, &["branch", "-q", "-t", "f", "c"]);
    git(&dir, &["checkout", "-q", "f"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "fwork"]);
    git(&dir, &["checkout", "-q", "main"]);

    let args = ["branch", "--delete-merged", "origin/main", "--delete-merged", "a"];
    let mut dry = args.to_vec();
    dry.push("--dry-run");
    assert_eq!(run(&dir, &dry).1, "Would delete branch a (was 8279be4).\n");
    assert!(run(&dir, &["config", "branch.c.merge"]).1.starts_with("refs/heads/a"));

    assert_eq!(
        run(&dir, &args),
        (Some(0), "Deleted branch a (was 8279be4).\n".into(), String::new())
    );
    assert_eq!(
        run(&dir, &["config", "--get-regexp", "^branch\\."]).1,
        "branch.f.remote .\nbranch.f.merge refs/heads/c\n"
    );
    let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    assert!(!config.contains("[branch \"c\"]"), "{config}");
}

/// `-d` with an upstream: merged there but not into HEAD is deleted with the
/// transition-period warning.
#[test]
fn delete_judges_against_the_upstream() {
    let dir = scratch("upstream");
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["config", "remote.origin.url", "."]);
    git(&dir, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&dir, &["branch", "-q", "-t", "w", "origin/main"]);
    git(&dir, &["checkout", "-q", "w"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "w1"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "w"]);

    assert_eq!(
        run(&dir, &["branch", "-d", "w"]),
        (
            Some(0),
            "Deleted branch w (was 7f32249).\n".into(),
            "warning: deleting branch 'w' that has been merged to\n         \
             'refs/remotes/origin/main', but not yet merged to HEAD\n"
                .into()
        )
    );
}

/// Every refusal is reported before the first deletion, and a name only
/// `refs/remotes/` has gets the `--remote` suggestion.
#[test]
fn delete_reports_refusals_before_deletions() {
    let dir = scratch("batch");
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["branch", "tmp"]);
    git(&dir, &["update-ref", "refs/remotes/xx", "HEAD"]);
    assert_eq!(
        run(&dir, &["branch", "-d", "tmp", "nosuch", "xx"]),
        (
            Some(1),
            "Deleted branch tmp (was 8279be4).\n".into(),
            "error: branch 'nosuch' not found\n\
             error: branch 'xx' not found.\nDid you forget --remote?\n"
                .into()
        )
    );
}
