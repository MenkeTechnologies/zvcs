//! `git log` / `git reflog`: `setup_revisions()` takes its arguments in order, so a
//! revision that does not resolve dies before a malformed diff option written after it,
//! while the same option written first is the usage error.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;


#[test]
fn unresolvable_revision_before_a_bad_diff_option_dies_first() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("log-option-order", stock);
    for args in [
        &["log", "nosuch", "--color=bad"][..],
        &["log", "--color=bad", "nosuch"],
        &["log", "HEAD", "--color=bad"],
        &["log", "--max-count=-0", "nosuch", "--no-color", "--color=2m"],
        &["log", "main", "nosuch", "--stat=5x"],
        &["log", "nosuch", "--stat=5x"],
        &["reflog", "nosuch", "--color=bad"],
        &["reflog", "--no-abbrev-commit", "show", "HEAD", "show", "--color=999999999"],
        &["reflog", "show", "--color=bad", "nosuch"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
