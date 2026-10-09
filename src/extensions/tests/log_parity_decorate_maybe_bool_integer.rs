//! `git log --decorate=<n>` reads any integer `git_parse_maybe_bool()` takes, hex and unit
//! suffixes included.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;


#[test]
fn decorate_value_is_any_maybe_bool_integer() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("log-decorate-int", stock);
    for args in [
        &["log", "--decorate=0x10", "-1"][..],
        &["log", "--decorate=0x0", "-1"],
        &["log", "--decorate=1k", "-1"],
        &["log", "--decorate=bogus", "-1"],
        &["reflog", "--decorate=0x10", "--", "a"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
