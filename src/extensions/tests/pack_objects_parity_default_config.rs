//! `git pack-objects` ends its `git_pack_config()` callback in `git_default_config()`, so a
//! `core.*`, `push.*` or `advice.*` value the default callback refuses is fatal before the
//! command reads a single object id — `-h` included, because the config is read ahead of the
//! option parse. The repository settings block still reports first when both are unreadable.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn default_callback_values_are_refused() {
    let Some(t) = Twins::new("pack-default") else { return };
    for (key, value) in [
        ("core.quotePath", "warn"),
        ("core.abbrev", "true"),
        ("core.abbrev", "0"),
        ("core.autocrlf", "%H"),
        ("push.default", "bogus"),
        ("advice.statusHints", "auto"),
    ] {
        for args in [&["pack-objects", "--stdout"][..], &["pack-objects", "-h"][..]] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn a_readable_configuration_still_packs() {
    let Some(t) = Twins::new("pack-default-ok") else { return };
    let (stock, _) = t.same_with("core.quotePath", "false", &["pack-objects", "-h"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}

#[test]
fn the_settings_block_reports_before_the_default_callback() {
    let Some(t) = Twins::new("pack-default-order") else { return };
    t.set_config(&[("core.quotePath", "warn"), ("core.packedGitLimit", "bogus")]);
    let home = t.root.join("home");
    let stock = config_twins::run(t.stock_bin, &t.root.join("stock"), &home, &["pack-objects", "--stdout"]);
    let zvcs = config_twins::run(config_twins::BIN, &t.root.join("zvcs"), &home, &["pack-objects", "--stdout"]);
    assert_eq!(stock, zvcs);
    assert!(stock.stderr.contains("core.packedgitlimit"), "{stock:?}");
}
