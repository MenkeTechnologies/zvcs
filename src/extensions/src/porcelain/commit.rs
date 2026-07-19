use anyhow::Result;
use std::process::ExitCode;

/// `git commit` — record a commit. TODO: port via gitoxide (`src/ported`).
pub fn commit(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
