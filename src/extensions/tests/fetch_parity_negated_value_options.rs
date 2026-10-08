//! The `--no-` spellings of `git fetch`'s value-taking options, and the two
//! checks `--depth` goes through.
//!
//! `builtin_fetch_options[]` gives `--upload-pack`, `--depth`, `--deepen`,
//! `--jobs`, `--shallow-since`, `--shallow-exclude`, `--negotiation-*` and
//! `--negotiate-only` a negation that resets the variable (NULL, 0, or an emptied
//! list). zvcs resolved the names but had no arm for them and refused each as an
//! unknown option at 129.
//!
//! `--depth` is an `OPT_STRING`: `cmd_fetch()` judges it with `atoi() < 1` after
//! the combination checks, and the transport later reads it with
//! `strtol(value, &end, 0)` once it exists. So ` 1` is accepted, `1x` passes the
//! first check and dies in the transport, and a repository with no remote never
//! reaches the transport at all.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn negated_value_options_are_accepted() {
    let Some(t) = Twin::new("fetch-neg") else { return };
    for neg in [
        "--no-upload-pack",
        "--no-depth",
        "--no-deepen",
        "--no-jobs",
        "--no-shallow-since",
        "--no-shallow-exclude",
        "--no-negotiation-restrict",
        "--no-negotiation-tip",
        "--no-negotiation-include",
        "--no-negotiate-only",
    ] {
        t.same(&["fetch", neg, "--dry-run", "origin"]);
    }
}

#[test]
fn a_negation_cancels_the_option_before_it() {
    let Some(t) = Twin::new("fetch-neg-cancel") else { return };
    t.same(&["fetch", "--depth=0", "--no-depth", "origin"]);
    t.same(&["fetch", "--deepen=1", "--no-deepen", "origin"]);
    t.same(&["fetch", "--upload-pack=/nonexistent/x", "--no-upload-pack", "origin"]);
}

#[test]
fn depth_is_judged_by_atoi_then_by_the_transport() {
    let Some(t) = Twin::new("fetch-depth") else { return };
    for depth in [" 1", "+1", "0", "-1", "abc", "", "1x", "0x1", "010"] {
        let arg = format!("--depth={depth}");
        t.same(&["fetch", &arg, "origin"]);
        // The first check is `cmd_fetch()`'s, ahead of any remote lookup.
        t.same(&["fetch", &arg, "no-such-remote"]);
    }
}

#[test]
fn a_depth_the_transport_refuses_needs_a_transport() {
    let Some(t) = Twin::new("fetch-depth-transport") else { return };
    // No remote configured: nothing opens a transport, so nothing reads the depth.
    t.same_in("up", &["fetch", "--depth=1x"]);
    t.same_in("up", &["fetch", "--depth=1x", "--all"]);
    t.same(&["fetch", "--depth=1x", "--dry-run", "origin"]);
}
