use anyhow::Result;
use std::process::ExitCode;

/// `git restore` — restore worktree/index files. TODO: port via gitoxide (`src/ported`).
pub fn restore(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
