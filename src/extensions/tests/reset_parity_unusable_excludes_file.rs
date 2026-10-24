//! `git reset` under a `core.excludesFile` that names a directory.
//!
//! `reset_index()` (`--hard`, `--merge`, `--keep`) and `read_from_tree()`'s
//! `do_diff_cache()` (`--mixed`, with or without paths) all reach
//! `unpack_trees()`, which leaves `preserve_ignored` at 0 and so runs
//! `setup_standard_excludes()`; `add_patterns_from_file()` dies with
//! `cannot use <path> as an exclude file` before `reset_refs()` moves anything
//! (builtin/reset.c:62-133, 196-221, 496-530; unpack-trees.c:1922-1926;
//! dir.c:1013). `--soft` never unpacks and succeeds. zvcs reset regardless.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;
#[path = "support/unusable_excludes.rs"]
mod unusable_excludes;

use unusable_excludes::{broken, same};

#[test]
fn every_unpacking_mode_dies_before_the_ref_moves() {
    let Some((s, z)) = broken("reset-excl-modes") else { return };
    for args in [
        &["reset", "--hard"][..],
        &["reset", "--hard", "HEAD~1"],
        &["reset", "-q", "--hard", "HEAD~1"],
        &["reset", "--merge", "HEAD~1"],
        &["reset", "--keep", "HEAD~1"],
        &["reset", "HEAD~1"],
        &["reset", "--mixed", "-q", "HEAD~1"],
        &["reset", "--no-refresh"],
        &["reset", "HEAD~1", "--", "a"],
        &["reset", "HEAD~1^{tree}", "--", "a"],
    ] {
        assert_eq!(same(&s, &z, args), 128, "{args:?}");
    }
    // `--soft` reads no tree into the index.
    assert_eq!(same(&s, &z, &["reset", "--soft", "HEAD~1"]), 0);
}

/// The refusals that stand ahead of `unpack_trees()` still win: `--keep`
/// inside a merge, and an operand that does not resolve.
#[test]
fn earlier_refusals_keep_their_place() {
    let Some((s, z)) = broken("reset-excl-order") else { return };
    same(&s, &z, &["reset", "--keep", "nosuch"]);
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "--", "a"]);
        side.git(&["config", "--unset", "core.excludesFile"]);
        side.git(&["merge", "-q", "side"]);
        side.git(&["config", "core.excludesFile", "d"]);
    }
    same(&s, &z, &["reset", "--keep"]);
    assert_eq!(same(&s, &z, &["reset", "--merge"]), 128);
    assert!(z.repo().join(".git/MERGE_HEAD").exists());
}
