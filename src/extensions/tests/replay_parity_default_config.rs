//! `replay` reads the merge configuration through `init_basic_merge_options()` ->
//! `merge_recursive_config()`, whose chain ends in `git_default_config()`. So a `core.*`,
//! `sparse.*` or `push.*` value that callback refuses ends the command at 128 - but only once
//! the options and operands have been accepted, because the merge options are built after them:
//! a usage error or an unresolvable `--onto` is reported first.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_value_default_config_refuses_ends_a_replay_that_got_as_far_as_merging() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("replay-default-config", stock);
    for value in [
        "core.abbrev=bogus",
        "core.createObject=bogus",
        "core.autocrlf=bogus",
        "core.ignorecase=bogus",
        "sparse.expectFilesOutsideOfPatterns=bogus",
        "push.default=bogus",
        "user.name",
    ] {
        let args = ["-c", value, "replay", "--onto=side", "main~1..main"];
        let want = s.git(&args);
        assert_eq!(want.code, 128, "{value}: {want:?}");
        assert_eq!(z.git(&args), want, "{value}");
    }
}

#[test]
fn the_operand_and_usage_errors_come_before_the_configuration() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("replay-default-config-order", stock);
    for args in [
        &["-c", "core.abbrev=bogus", "replay"][..],
        &["-c", "core.abbrev=bogus", "replay", "--onto=nosuch", "main~1..main"],
        &["-c", "core.abbrev=bogus", "replay", "--bogus"],
    ] {
        let want = s.git(args);
        assert_ne!(want.code, 0, "{args:?}: {want:?}");
        assert!(!want.stderr.contains("core.abbrev"), "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}

