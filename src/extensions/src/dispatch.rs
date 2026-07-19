//! Subcommand routing for the shadow `git` binary.
//!
//! Two namespaces share one dispatch table:
//!   * **superset** verbs (`z*`) — the novel coordination layer, [`superset`].
//!   * **git-compat** porcelain — stock git subcommands served via gitoxide,
//!     ported incrementally, [`porcelain`].

use crate::{porcelain, superset};
use anyhow::Result;
use std::process::ExitCode;

/// zvcs-native extension verbs — the superset that stock git does not have.
pub const SUPERSET_VERBS: &[&str] = &["zsync", "zbump", "zdaemon"];

pub fn run(sub: &str, args: &[String]) -> Result<ExitCode> {
    match sub {
        // ---- superset (novel) ----
        "zsync" => superset::zsync(args),
        "zbump" => superset::zbump(args),
        "zdaemon" => superset::zdaemon(args),

        // ---- git-compat porcelain (gitoxide-backed) ----
        "rev-parse" => porcelain::rev_parse(args),
        "status" => porcelain::status(args),
        "log" => porcelain::log(args),
        "show" => porcelain::show(args),
        "diff" => porcelain::diff(args),
        "cat-file" => porcelain::cat_file(args),
        "ls-files" => porcelain::ls_files(args),
        "ls-tree" => porcelain::ls_tree(args),
        "rev-list" => porcelain::rev_list(args),
        "blame" => porcelain::blame(args),
        "describe" => porcelain::describe(args),
        "config" => porcelain::config(args),
        "remote" => porcelain::remote(args),
        "branch" => porcelain::branch(args),
        "tag" => porcelain::tag(args),
        "add" => porcelain::add(args),
        "rm" => porcelain::rm(args),
        "mv" => porcelain::mv(args),
        "restore" => porcelain::restore(args),
        "reset" => porcelain::reset(args),
        "commit" => porcelain::commit(args),
        "checkout" => porcelain::checkout(args),
        "switch" => porcelain::switch(args),
        "stash" => porcelain::stash(args),
        "merge" => porcelain::merge(args),
        "fetch" => porcelain::fetch(args),
        "pull" => porcelain::pull(args),
        "push" => porcelain::push(args),
        "clone" => porcelain::clone(args),
        "init" => porcelain::init(args),

        // Not yet ported. No fallthrough to stock git — this is a from-scratch engine.
        _ => anyhow::bail!("not yet ported (superset verbs: {})", SUPERSET_VERBS.join(", ")),
    }
}
