use anyhow::Result;
use std::process::ExitCode;

/// `git push` — upload objects+refs. TODO: port via gitoxide (`src/ported`).
pub fn push(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
