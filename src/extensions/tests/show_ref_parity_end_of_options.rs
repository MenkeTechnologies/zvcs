//! `show-ref --end-of-options` ends option parsing the way `--` does.
//!
//! parse_options_step() tests `--end-of-options` with an exact `strcmp()` ahead of
//! parse_long_opt() and consumes it (parse-options.c:1119-1125), so everything after it
//! is a pattern or ref even when it starts with `-`. zvcs rejected it as an unknown
//! option, exit 129 with the usage block.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn end_of_options_ends_option_parsing() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("show-ref-end-of-options", stock);
    for args in [
        &["show-ref", "--end-of-options", "main"][..],
        &["show-ref", "-s", "--end-of-options", "side", "main"],
        &["show-ref", "--verify", "--end-of-options", "refs/heads/main"],
        &["show-ref", "--end-of-options", "--tags"],
        &["show-ref", "--exists", "--end-of-options", "refs/heads/side"],
        &["show-ref", "--end-of-options", "--end-of-options"],
    ] {
        let (so, zo) = (s.git(args), z.git(args));
        assert_ne!(so.code, 129, "{args:?}: {so:?}");
        assert_eq!(so, zo, "{args:?}");
    }
}
