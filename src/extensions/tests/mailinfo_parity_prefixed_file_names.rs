//! `cmd_mailinfo()` hands `mailinfo()` the operands behind the prefix
//! (`prefix_filename()`, builtin/mailinfo.c:111-112), because git has already moved to the top of
//! the work tree. The names it reports — `perror(msg)` when a file cannot be created and
//! `empty patch: '<patch>'` — therefore carry the directory the command was started in.
//!
//! Stock git 2.54 and later crash with SIGSEGV after the failed `fopen()` diagnostic, which
//! this port does not reproduce; the diagnostic itself is compared and the exit status only
//! where stock exits normally. Stock git is the oracle (`support/stock_git.rs`).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

fn in_subdir(t: &Twins, args: &[&str]) -> (config_twins::Outcome, config_twins::Outcome) {
    let home = t.root.join("home");
    let mut outcomes = Vec::new();
    for (side, bin) in [("stock", t.stock_bin), ("zvcs", config_twins::BIN)] {
        let sub = t.root.join(side).join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        outcomes.push(config_twins::run(bin, &sub, &home, args));
    }
    let zvcs = outcomes.pop().unwrap();
    let stock = outcomes.pop().unwrap();
    (stock, zvcs)
}

#[test]
fn an_uncreatable_file_is_reported_behind_the_prefix() {
    let Some(t) = Twins::new("mailinfo-prefix-open") else { return };
    for args in [
        &["mailinfo", "missing/msg", "patch"][..],
        &["mailinfo", "msg", "missing/patch"][..],
    ] {
        let (stock, zvcs) = in_subdir(&t, args);
        assert_eq!(stock.stderr, zvcs.stderr, "{args:?}");
        assert!(stock.stderr.starts_with("sub/missing/"), "{stock:?}");
    }
}

#[test]
fn an_empty_patch_names_the_prefixed_patch_file() {
    let Some(t) = Twins::new("mailinfo-prefix-empty") else { return };
    let (stock, zvcs) = in_subdir(&t, &["mailinfo", "msg", "patch"]);
    assert_eq!(stock, zvcs);
    assert_eq!(stock.code, 1, "{stock:?}");
    assert!(stock.stderr.contains("empty patch: 'sub/patch'"), "{stock:?}");
}
