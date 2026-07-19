use anyhow::Result;
use std::process::ExitCode;

/// `git add` — stage worktree changes. TODO: port via gitoxide (`src/ported`).
pub fn add(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
