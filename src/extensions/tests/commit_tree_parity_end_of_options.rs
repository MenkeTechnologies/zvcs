//! `git commit-tree` takes `--end-of-options` as `parse_options()` does: the
//! token is consumed and everything after it is an operand.
//!
//! ```c
//! } else if (!strcmp(arg + 2, "end-of-options")) {
//!         if (!(ctx->flags & PARSE_OPT_KEEP_UNKNOWN_OPT)) {
//!                 ctx->argc--;
//!                 ctx->argv++;
//!         }
//!         break;
//! }
//! ```
//!
//! (parse-options.c:1119-1124.) zvcs refused it as ``unknown option
//! `end-of-options'`` with the usage block and exit 129.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn end_of_options_ends_option_parsing() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("commit-tree-eoo", stock);
    let cases: &[&[&str]] = &[
        &["commit-tree", "-m", "x", "--end-of-options", "HEAD^{tree}"],
        &["commit-tree", "-m", "x", "-p", "HEAD", "--end-of-options", "HEAD^{tree}"],
        &["commit-tree", "-m", "x", "--end-of-options", "-p"],
        &["commit-tree", "-m", "x", "--end-of-options", "--", "HEAD^{tree}"],
    ];
    for args in cases {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
