use anyhow::Result;
use std::process::ExitCode;

/// `git config` — get/set config. TODO: port via gitoxide (`src/ported`).
pub fn config(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
