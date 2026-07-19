use anyhow::Result;
use std::process::ExitCode;

/// `git rev-list` — list commits. TODO: port via gitoxide (`src/ported`).
pub fn rev_list(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
