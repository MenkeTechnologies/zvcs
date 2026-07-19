use anyhow::Result;
use std::process::ExitCode;

/// `git tag` — list/create/delete tags. TODO: port via gitoxide (`src/ported`).
pub fn tag(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
