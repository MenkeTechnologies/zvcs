//! An absolute pathspec element is resolved with `real_path()` before the inside/outside
//! check (`prefix_path_gently()`), so a missing parent is `Invalid path '<parent>'`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn an_absolute_pathspec_with_a_missing_parent_is_an_invalid_path() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("last-modified-invalid-path", stock);
    for args in [
        &["last-modified", "--", "/zvcs-no-such-dir/x"][..],
        &["last-modified", "main", "--", "/zvcs-no-such-dir/x"],
        &["last-modified", "--", "/zvcs-no-such-dir/sub/x"],
        &["ls-files", "--", "/zvcs-no-such-dir/x"],
        &["ls-files", "-o", "--", "/zvcs-no-such-dir/x"],
        &["ls-files", "--", "/etc/hosts/x"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
