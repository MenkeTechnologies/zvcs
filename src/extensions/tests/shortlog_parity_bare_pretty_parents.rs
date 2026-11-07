//! `git shortlog --pretty`, `--oneline` and `--parents`.
//!
//! handle_revision_opt() takes all three (revision.c:2491-2493, :2594-2597,
//! :2648-2652), and cmd_shortlog() hands it every word its own table does not
//! claim (builtin/shortlog.c:430-445). The bare format options set a built-in
//! format, so a `--format=<user>` before them no longer applies
//! (builtin/shortlog.c:461), while `--parents` rewrites the parents a commit is
//! shown with and not which commits are walked. zvcs refused all three as
//! unknown options, at 129.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn bare_format_options_and_parents_are_revision_options() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("shortlog-bare-pretty", stock);
    for side in [&s, &z] {
        side.git(&["merge", "-q", "--no-edit", "-s", "ours", "side"]);
    }
    for args in [
        &["shortlog", "--pretty", "HEAD"][..],
        &["shortlog", "--oneline", "HEAD"],
        &["shortlog", "--parents", "HEAD"],
        &["shortlog", "-e", "--parents", "HEAD"],
        &["shortlog", "-s", "--parents", "--no-merges", "HEAD"],
        &["shortlog", "--format=%H", "--pretty", "HEAD"],
        &["shortlog", "--pretty", "--format=%H", "HEAD"],
        &["shortlog", "--format=%H", "--oneline", "HEAD"],
        &["shortlog", "--parents", "HEAD", "--", "a"],
        &["shortlog", "--parents", "--full-history", "HEAD", "--", "a"],
        &["shortlog", "--end-of-options", "--parents", "HEAD"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
