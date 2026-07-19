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

        // Not yet ported. No fallthrough to stock git — this is a from-scratch engine.
        _ => anyhow::bail!("not yet ported (superset verbs: {})", SUPERSET_VERBS.join(", ")),
    }
}
