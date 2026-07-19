use anyhow::Result;
use std::process::ExitCode;

/// `git checkout` — switch/restore (legacy). TODO: port via gitoxide (`src/ported`).
pub fn checkout(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
