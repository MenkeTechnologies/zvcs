use anyhow::Result;
use std::process::ExitCode;

/// `git describe` — name a commit from tags. TODO: port via gitoxide (`src/ported`).
pub fn describe(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
