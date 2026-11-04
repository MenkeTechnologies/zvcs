//! `report_path_error()` against stock git for an `:(exclude)` element: `do_match_pathspec()`
//! marks an exclusion seen only on the `DO_MATCH_EXCLUDE` pass that follows a positive match
//! (dir.c:557-563), so it is named whenever no element matched positively, and an
//! all-exclusion pathspec gains a `.` item (pathspec.c:671-678) that is named too when no
//! name was there to match. Every unmatched element is named, not only the first. Covers
//! each caller: `add -u`, `checkout`, `restore`, `commit`, `stash push` and
//! `ls-files --error-unmatch`, plus `rm`, whose own loop dies on the first unmatched item.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

fn stock_view(stock: &str, repo: &Path, args: &[&str]) -> String {
    let out = Command::new(stock)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

const CASES: &[&[&str]] = &[
    &["add", "-u", "--", "new", ":(exclude)a"],
    &["add", "-u", "--", ":!zz"],
    &["add", "-u", "--", "new", "new"],
    &["checkout", "--", "n1", "n2", ":!zz"],
    &["checkout", "main", "--", "n1", ":!zz"],
    &["checkout", "--no-overlay", "main", "--", "n1", ":!zz"],
    &["checkout", "--", ":!zz"],
    &["restore", "--", "n1", "n2", ":!zz"],
    &["restore", "-s", "main", "--", "n1", ":!zz"],
    &["restore", "--", "a", ":!zz"],
    &["commit", "-m", "x", "--", "n1", ":!zz"],
    &["commit", "-m", "x", "--", "n*", ":(top)n2"],
    &["commit", "-m", "x", "-i", "--", "n1", ":!zz"],
    &["commit", "-m", "x", "--", ":!zz"],
    &["stash", "push", "--", "n1", ":!zz"],
    &["stash", "push", "--", ":!zz"],
    &["ls-files", "--error-unmatch", "--", "n1", ":!zz"],
    &["ls-files", "--error-unmatch", "--", "a", ":!zz", "n1", "n1"],
    &["ls-files", "--error-unmatch", "--", ":!zz"],
    &["ls-files", "--error-unmatch", "--", ":!a"],
    &["rm", "-n", "--", ":!zz", "n1"],
    &["rm", "-n", "--", ":!a"],
    &["rm", "-n", "--", ":!zz"],
    &["rm", "-n", "--ignore-unmatch", "--", ":!zz"],
];

fn run_cases(s: &twin_repo::Side, z: &twin_repo::Side, stock: &str, label: &str, setup: &dyn Fn(&twin_repo::Side)) {
    for args in CASES {
        for side in [s, z] {
            side.git(&["reset", "-q", "--hard", "main"]);
            side.git(&["stash", "clear"]);
            side.write("a", "edited\n");
            side.write("new", "new\n");
            setup(side);
        }
        assert_eq!(z.git(args), s.git(args), "{label}: {args:?}");
        for view in [&["status", "--porcelain"][..], &["stash", "list"]] {
            assert_eq!(
                stock_view(stock, &z.repo(), view),
                stock_view(stock, &s.repo(), view),
                "{label}: {view:?} after {args:?}"
            );
        }
    }
}

#[test]
fn exclusions_are_named_when_nothing_matched_positively() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("pathspec-exclude-report", stock);
    run_cases(&s, &z, stock, "tracked", &|_| {});
}

#[test]
fn an_all_exclusion_pathspec_over_an_empty_index_names_dot() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("pathspec-exclude-report-empty", stock);
    run_cases(&s, &z, stock, "empty index", &|side| {
        side.git(&["read-tree", "--empty"]);
    });
}
