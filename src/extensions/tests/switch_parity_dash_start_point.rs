//! `parse_branchname_arg()` reads a lone `-` as `@{-1}` (builtin/checkout.c:1320-1321) wherever
//! `switch` takes a reference: the start-point of `-c`/`-C`, the commit of `-d`, and the one
//! `--orphan` refuses. `setup_branch_path()` then expands `@{-N}` into the branch (or id) it names
//! (`strbuf_branchname()`), and that expansion is what the reflogs record.
//!
//! zvcs resolved the dash only for the plain `switch -`, so `switch -c new -`, `switch -C new -- -`
//! and `switch -d -` died `invalid reference: -`; and with the spelling `@{-1}` it logged the
//! shorthand rather than the branch.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

/// `work` has been on `other` and is back on `main`, so `@{-1}` is `other`.
fn back_and_forth(label: &str) -> Option<Twin> {
    let t = Twin::new(label)?;
    t.prepare(&["switch", "-q", "-c", "other"]);
    t.prepare(&["switch", "-q", "main"]);
    Some(t)
}

#[test]
fn a_dash_start_point_is_the_previous_branch() {
    let Some(t) = back_and_forth("switch-dash-create") else { return };
    t.same(&["switch", "-c", "x", "-"]);
    t.same(&["reflog", "show", "x"]);
    t.same(&["rev-parse", "x", "other"]);
}

#[test]
fn it_survives_the_end_of_options_and_the_reset_spelling() {
    let Some(t) = back_and_forth("switch-dash-force") else { return };
    t.same(&["switch", "-C", "y", "--guess", "--", "-"]);
    t.same(&["reflog", "show", "y"]);
    t.same(&["switch", "-C", "y", "--", "main"]);
    t.same(&["switch", "-c", "z", "--no-track", "-"]);
}

#[test]
fn detaching_at_the_dash_logs_the_expanded_name() {
    let Some(t) = back_and_forth("switch-dash-detach") else { return };
    t.same(&["switch", "-d", "-"]);
    t.same(&["reflog", "-n", "3"]);
    t.same(&["switch", "main"]);
    t.same(&["switch", "-d", "@{-1}"]);
    t.same(&["reflog", "-n", "3"]);
}

#[test]
fn orphan_and_a_spelled_out_shorthand() {
    let Some(t) = back_and_forth("switch-dash-orphan") else { return };
    t.same(&["switch", "--orphan", "o", "--", "-"]);
    t.same(&["switch", "-c", "w", "@{-1}"]);
    t.same(&["reflog", "show", "w"]);
}

#[test]
fn a_dash_with_no_previous_branch_names_the_shorthand() {
    let Some(t) = Twin::new("switch-dash-none") else { return };
    t.same(&["switch", "-c", "x", "-"]);
    t.same(&["switch", "-d", "-"]);
    t.same(&["switch", "--orphan", "o", "-"]);
}
