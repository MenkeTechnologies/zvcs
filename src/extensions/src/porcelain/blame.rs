use anyhow::Result;
use std::process::ExitCode;

/// `git blame` — line-by-line authorship. TODO: port via gitoxide (`src/ported`).
pub fn blame(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
