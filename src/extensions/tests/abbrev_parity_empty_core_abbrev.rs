//! An empty `core.abbrev` is the whole object name.
//!
//! `git_default_core_config()` sends the value through `git_parse_maybe_bool_text()` before the
//! integer reader (environment.c:349-363); that function answers 0 for `""` exactly as for `no`,
//! `off` and `false`, and `0` means `default_abbrev = hexsz`. zvcs treated the empty string as an
//! unreadable value and fell back to the automatic length.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn an_empty_value_prints_whole_names() {
    let Some(t) = Twin::new("abbrev-empty") else { return };
    t.prepare(&["config", "core.abbrev", ""]);
    for args in [
        &["log", "--oneline"][..],
        &["reflog"],
        &["rev-parse", "--short", "HEAD"],
        &["branch", "-v"],
        &["show", "--oneline", "--raw", "--no-patch", "HEAD"],
    ] {
        let (stock, zvcs) = t.run_in("work", args);
        assert_eq!(stock.code, 0, "{args:?}: {stock:?}");
        assert!(stock.stdout.lines().next().is_some_and(|l| l.len() >= 40), "{args:?}: {stock:?}");
        assert_eq!(zvcs, stock, "{args:?}");
    }
}

#[test]
fn a_blank_but_nonempty_value_is_still_refused() {
    let Some(t) = Twin::new("abbrev-blank") else { return };
    t.prepare(&["config", "core.abbrev", " "]);
    let (stock, zvcs) = t.run_in("work", &["log", "--oneline"]);
    assert_eq!(zvcs, stock);
}
