//! `stripspace -s` and `-c` run `setup_git_directory_gently()` and then
//! `repo_config(the_repository, git_default_config, NULL)` (builtin/stripspace.c:56-58), so
//! any value the default callback refuses — not only `core.commentChar` — is fatal at 128 for
//! those two modes. The default mode never reads configuration and ignores the same value.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn comment_modes_refuse_values_the_default_callback_refuses() {
    let Some(t) = Twins::new("stripspace-default") else { return };
    for (key, value) in [
        ("core.quotePath", "input"),
        ("push.default", "bogus"),
        ("core.autocrlf", "%H"),
        ("core.commentChar", ""),
    ] {
        for args in [
            &["stripspace", "-s"][..],
            &["stripspace", "-c"][..],
            &["stripspace", "--strip-comments"][..],
        ] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn the_default_mode_never_reads_the_configuration() {
    let Some(t) = Twins::new("stripspace-default-mode") else { return };
    let (stock, _) = t.same_with("push.default", "bogus", &["stripspace"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}

#[test]
fn a_usage_error_precedes_the_configuration() {
    let Some(t) = Twins::new("stripspace-usage") else { return };
    let (stock, _) = t.same_with("push.default", "bogus", &["stripspace", "-s", "extra"]);
    assert_eq!(stock.code, 129, "{stock:?}");
}
