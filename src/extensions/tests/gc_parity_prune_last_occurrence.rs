//! `gc` validates only the last `--prune` form on the line.
//!
//! `--prune` is an `OPT_STRING` with `PARSE_OPT_OPTARG` whose `defval` is the "nothing said"
//! sentinel (builtin/gc.c:860-872), and `--no-prune` stores NULL. `cmd_gc()` runs
//! `parse_expiry_date()` once on whatever survives (builtin/gc.c:916), so an unparsable
//! `--prune=<v>` followed by a bare `--prune` or `--no-prune` is never seen. zvcs kept the earlier
//! value and died with `failed to parse prune expiry value`.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_later_bare_or_negated_prune_hides_an_unparsable_earlier_value() {
    let Some(t) = Twin::new("gc-prune-last") else { return };
    for args in [
        &["gc", "--quiet", "--prune==", "--no-prune"][..],
        &["gc", "--quiet", "--prune=true", "--no-prune"],
        &["gc", "--quiet", "--prune==", "--prune"],
    ] {
        let (stock, zvcs) = t.run_in("work", args);
        assert_eq!(stock.code, 0, "{args:?}: {stock:?}");
        assert_eq!(zvcs, stock, "{args:?}");
    }
}

#[test]
fn an_unparsable_last_value_still_dies() {
    let Some(t) = Twin::new("gc-prune-bad") else { return };
    let (stock, zvcs) = t.run_in("work", &["gc", "--quiet", "--no-prune", "--prune=="]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert_eq!(zvcs, stock);
}
