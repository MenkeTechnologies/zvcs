use anyhow::Result;
use std::process::ExitCode;

/// `git status` — working-tree status. TODO: port via gitoxide (`src/ported`).
pub fn status(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
