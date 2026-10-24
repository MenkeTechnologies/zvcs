//! `git stash create` and `git stash push` under a `core.excludesFile` that
//! names a directory.
//!
//! Both open `check_changes_tracked_files()`, which once `HEAD` resolves runs
//! `run_diff_index(&rev, DIFF_INDEX_CACHED)`; its `unpack_trees()` runs
//! `setup_standard_excludes()` and dies with `cannot use <path> as an exclude
//! file` (builtin/stash.c:1241-1279, 1646-1669, 1723-1733;
//! unpack-trees.c:1922-1926). `push` gets there after its pathspec check and
//! `repo_refresh_and_write_index()`; `create` before anything else. No stash
//! is created and `refs/stash` does not move. zvcs stashed regardless.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;
#[path = "support/unusable_excludes.rs"]
mod unusable_excludes;

use unusable_excludes::{broken, same};

#[test]
fn create_dies_before_building_a_commit() {
    let Some((s, z)) = broken("stash-excl-create") else { return };
    assert_eq!(same(&s, &z, &["stash", "create"]), 128);
    assert_eq!(same(&s, &z, &["stash", "create", "a", "message"]), 128);
}

#[test]
fn every_push_spelling_dies_in_check_changes() {
    let Some((s, z)) = broken("stash-excl-push") else { return };
    for args in [
        &["stash"][..],
        &["stash", "-q"],
        &["stash", "push", "-m", "x"],
        &["stash", "push", "--staged"],
        &["stash", "push", "-u"],
        &["stash", "push", "--keep-index"],
        &["stash", "push", "--", "a"],
        &["stash", "save", "x"],
    ] {
        assert_eq!(same(&s, &z, args), 128, "{args:?}");
    }
    // The pathspec check stands ahead of it.
    same(&s, &z, &["stash", "push", "--", "nosuch"]);
}

/// On an unborn branch `check_changes_tracked_files()` returns before the
/// diff, so `do_create_stash()`'s own refusal is what both print.
#[test]
fn an_unborn_head_never_reaches_the_diff() {
    let Some((s, z)) = broken("stash-excl-unborn") else { return };
    for side in [&s, &z] {
        // `checkout` unpacks too, so the orphan is made with the file usable.
        side.git(&["config", "--unset", "core.excludesFile"]);
        assert_eq!(side.git(&["checkout", "-q", "--orphan", "new"]).code, 0);
        side.git(&["config", "core.excludesFile", "d"]);
    }
    same(&s, &z, &["stash", "create"]);
    same(&s, &z, &["stash"]);
}
