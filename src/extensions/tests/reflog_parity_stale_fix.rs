//! `git reflog expire --stale-fix`, checked against stock on twin fixtures.
//!
//! With `--stale-fix` an entry is pruned when either id fails `keep_entry()`
//! (reflog.c:277-300): the commit is missing, or `commit_is_complete()` finds a
//! commit, tree or blob missing anywhere between it and the commits
//! `mark_reachable_objects()` marked `SEEN` (builtin/reflog.c:231-243). zvcs
//! accepted the option and ran no walk, so nothing was pruned and
//! `Marking reachable objects...` was never printed. The loop over named refs
//! builds its callback without `dry_run` (builtin/reflog.c:289), so `-n --verbose`
//! says `prune` there.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], date: u64) -> (i32, String, String) {
    let date = format!("{date} +0000");
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `A-B-C` on `main`; `D-F` on a branch off `B` (`D` adds `d`) and `G` on
/// another, both branches deleted so only the reflogs name them; `keep` was
/// reset to `F` and back to `B`. Then the object `missing` names is removed.
fn fixture(stock: &str, label: &str, missing: &str, extra_ref: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-reflog-stale-fix-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let mut date = 1_700_000_000;
    let mut run = |args: &[&str]| {
        date += 60;
        let out = git(stock, &dir, &[&["-c", "maintenance.auto=false", "-c", "gc.auto=0"], args].concat(), date);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
        out.1
    };
    run(&["init", "-q", "-b", "main"]);
    let mut commit = |run: &mut dyn FnMut(&[&str]) -> String, name: &str| {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
        run(&["add", name]);
        run(&["commit", "-q", "-m", name]);
    };
    commit(&mut run, "a");
    commit(&mut run, "b");
    run(&["checkout", "-q", "-b", "side"]);
    commit(&mut run, "d");
    commit(&mut run, "f");
    run(&["checkout", "-q", "main"]);
    commit(&mut run, "c");
    run(&["checkout", "-q", "-b", "good", "main~1"]);
    commit(&mut run, "g");
    run(&["checkout", "-q", "main"]);
    run(&["branch", "-q", "keep", "main~1"]);
    run(&["branch", "-f", "keep", "side"]);
    run(&["branch", "-f", "keep", "main~1"]);
    run(&["branch", "-D", "-q", "side", "good"]);
    if extra_ref {
        run(&["update-ref", "refs/heads/bad", "HEAD@{5}"]);
    }
    let oid = run(&["rev-parse", missing]);
    let oid = oid.trim();
    std::fs::remove_file(dir.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
    dir
}

/// Every log file under `.git/logs`, path and bytes.
fn logs(dir: &Path) -> String {
    let mut out = String::new();
    for name in ["HEAD", "refs/heads/main", "refs/heads/keep"] {
        let body = std::fs::read_to_string(dir.join(".git/logs").join(name)).unwrap_or_default();
        out.push_str(&format!("## {name}\n{body}"));
    }
    out
}

fn same(label: &str, missing: &str, extra_ref: bool, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, &format!("{label}-{side}"), missing, extra_ref);
        let out = git(bin, &dir, args, 1_700_100_000);
        let refs = git(stock, &dir, &["for-each-ref"], 1_700_100_000);
        seen.push((out, logs(&dir), refs));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "git {args:?}: left is zvcs, right is stock");
}

const NEVER: [&str; 2] = ["--expire=never", "--expire-unreachable=never"];

/// A blob missing below `D` makes `D` and `F` incomplete; `G` is whole and kept.
#[test]
fn a_missing_blob_prunes_every_entry_above_it() {
    same("blob", "HEAD@{6}:d", false, &[&["reflog", "expire", "--stale-fix", "--all", "--verbose"][..], &NEVER].concat());
    same(
        "blob-rw",
        "HEAD@{6}:d",
        false,
        &[&["reflog", "expire", "--stale-fix", "--all", "--rewrite", "--updateref"][..], &NEVER].concat(),
    );
}

/// `commit_is_complete()` checks the tree of the `SEEN` commit its study stops
/// at, so `B`'s missing tree takes `D`, `F` and `G` with it while the entries
/// naming `B` itself, which the walk marked, stay.
#[test]
fn a_reachable_commit_with_a_missing_tree_fails_its_children() {
    same("reachtree", "main~1^{tree}", false, &[&["reflog", "expire", "--stale-fix", "--all", "--verbose"][..], &NEVER].concat());
}

/// Named refs: `would prune` is only said under `--all`.
#[test]
fn named_refs_under_dry_run_say_prune() {
    same("named-dry", "HEAD@{5}", false, &["reflog", "expire", "--stale-fix", "-n", "--verbose", "HEAD", "keep"]);
}

/// A ref naming a missing object stops the walk: `parse_object_or_die()`.
#[test]
fn a_ref_to_a_missing_object_is_fatal() {
    same("bad-ref", "HEAD@{5}", true, &["reflog", "expire", "--stale-fix", "--verbose", "--all"]);
}
