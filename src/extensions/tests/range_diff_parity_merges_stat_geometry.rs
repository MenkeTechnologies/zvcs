//! `git range-diff` over ranges that hold merges, `--stat` geometry, and a
//! pathspec given from a subdirectory.
//!
//! Every `--diff-merges` spelling sets `include_merges` (builtin/range-diff.c:
//! 93-97), so `read_patches()` reads each range from a `git log` child without
//! `--no-merges` (range-diff.c:54-75) and folds whatever that log prints for a
//! merge into patches: `separate` gives one `commit <id> (from <parent>)` record
//! per parent, a combined diff has no `diff --git` line and stays in the header,
//! and a remerge diff's `remerge CONFLICT` line ends `parse_git_diff_header()`'s
//! table and turns into context. zvcs always walked with merges excluded.
//!
//! `diff_opt_stat()` (diff.c:5381-5427) threads `--stat=<w>[,<n>[,<c>]]` and the
//! `--stat-*` options into the outer diff's `show_stats()`; zvcs refused all of
//! them but a bare `--stat`.
//!
//! The inner log runs where `setup_git_directory()` left range-diff, at the top
//! of the work tree (setup.c:1268-1272), so `-- <path>` names a path from there,
//! and a pathspec it refuses is that log's `die()` followed by `could not parse
//! log` (range-diff.c:78-86, 255).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::{Out, Side};

/// Run `args` in `side`'s repository with stock git, so both worlds hold the
/// same objects whatever zvcs's own porcelain would have written.
fn stock_run(side: &Side, stock: &str, args: &[&str]) -> Out {
    let setup = Side { root: side.root.clone(), bin: stock.to_owned() };
    let out = setup.git(args);
    // `setup` shares `side`'s directory, which only `side` may remove.
    std::mem::forget(setup);
    out
}

/// `x` and `y` both rewrite line 2 of `a` from `main~2`; `m1` and `m2` merge `x`
/// into `y` and resolve the conflict two ways. `sub/f` gives the subdirectory
/// the pathspec cases run from.
fn fixture(side: &Side, stock: &str) {
    let git = |args: &[&str]| {
        let out = stock_run(side, stock, args);
        assert!(out.code == 0 || args.contains(&"merge"), "{args:?}: {out:?}");
    };
    git(&["checkout", "-q", "-b", "x", "main~2"]);
    side.write("a", "1\nX\n");
    git(&["commit", "-q", "-am", "x"]);
    git(&["checkout", "-q", "-b", "y", "main~2"]);
    side.write("a", "1\nY\n");
    side.write("sub/f", "f\n");
    git(&["add", "a", "sub/f"]);
    git(&["commit", "-q", "-m", "y"]);
    git(&["branch", "y2"]);
    for (branch, resolution) in [("y", "1\nXY\n"), ("y2", "1\nXY2\n")] {
        git(&["checkout", "-q", branch]);
        git(&["-c", "maintenance.auto=false", "merge", "-q", "-m", "merge x", "x"]);
        side.write("a", resolution);
        git(&["add", "a"]);
        git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "merge x"]);
    }
    git(&["checkout", "-q", "main"]);
    // Untracked, so it survives the checkout of `main`.
    std::fs::create_dir_all(side.repo().join("sub")).unwrap();
}

#[test]
fn merges_stat_geometry_and_subdirectory_pathspecs_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("range-diff-merges", stock);
    fixture(&s, stock);
    fixture(&z, stock);

    for args in [
        // One record per parent of each merge, and the merge's conflict notice.
        &["range-diff", "--diff-merges=separate", "main~2..y", "main~2..y2"][..],
        &["range-diff", "--remerge-diff", "main~2..y", "main~2..y2"],
        &["range-diff", "--diff-merges=cc", "-s", "main~2..y", "main~2..y2"],
        // Refused by the inner log, not by range-diff's parser.
        &["range-diff", "--no-remerge-diff", "main~2..y", "main~2..y2"],
        // The outer diff's stat geometry.
        &["range-diff", "--creation-factor=1000", "--stat=30,2", "main~2..x", "main~2..y~1"],
        &["range-diff", "--creation-factor=1000", "--stat-graph-width=1", "main~2..x", "main~2..y~1"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }

    // From `sub`, `a` is the top-level file and `../a` is outside the repository.
    for args in [
        &["range-diff", "main~2..x", "main~2..y~1", "--", "a"][..],
        &["range-diff", "main~2..x", "main~2..y~1", "--", "../a"],
    ] {
        let at = |side: &Side| side.run_in(&side.repo().join("sub"), &[], args);
        assert_eq!(at(&z), at(&s), "{args:?}");
    }
}
