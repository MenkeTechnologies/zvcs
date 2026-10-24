//! `git diff-index` under a `core.excludesFile` that names a directory.
//!
//! `run_diff_index()` hands the tree to `diff_cache()`, whose `unpack_trees()`
//! leaves `preserve_ignored` at 0, so `setup_standard_excludes()` dies with
//! `cannot use <path> as an exclude file` before a pair is queued — with or
//! without `--cached`, and whether or not anything differs (diff-lib.c:559-589,
//! 634-668; unpack-trees.c:1922-1926). zvcs diffed as if the file were fine.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;
#[path = "support/unusable_excludes.rs"]
mod unusable_excludes;

use unusable_excludes::{broken, same};

#[test]
fn every_tree_diff_dies_in_unpack_trees() {
    let Some((s, z)) = broken("diff-index-excl") else { return };
    for args in [
        &["diff-index", "HEAD"][..],
        &["diff-index", "--cached", "HEAD"],
        &["diff-index", "--quiet", "HEAD"],
        &["diff-index", "-m", "-p", "HEAD~1"],
        &["diff-index", "--merge-base", "side"],
        &["diff-index", "HEAD", "--", "a"],
    ] {
        assert_eq!(same(&s, &z, args), 128, "{args:?}");
    }
}

/// What `setup_revisions()` and `diff_cache()` refuse before `unpack_trees()`
/// keeps its own answer: the usage block, a bad revision, a tree-ish that is
/// not a tree.
#[test]
fn earlier_refusals_keep_their_place() {
    let Some((s, z)) = broken("diff-index-excl-order") else { return };
    assert_eq!(same(&s, &z, &["diff-index"]), 129);
    same(&s, &z, &["diff-index", "nosuch"]);
    same(&s, &z, &["diff-index", "HEAD:a"]);
}
