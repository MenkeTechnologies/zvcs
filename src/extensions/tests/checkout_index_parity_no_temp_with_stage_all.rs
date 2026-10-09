//! `--temp` is a tri-state in `git checkout-index`: unset, on, or off. Only after
//! every option is parsed does `cmd_checkout_index()` settle the unset case as
//! `checkout_stage == CHECKOUT_ALL` and then refuse an *explicit* `--no-temp`
//! beside `--stage=all` (builtin/checkout-index.c:277-281):
//! `fatal: options '--stage=all' and '--no-temp' cannot be used together`.
//!
//! zvcs set `--temp` as a side effect of `--stage=all`, so an earlier `--no-temp`
//! was overwritten and the command carried on and reported missing entries.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

#[test]
fn explicit_no_temp_refuses_stage_all_in_either_order() {
    let Some(t) = Twin::new("ci-no-temp-stage-all") else { return };
    t.same(&["checkout-index", "--no-temp", "--stage=all", "nosuch"]);
    t.same(&["checkout-index", "--stage=all", "--no-temp", "nosuch"]);
    t.same(&["checkout-index", "--no-temp", "--stage=all", "--stage=all", "-z", "nosuch"]);
}

#[test]
fn the_refusal_precedes_the_argument_mixing_errors() {
    let Some(t) = Twin::new("ci-no-temp-stage-all-mix") else { return };
    t.same(&["checkout-index", "--no-temp", "--stage=all", "-a", "--stdin"]);
    t.same(&["checkout-index", "--no-temp", "--stage=all", "-a", "nosuch"]);
}

#[test]
fn a_later_numeric_stage_or_an_explicit_temp_lifts_the_refusal() {
    let Some(t) = Twin::new("ci-no-temp-stage-all-lift") else { return };
    t.same(&["checkout-index", "--no-temp", "--stage=all", "--stage=2", "nosuch"]);
    t.same(&["checkout-index", "--no-temp", "--stage=2", "nosuch"]);
    t.same(&["checkout-index", "--stage=all", "--temp", "nosuch"]);
    t.same(&["checkout-index", "--stage=all", "--no-temp", "--temp", "nosuch"]);
}
