//! `cmd_prune_packed()` parses its options and refuses a stray operand
//! (`usage_msg_opt(_("too many arguments"), …)`, exit 129) before
//! `prune_packed_objects()` — the first thing to prepare the repository settings
//! block, whose boolean keys (`core.commitGraph` …) die on a value they cannot
//! read. So a usage error outranks a bad setting; a well-formed command line does
//! not.
//!
//! zvcs prepared the settings in the dispatcher, ahead of the option parse, and
//! answered a bad `core.commitGraph` with 128 where git answers 129.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

#[test]
fn usage_errors_come_before_a_bad_setting() {
    let Some(t) = Twin::new("prune-packed-usage-first") else { return };
    t.prepare(&["config", "core.commitGraph", "\t"]);
    t.same(&["prune-packed", "stray"]);
    t.same(&["prune-packed", "--no-quiet", "stray", "stray"]);
    t.same(&["prune-packed", "--bogus"]);
    t.same(&["prune-packed", "-x"]);
}

#[test]
fn a_well_formed_command_line_meets_the_bad_setting() {
    let Some(t) = Twin::new("prune-packed-setting") else { return };
    t.prepare(&["config", "core.commitGraph", "\t"]);
    t.same(&["prune-packed"]);
    t.same(&["prune-packed", "-n"]);
    t.same(&["prune-packed", "--quiet"]);
}

#[test]
fn a_good_setting_changes_nothing() {
    let Some(t) = Twin::new("prune-packed-good") else { return };
    t.same(&["prune-packed"]);
    t.same(&["prune-packed", "-n"]);
    t.same(&["prune-packed", "stray"]);
}
