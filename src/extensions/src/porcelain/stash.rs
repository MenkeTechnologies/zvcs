use anyhow::Result;
use std::process::ExitCode;

/// `git stash` — stash/unstash changes. TODO: port via gitoxide (`src/ported`).
pub fn stash(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
