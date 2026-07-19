use anyhow::Result;
use std::process::ExitCode;

/// `git show` — show an object. TODO: port via gitoxide (`src/ported`).
pub fn show(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
