//! `cmd_merge_file()` runs `repo_config(repo, git_xmerge_config, NULL)` before
//! `parse_options()` (builtin/merge-file.c:99), and `git_xmerge_config()` ends in
//! `git_default_config()`. Inside a repository a `core.*` or `push.*` value the default
//! callback refuses is therefore fatal at 128 ahead of every usage and option error, `-h`
//! included; the walk is in parse order, so whichever of it and a bad `merge.conflictStyle`
//! comes first in the configuration is the one reported. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn default_callback_values_precede_option_and_usage_errors() {
    let Some(t) = Twins::new("merge-file-default") else { return };
    for (key, value) in [
        ("core.fileMode", "input"),
        ("core.bare", "always"),
        ("core.autocrlf", "%H"),
        ("push.default", "bogus"),
    ] {
        for args in [
            &["merge-file"][..],
            &["merge-file", "-h"][..],
            &["merge-file", "--object-id", "--zdiff3", "--no-diff3"][..],
            &["merge-file", "-p", "--diff-algorithm=bogus", "file", "file", "file"][..],
        ] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn the_first_refused_value_in_parse_order_is_reported() {
    let Some(t) = Twins::new("merge-file-order") else { return };
    for entries in [
        [("core.fileMode", "input"), ("merge.conflictStyle", "bogus")],
        [("merge.conflictStyle", "bogus"), ("core.fileMode", "input")],
    ] {
        t.set_config(&entries);
        let home = t.root.join("home");
        let stock = config_twins::run(t.stock_bin, &t.root.join("stock"), &home, &["merge-file"]);
        let zvcs = config_twins::run(config_twins::BIN, &t.root.join("zvcs"), &home, &["merge-file"]);
        assert_eq!(stock, zvcs, "{entries:?}");
        assert_eq!(stock.code, 128, "{stock:?}");
    }
}

#[test]
fn a_readable_configuration_still_reaches_the_usage_error() {
    let Some(t) = Twins::new("merge-file-ok") else { return };
    let (stock, _) = t.same_with("core.fileMode", "false", &["merge-file"]);
    assert_eq!(stock.code, 129, "{stock:?}");
}
