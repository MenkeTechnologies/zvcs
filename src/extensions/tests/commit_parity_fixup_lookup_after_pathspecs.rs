//! `git commit --fixup` / `--squash` against stock git: `prepare_index()` reports unmatched
//! pathspecs before `prepare_to_commit()` looks up the commit, so the pathspec error (exit 1)
//! wins over `could not lookup commit` (128).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn both(label: &str, setup: impl Fn(&Side)) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    setup(&s);
    setup(&z);
    Some((s, z))
}

#[test]
fn pathspec_errors_come_before_the_fixup_lookup() {
    let Some((s, z)) = both("commit-fixup", |side| side.write("a", "dirty\n")) else { return };
    for args in [
        &["commit", "-m", "x", "--fixup=2m", "no-such-path"][..],
        &["commit", "-m", "x", "--fixup=2m", "a"],
        &["commit", "--squash=2m", "no-such-path"],
        &["commit", "--fixup=amend:2m", "no-such-path"],
        &["commit", "-m", "x", "--fixup=HEAD~1", "a"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
