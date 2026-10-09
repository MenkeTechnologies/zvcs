//! `git merge-tree`: `prepare_repo_settings()` is lazy, so a branch operand that
//! resolves to nothing is `not something we can merge` even when a `core.*` value the
//! settings block refuses is configured; the die comes with the first object read.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_refused_core_value_is_reported_only_once_an_object_is_read() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-tree-lazy-settings", stock);
    for args in [
        &["-c", "core.deltaBaseCacheLimit=no", "merge-tree", "nosuch", "main"][..],
        &["-c", "core.deltaBaseCacheLimit=no", "merge-tree", "--write-tree", "nosuch", "main"],
        &["-c", "core.deltaBaseCacheLimit=no", "merge-tree", "main", "nosuch"],
        &["-c", "core.deltaBaseCacheLimit=no", "merge-tree", "main", "side"],
        &["-c", "core.packedGitLimit=bogus", "merge-tree", "--write-tree", "main", "side"],
        &["-c", "core.packedGitLimit=bogus", "merge-tree"],
        &["-c", "core.packedGitLimit=bogus", "merge-tree", "a", "b", "c"],
        &["-c", "core.packedGitLimit=bogus", "merge-tree", "--merge-base=nosuch", "main", "side"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
