//! `git commit` against stock git: `prepare_to_commit()` reads `commit.template` into the message
//! buffer before the `!committable` guard, and `--no-template` clears the option but not the
//! configured path, so an unreadable template dies even when there is nothing to commit.
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
fn an_unreadable_template_dies_before_nothing_to_commit() {
    let Some((s, z)) = both("commit-template", |side| {
        side.git(&["config", "commit.template", "no-such-template"]);
        side.write("a", "dirty\n");
    }) else {
        return;
    };
    for args in [
        &["commit"][..],
        &["commit", "--no-template"],
        &["commit", "-m", "x"],
        &["commit", "--no-template", "-m", "x"],
        &["commit", "-t", "other-missing"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
