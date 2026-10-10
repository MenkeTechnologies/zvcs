//! A zero `--geometric` factor is no `--geometric` at all.
//!
//! Every geometric conflict check in `cmd_repack()` tests `geometry.split_factor`
//! (builtin/repack.c:267), the integer the option stores, not whether the option appeared. So
//! `repack -a --geometric=0` and `repack -a -g0` are ordinary all-into-one repacks, and
//! `--geometric=0 --filter=…` is not the `--stdin-packs` refusal. zvcs took the option's presence
//! for the factor and died with `options '--geometric' and '-A/-a' cannot be used together`.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_zero_factor_does_not_conflict_with_all_into_one() {
    let Some(t) = Twin::new("repack-geo-zero") else { return };
    for args in [
        &["repack", "-a", "-d", "-q", "--geometric=0"][..],
        &["repack", "-a", "-q", "-g0"],
        &["repack", "-a", "-q", "-g", "0"],
    ] {
        let (stock, zvcs) = t.run_in("work", args);
        assert_eq!(stock.code, 0, "{args:?}: {stock:?}");
        assert_eq!(zvcs, stock, "{args:?}");
    }
}

#[test]
fn a_nonzero_factor_still_conflicts() {
    let Some(t) = Twin::new("repack-geo-nonzero") else { return };
    for args in [&["repack", "-a", "--geometric=2"][..], &["repack", "-a", "-g2"]] {
        let (stock, zvcs) = t.run_in("work", args);
        assert_eq!(stock.code, 128, "{args:?}: {stock:?}");
        assert_eq!(zvcs, stock, "{args:?}");
    }
}
