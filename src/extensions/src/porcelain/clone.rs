use anyhow::Result;
use std::process::ExitCode;

/// `git clone` — clone a repository. TODO: port via gitoxide (`src/ported`).
pub fn clone(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
