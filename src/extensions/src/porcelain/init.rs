use anyhow::Result;
use std::process::ExitCode;

/// `git init` — create a repository. TODO: port via gitoxide (`src/ported`).
pub fn init(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
