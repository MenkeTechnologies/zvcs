use anyhow::Result;
use std::process::ExitCode;

/// `git branch` — list/create/delete branches. TODO: port via gitoxide (`src/ported`).
pub fn branch(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
