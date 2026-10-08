//! `git commit -u<mode>` against stock git: `parse_and_validate_options()` hands a mode that is
//! not one of the three names to `git_parse_maybe_bool()`, so `1`, `true` and `0x10` are `normal`
//! and only an unreadable value is `Invalid untracked files mode` — which dies ahead of the
//! pathspec check.
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
fn untracked_mode_is_a_name_or_a_maybe_bool() {
    let Some((s, z)) = both("commit-untracked", |side| side.write("a", "dirty\n")) else { return };
    for mode in ["no", "normal", "all", "1", "0", "true", "false", "0x10", "999999999", "bogus", "n", "ALL", ""] {
        let arg = format!("--untracked-files={mode}");
        for args in [
            vec!["commit", &arg, "-m", "x"],
            vec!["commit", &arg, "no-such-path"],
            vec!["commit", "--dry-run", &arg],
            vec!["commit", &arg],
        ] {
            assert_eq!(z.git(&args), s.git(&args), "{args:?}");
        }
    }
}
