//! `git range-diff` and `format-patch --range-diff` with a parent mark behind a
//! range.
//!
//! handle_revision_arg_1() tries handle_dotdot() first and its parent marks only
//! after (revision.c:2186-2216), and those marks are its own grammar, not the
//! revision parser's: `main..side^@` is no range, because `get_oid_with_context()`
//! cannot resolve `side^@`, and is no parent mark either, because
//! add_parents_only() cannot resolve `main..side`, so the whole word is
//! `bad revision`. is_range_diff_range() therefore answers 128, and the inner
//! log of `format-patch --range-diff=<prev>` dies, leaving the Range-diff block
//! empty. zvcs handed the word to gitoxide's rev-parse, which read a range: the
//! two-range form exited 129 and format-patch rendered a range-diff.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_mark_behind_a_range_is_a_bad_revision() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("range-diff-marks", stock);
    for args in [
        &["range-diff", "main..side^@", "main~1..main"][..],
        &["range-diff", "main..side^!", "main~1..main"],
        &["range-diff", "main..side^-1", "main~1..main"],
        &["range-diff", "main~1..main", "main..side^@"],
        &["range-diff", "side^@", "main"],
        &["range-diff", "main^!", "side^!"],
        &["range-diff", "^main^!", "side"],
        &["range-diff", "main^-0", "side^!"],
        &["range-diff", "main^-3", "side^!"],
        &["range-diff", "main~2", "main", "main..side^@"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn format_patch_range_diff_with_a_mark_behind_a_range() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("format-patch-range-diff-marks", stock);
    for args in [
        &["format-patch", "--stdout", "-1", "--range-diff=main..side^@", "main"][..],
        &["format-patch", "--stdout", "-1", "--range-diff=side^@", "main"],
        &["format-patch", "--stdout", "-1", "--range-diff=main..side^!", "main"],
        &["format-patch", "--stdout", "-1", "--range-diff=side^!", "main"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
