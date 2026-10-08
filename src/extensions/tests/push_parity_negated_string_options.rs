//! `git push --no-repo` and `--no-push-option`.
//!
//! `--repo` is an `OPT_STRING` and `--push-option` an `OPT_STRING_LIST`; their
//! negations reset the string to NULL and empty the list. zvcs had no arm for
//! either and ended the command with ``zvcs: push: unsupported option`` at 1,
//! where git carries on (here into `No configured push destination.`).
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn the_negations_reset_the_value() {
    let Some(t) = Twin::new("push-neg") else { return };
    t.same_in("up", &["push", "--no-push-option", "--atomic"]);
    t.same_in("up", &["push", "--no-repo"]);
    t.same_in("up", &["push", "--repo=nowhere", "--no-repo"]);
    t.same_in("up", &["push", "-o", "x", "--no-push-option", "--dry-run"]);
}

#[test]
fn a_repo_the_negation_cleared_falls_back_to_the_default() {
    let Some(t) = Twin::new("push-neg-default") else { return };
    t.same(&["push", "--dry-run", "--repo=up", "--no-repo"]);
    t.same(&["push", "--dry-run", "-o", "x", "--no-push-option", "origin", "main"]);
}
