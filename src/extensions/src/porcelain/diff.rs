use anyhow::Result;
use std::process::ExitCode;

/// `git diff` — changes between trees/index/worktree. TODO: port via gitoxide (`src/ported`).
pub fn diff(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
