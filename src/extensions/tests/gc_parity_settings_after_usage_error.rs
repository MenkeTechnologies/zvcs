//! `cmd_gc()` reads `gc_config()` (which ends in `git_default_config()`) and then runs
//! `parse_options()`, but reaches `prepare_repo_settings()` only with its first object-database
//! access, long after any usage error (builtin/gc.c:899-910). A value only the settings block
//! reads (`index.sparse=bogus`) must therefore leave a usage error at 129, while a value the
//! default callback refuses is fatal at 128 ahead of the usage error, and a below-threshold
//! `gc --auto` returns before it too. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn a_settings_block_value_does_not_pre_empt_a_usage_error() {
    let Some(t) = Twins::new("gc-settings-usage") else { return };
    for args in [
        &["gc", "--keep-largest-pack", "--", "does-not-exist"][..],
        &["gc", "--bogus"][..],
        &["gc", "-h"][..],
    ] {
        let (stock, _) = t.same_with("index.sparse", "bogus", args);
        assert_ne!(stock.code, 128, "{args:?}: {stock:?}");
    }
}

#[test]
fn a_default_callback_value_pre_empts_a_usage_error() {
    let Some(t) = Twins::new("gc-default-usage") else { return };
    let (stock, _) = t.same_with("core.autocrlf", "%H", &["gc", "--", "does-not-exist"]);
    assert_eq!(stock.code, 128, "{stock:?}");
}

#[test]
fn a_below_threshold_auto_run_never_reads_the_settings_block() {
    let Some(t) = Twins::new("gc-settings-run") else { return };
    let (stock, _) = t.same_with("index.sparse", "bogus", &["gc", "--auto", "-q"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}
