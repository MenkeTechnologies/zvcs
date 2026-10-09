//! `cmd_cat_file()` reads `git_default_config` first, then runs `parse_options()`, and only then
//! reaches `prepare_repo_settings()`. So an option it refuses (a cmdmode conflict, an unknown
//! option, a missing value) is the 129 a bad settings value (`index.sparse = <overflowing
//! number>`) cannot pre-empt, while a bad default-config value (`core.bare = auto`) still
//! precedes it. zvcs ran its settings gate first and died at 128 on the former.
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

#[test]
fn a_bad_default_config_value_still_precedes_the_option_parse() {
    let Some((s, z)) = world("cat-file-default-config-first") else { return };
    for side in [&s, &z] {
        std::fs::write(side.root.join("global.config"), "[core]\n\tbare = auto\n").unwrap();
    }
    let run = |side: &Side, args: &[&str]| {
        let global = side.root.join("global.config");
        side.git_env(&[("GIT_CONFIG_GLOBAL", global.to_str().unwrap())], args)
    };
    for args in [
        &["cat-file", "-s", "-t", "HEAD"][..],
        &["cat-file", "--batch-command", "-p", "--batch-command", "--batch-check"],
        &["cat-file", "--bogus"],
        &["cat-file", "-h"],
    ] {
        let want = run(&s, args);
        assert_eq!(want.code, 128, "{args:?}: {want:?}");
        assert_eq!(run(&z, args), want, "{args:?}");
    }
}
