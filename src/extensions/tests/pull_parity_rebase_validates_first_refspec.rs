//! `pull --rebase` validates its first refspec itself, before the fetch child starts.
//!
//! `cmd_pull()` calls `get_rebase_fork_point()` (builtin/pull.c:1079) ahead of `run_fetch()`; with
//! a refspec that reaches `get_tracking_branch()`, whose `refspec_item_init_or_die()` ends the
//! pull with `die()` — exit 128. Without `--rebase` the malformed refspec is only the fetch
//! child's failure, which `cmd_pull()` flattens to 1. zvcs ended both at 1.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_malformed_first_refspec_dies_in_pull_under_rebase() {
    let Some(t) = Twin::new("pull-rebase-refspec") else { return };
    let (stock, zvcs) = t.run_in("work", &["pull", "-r", "origin", "."]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert_eq!(zvcs, stock);
    t.same(&["pull", "--rebase=interactive", "origin", ".", "main"]);
}

#[test]
fn without_rebase_the_fetch_failure_stays_at_one() {
    let Some(t) = Twin::new("pull-norebase-refspec") else { return };
    let (stock, zvcs) = t.run_in("work", &["pull", "--no-rebase", "origin", "."]);
    assert_eq!(stock.code, 1, "{stock:?}");
    assert_eq!(zvcs, stock);
}
