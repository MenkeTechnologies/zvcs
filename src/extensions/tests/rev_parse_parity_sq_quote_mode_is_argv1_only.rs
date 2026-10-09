//! `cmd_rev_parse()` answers `--parseopt` and `--sq-quote` before `setup_git_directory()`
//! only as `argv[1]`. Anywhere later they are ordinary arguments of a command that has
//! already set up, so a repository whose configuration setup refuses (`core.bare = input`)
//! ends it first. zvcs skipped the setup gates when either appeared anywhere.
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
fn a_later_sq_quote_does_not_skip_repository_setup() {
    let Some((s, z)) = world("rev-parse-sq-later") else { return };
    for side in [&s, &z] {
        side.write(".git/config", "[core]\n\tbare = input\n");
    }
    for args in [
        &["rev-parse", "--shared-index-path", "--sq-quote", "--show-toplevel"][..],
        &["rev-parse", "HEAD", "--sq-quote"],
        &["rev-parse", "HEAD", "--parseopt"],
        &["rev-parse", "--sq-quote", "a b"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let later = s.git(&["rev-parse", "HEAD", "--sq-quote"]);
    assert_eq!(later.code, 128, "{later:?}");
    let first = s.git(&["rev-parse", "--sq-quote", "a b"]);
    assert_eq!((first.code, first.stdout.as_str()), (0, " 'a b'\n"));
}
