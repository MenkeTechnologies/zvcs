//! The zvcs superset — coordination verbs stock git structurally cannot have.
//!
//! This is the world's-first layer: not "git in Rust" (gitoxide already is that),
//! but a VCS that solves the N-concurrent-agent + submodule pain of the meta repo:
//!
//!   * [`zdaemon`] — per-repo coordinator. A FIFO queue/barrier replaces git's
//!     `index.lock` flock, so a contended writer waits its turn instead of failing.
//!     Hosts background reconcile threads.
//!   * [`zsync`]   — reconcile every submodule to its tracked mainline
//!     (`origin/main`/`origin/master`) and keep it *attached* — detached HEAD
//!     never happens. Fast-forward only; never touches a dirty worktree.
//!   * [`zbump`]   — forward-only submodule gitlink bumps: stage a submodule
//!     pointer only when the new SHA is a descendant of the recorded one.
//!
//! All three are stubs today; the dispatch table and the boundaries are the
//! contract. Implementations land next.

use anyhow::Result;
use std::process::ExitCode;

pub fn zdaemon(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("coordinator not yet implemented")
}

pub fn zsync(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("submodule reconcile not yet implemented")
}

pub fn zbump(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("forward-only pointer bump not yet implemented")
}
