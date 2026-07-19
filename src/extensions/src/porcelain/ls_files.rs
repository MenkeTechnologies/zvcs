use anyhow::Result;
use std::process::ExitCode;

/// `git ls-files` — list index entries. TODO: port via gitoxide (`src/ported`).
pub fn ls_files(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
