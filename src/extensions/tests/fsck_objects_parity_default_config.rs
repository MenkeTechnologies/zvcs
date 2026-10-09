//! `git fsck-objects` is `git fsck` under its historical name (both map to `cmd_fsck`), and
//! `cmd_fsck()` runs `repo_config(repo, git_fsck_config, …)` — which ends in
//! `git_default_config()` — right after its option parse (builtin/fsck.c:1051). A `core.*` or
//! `push.*` value the default callback refuses is therefore fatal for the synonym exactly as it
//! is for `fsck`, before any object is walked and ahead of the operand check. `-h` is answered
//! before the config is read. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn default_callback_values_are_refused_by_the_synonym() {
    let Some(t) = Twins::new("fsck-objects-default") else { return };
    for (key, value) in [
        ("core.autocrlf", "%H"),
        ("core.compression", "-2"),
        ("core.quotePath", "warn"),
        ("push.default", "bogus"),
    ] {
        for args in [
            &["fsck-objects"][..],
            &["fsck-objects", "--full", "v1", "side"][..],
            &["fsck-objects", "--unreachable", "--no-references", "--", "side"][..],
        ] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn help_is_answered_before_the_configuration() {
    let Some(t) = Twins::new("fsck-objects-help") else { return };
    let (stock, _) = t.same_with("core.autocrlf", "%H", &["fsck-objects", "-h"]);
    assert!(stock.stdout.starts_with("usage: git fsck"), "{stock:?}");
}

#[test]
fn a_readable_configuration_still_checks() {
    let Some(t) = Twins::new("fsck-objects-ok") else { return };
    let (stock, _) = t.same_with("core.quotePath", "false", &["fsck-objects"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}
