use anyhow::Result;
use std::process::ExitCode;

/// `git cat-file` — object type/size/content. TODO: port via gitoxide (`src/ported`).
pub fn cat_file(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
