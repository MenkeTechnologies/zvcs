use anyhow::Result;
use std::process::ExitCode;

/// `git log` — commit history. TODO: port via gitoxide (`src/ported`).
pub fn log(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
