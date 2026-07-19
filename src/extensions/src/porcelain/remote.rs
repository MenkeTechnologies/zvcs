use anyhow::Result;
use std::process::ExitCode;

/// `git remote` — manage remotes. TODO: port via gitoxide (`src/ported`).
pub fn remote(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
