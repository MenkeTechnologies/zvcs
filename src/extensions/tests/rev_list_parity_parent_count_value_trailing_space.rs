//! `--min-parents=<n>` / `--max-parents=<n>` read their value with `strtol_i()`: leading
//! whitespace is skipped, trailing text of any kind — a blank included — is refused with
//! `fatal: '<v>': not an integer`. zvcs trimmed both ends.
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
fn only_leading_blanks_are_skipped_in_a_parent_count() {
    let Some((s, z)) = world("rev-list-parents-ws") else { return };
    for args in [
        &["rev-list", "--min-parents=1 ", "main"][..],
        &["rev-list", "--max-parents=0\t", "main"],
        &["rev-list", "--max-parents= 0", "main"],
        &["rev-list", "--min-parents=\n0", "main"],
        &["rev-list", "--min-parents=0x1", "main"],
        &["rev-list", "--max-parents=", "main"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let bad = s.git(&["rev-list", "--min-parents=1 ", "main"]);
    assert_eq!((bad.code, bad.stderr.as_str()), (128, "fatal: '1 ': not an integer\n"));
}
