//! `cmd_cat_file()` runs `parse_options()` before any repository setting is read, so an
//! option it refuses (a cmdmode conflict, an unknown option, a missing value) is the 129 no
//! configuration value can pre-empt. zvcs ran its settings gate first and died at 128 on
//! `index.sparse = <overflowing number>`.
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
fn refused_options_win_over_a_bad_setting_and_valid_ones_still_hit_it() {
    let Some((s, z)) = world("cat-file-options-first") else { return };
    let env = [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "index.sparse"), ("GIT_CONFIG_VALUE_0", "99999999999999999999")];
    for args in [
        &["cat-file", "-s", "-t", "HEAD"][..],
        &["cat-file", "-p", "-e", "HEAD"],
        &["cat-file", "--textconv", "--batch-all-objects"],
        &["cat-file", "--path"],
        &["cat-file", "--batch", "--batch-check"],
        &["cat-file", "--bogus"],
        &["cat-file", "-x"],
        // Parses cleanly, so the settings block is reached first.
        &["cat-file", "-s", "HEAD"],
        &["cat-file", "-t"],
        &["cat-file", "--batch-check", "HEAD"],
    ] {
        let want = s.git_env(&env, args);
        assert_eq!(z.git_env(&env, args), want, "{args:?}");
    }
    let refused = s.git_env(&env, &["cat-file", "-s", "-t", "HEAD"]);
    assert_eq!(
        (refused.code, refused.stderr.as_str()),
        (129, "error: options '-t' and '-s' cannot be used together\n")
    );
    let reached = s.git_env(&env, &["cat-file", "-s", "HEAD"]);
    assert_eq!(reached.code, 128, "{reached:?}");
}
