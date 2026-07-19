use anyhow::Result;
use std::process::ExitCode;

/// `git merge` — merge (fast-forward). TODO: port via gitoxide (`src/ported`).
pub fn merge(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
