//! `--diff-filter=<letters>` names change classes, and `diff_opt_diff_filter()` refuses a
//! letter that is none (`error: unknown change class '<c>' in --diff-filter=<value>`, 129).
//! `diff`, `log` and `show` kept the letters unchecked and ran as if the option were absent.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

#[test]
fn an_unknown_change_class_is_refused_by_diff_log_and_show() {
    let Some((s, z)) = world("diff-filter-class") else { return };
    for args in [
        &["diff", "--diff-filter=0"][..],
        &["diff", "--cached", "--diff-filter=Ad9"],
        &["log", "--diff-filter=0"],
        &["log", "--oneline", "--diff-filter=AZ"],
        &["show", "--diff-filter=0"],
        &["show", "--stat", "--diff-filter=aM!"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 129, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}

#[test]
fn known_classes_still_filter() {
    let Some((s, z)) = world("diff-filter-known") else { return };
    for args in [
        &["log", "--oneline", "--name-status", "--diff-filter=M"][..],
        &["show", "--name-status", "--diff-filter=d"],
        &["diff", "main~2", "--name-status", "--diff-filter=A"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
