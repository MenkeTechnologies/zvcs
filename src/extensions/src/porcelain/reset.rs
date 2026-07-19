use anyhow::Result;
use std::process::ExitCode;

/// `git reset` — move HEAD / unstage. TODO: port via gitoxide (`src/ported`).
pub fn reset(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
