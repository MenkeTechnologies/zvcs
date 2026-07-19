use anyhow::Result;
use std::process::ExitCode;

/// `git rm` — remove from index+worktree. TODO: port via gitoxide (`src/ported`).
pub fn rm(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
