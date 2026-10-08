//! `git remote -- <word>`.
//!
//! `cmd_remote()` calls `parse_options()` with `PARSE_OPT_SUBCOMMAND_OPTIONAL`,
//! which recognises the subcommand only ahead of a `--`. Whatever follows a `--`
//! is a leftover argument, so even `prune` or `remove` is
//! ``error: unknown subcommand: `prune'`` with the usage block at 129. zvcs
//! dispatched it as the subcommand it names.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_word_after_dashdash_is_never_a_subcommand() {
    let Some(t) = Twin::new("remote-dashdash") else { return };
    for sub in ["prune", "remove", "rm", "show", "update", "add", "nonsense"] {
        t.same(&["remote", "--", sub, "origin"]);
        t.same(&["remote", "--verbose", "--", sub, "origin"]);
    }
}

#[test]
fn dashdash_alone_lists_and_a_subcommand_before_it_still_runs() {
    let Some(t) = Twin::new("remote-dashdash-list") else { return };
    t.same(&["remote", "--"]);
    t.same(&["remote", "-v", "--"]);
    t.same(&["remote", "show", "-n", "origin"]);
}
