use anyhow::Result;
use std::process::ExitCode;

/// `git ls-tree` — list a tree object. TODO: port via gitoxide (`src/ported`).
pub fn ls_tree(_args: &[String]) -> Result<ExitCode> {
    anyhow::bail!("not yet ported")
}
