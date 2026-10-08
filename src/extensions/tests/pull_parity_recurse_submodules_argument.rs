//! `git pull --recurse-submodules=<value>` parses its value itself.
//!
//! `pull` registers `option_fetch_parse_recurse_submodules`, which is
//! `parse_fetch_recurse()`: `git_parse_maybe_bool()`'s whole grammar (words and
//! every integer `git_parse_int()` reads, an empty value being false), then
//! `on-demand`, then `bad recurse-submodules argument`. zvcs accepted a short
//! list of words and refused `0`, `1`, ` 1` and the empty value at 128, where git
//! carries on into the fetch.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn the_value_follows_the_maybe_bool_grammar() {
    let Some(t) = Twin::new("pull-recurse") else { return };
    // A branch with no upstream ends the pull right after the fetch, so every
    // accepted spelling reaches the same place and a refused one never does.
    t.prepare(&["checkout", "-q", "-b", "topic"]);
    for value in ["", "0", "1", " 1", "2", "yes", "no", "on", "off", "true", "false", "on-demand", "ON-DEMAND", "x", "1x", "0x10"] {
        let arg = format!("--recurse-submodules={value}");
        t.same(&["pull", "--no-progress", &arg]);
    }
    t.same(&["pull", "--no-progress", "--recurse-submodules"]);
    t.same(&["pull", "--no-progress", "--no-recurse-submodules"]);
}
