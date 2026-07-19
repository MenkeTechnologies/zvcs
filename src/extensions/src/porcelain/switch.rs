use anyhow::Result;
use std::process::ExitCode;

/// `git switch` — switch branches. TODO: port via gitoxide (`src/ported`).
pub fn switch(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
