use anyhow::Result;
use std::process::ExitCode;

/// `git pull` — fetch + integrate. TODO: port via gitoxide (`src/ported`).
pub fn pull(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
