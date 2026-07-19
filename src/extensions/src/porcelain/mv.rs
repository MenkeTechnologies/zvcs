use anyhow::Result;
use std::process::ExitCode;

/// `git mv` — move/rename tracked path. TODO: port via gitoxide (`src/ported`).
pub fn mv(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
