use anyhow::Result;
use std::process::ExitCode;

/// `git fetch` — download objects+refs. TODO: port via gitoxide (`src/ported`).
pub fn fetch(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
