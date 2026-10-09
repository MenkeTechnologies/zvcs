//! `cmd_fast_import()` runs `git_pack_config()` ahead of its option parse and the stream
//! (builtin/fast-import.c:3852-3877): `pack.depth`, `pack.indexversion`, `pack.packsizelimit`
//! and `fastimport.unpacklimit` / `transfer.unpacklimit` through the targeted readers, then
//! `repo_config(git_default_config)`. A value any of them refuses is fatal at 128 before the
//! first command is read, and the targeted readers report ahead of the default callback.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

#[test]
fn values_the_pack_and_default_callbacks_refuse_are_fatal() {
    let Some(t) = Twins::new("fast-import-config") else { return };
    for (key, value) in [
        ("pack.depth", "bogus"),
        ("pack.indexVersion", "3"),
        ("pack.indexVersion", "bogus"),
        ("pack.packSizeLimit", "bogus"),
        ("fastimport.unpackLimit", "bogus"),
        ("transfer.unpackLimit", "bogus"),
        ("core.quotePath", "input"),
        ("core.precomposeUnicode", "99999999999999999999999999"),
        ("core.abbrev", "0"),
        ("core.compression", "-2"),
        ("push.default", "bogus"),
    ] {
        for args in [&["fast-import", "--quiet", "--done"][..], &["fast-import", "--date-format=raw-permissive"][..]] {
            let (stock, _) = t.same_with(key, value, args);
            assert_eq!(stock.code, 128, "{args:?} with {key}={value}: {stock:?}");
        }
    }
}

#[test]
fn the_targeted_readers_report_ahead_of_the_default_callback() {
    let Some(t) = Twins::new("fast-import-config-order") else { return };
    t.set_config(&[("core.quotePath", "input"), ("pack.depth", "bogus")]);
    let home = t.root.join("home");
    let args = ["fast-import", "--quiet"];
    let stock = config_twins::run(t.stock_bin, &t.root.join("stock"), &home, &args);
    let zvcs = config_twins::run(config_twins::BIN, &t.root.join("zvcs"), &home, &args);
    assert_eq!(stock, zvcs);
    assert!(stock.stderr.contains("pack.depth"), "{stock:?}");
}

#[test]
fn a_readable_configuration_still_imports() {
    let Some(t) = Twins::new("fast-import-config-ok") else { return };
    let (stock, _) = t.same_with("fastimport.unpackLimit", "10", &["fast-import", "--quiet"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}
