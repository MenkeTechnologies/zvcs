//! `git last-modified`: `setup_revisions()` takes every argument before
//! `populate_paths_from_revs()` counts the tips, so a later operand that is neither a revision
//! nor a path dies first.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_bad_operand_after_a_second_tip_dies_before_the_tip_count() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("last-modified-tip-count", stock);
    for args in [
        &["last-modified", "main", "side", "nosuch"][..],
        &["last-modified", "main", "side"],
        &["last-modified", "main", "side", "--bogus"],
        &["last-modified", "main", "side", "nosuch", "--bogus"],
        &["last-modified", "--max-depth=2", "main", "side", "a", "nosuch"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
