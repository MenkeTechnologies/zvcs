//! `cmd_unpack_objects()` calls `repo_config(the_repository, git_default_config, NULL)` before
//! `show_usage_if_asked()` and the argument loop (builtin/unpack-objects.c:624-628), so a value
//! the default callback refuses is fatal at 128 ahead of every usage error and of `-h`.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn default_callback_values_precede_usage_errors_and_help() {
    let Some(t) = Twins::new("unpack-default") else { return };
    for (key, value) in [
        ("push.default", "off"),
        ("core.autocrlf", "%H"),
        ("core.quotePath", "warn"),
    ] {
        for args in [
            &["unpack-objects", "-r"][..],
            &["unpack-objects", "-r", "does-not-exist"][..],
            &["unpack-objects", "--bogus"][..],
            &["unpack-objects", "-h"][..],
        ] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn a_readable_configuration_still_reaches_the_usage_error() {
    let Some(t) = Twins::new("unpack-default-ok") else { return };
    let (stock, _) = t.same_with("core.quotePath", "false", &["unpack-objects", "-r", "does-not-exist"]);
    assert_eq!(stock.code, 129, "{stock:?}");
}
