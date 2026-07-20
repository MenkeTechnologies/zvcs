//! Tree-wide atomic snapshot/restore: `zsnapshot`, `zrestore`, `zsnapshots`.
//!
//! `zsnapshot <name>` records the exact HEAD commit of the current repo and every
//! (nested) submodule as one named restore point in the db. `zrestore <name>`
//! puts the whole tree back to those commits (`reset --hard` per repo, reusing
//! the faithful porcelain). Doing this by hand across a deep submodule tree is
//! painful in git; here it is one command over the whole tree.
//!
//! Restore is deliberately destructive to *tracked* changes (that is what
//! "restore" means) but `reset --hard` preserves untracked files. A per-repo
//! dirty-skip is not used: a parent always reads as "dirty" once a submodule has
//! moved, so skipping would leave the tree half-restored.

use anyhow::{anyhow, Result};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Collect `(git_dir, workdir, head_sha)` for `repo` and all nested submodules.
fn collect(repo: &gix::Repository, out: &mut Vec<(String, String, String)>) {
    if let Some(id) = repo
        .head()
        .ok()
        .and_then(|mut h| h.try_peel_to_id().ok().flatten())
    {
        let git_dir = repo
            .git_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.git_dir().to_path_buf());
        let workdir = repo
            .workdir()
            .map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()))
            .unwrap_or_else(|| git_dir.clone());
        out.push((
            git_dir.to_string_lossy().into_owned(),
            workdir.to_string_lossy().into_owned(),
            id.detach().to_string(),
        ));
    }
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                collect(&sub, out);
            }
        }
    }
}

/// `git zsnapshot <name>` — record the tree's HEADs as a restore point.
pub fn zsnapshot(args: &[String]) -> Result<ExitCode> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: git zsnapshot <name>"))?;
    let repo = gix::discover(".")?;
    let mut entries = Vec::new();
    collect(&repo, &mut entries);
    if entries.is_empty() {
        anyhow::bail!("nothing to snapshot (unborn HEAD?)");
    }
    let conn = crate::db::open_rw()?;
    crate::db::save_snapshot(&conn, name, &entries)?;
    println!("snapshot '{name}': {} repo(s)", entries.len());
    Ok(ExitCode::SUCCESS)
}

/// `git zrestore <name>` — reset the whole tree back to a snapshot.
pub fn zrestore(args: &[String]) -> Result<ExitCode> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: git zrestore <name>"))?;
    let conn = crate::db::open_ro()?;
    let entries = crate::db::load_snapshot(&conn, name)?;
    if entries.is_empty() {
        anyhow::bail!("no snapshot named '{name}'");
    }
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))?;

    let mut restored = 0usize;
    let mut failed = 0usize;
    for (_git_dir, workdir, sha) in &entries {
        let ok = Command::new(&exe)
            .args(["reset", "--hard", sha])
            .current_dir(PathBuf::from(workdir))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            restored += 1;
        } else {
            println!("{workdir}: restore failed");
            failed += 1;
        }
    }
    println!("restored {restored} repo(s) to '{name}', {failed} failed");
    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// `git zsnapshots` — list snapshot names and their repo counts.
pub fn zsnapshots(_args: &[String]) -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    let interactive = std::io::stdout().is_terminal();
    let rows = crate::db::list_snapshots(&conn)?;
    for (name, count) in &rows {
        println!("{name}\t{count}");
    }
    if interactive && rows.is_empty() {
        eprintln!("no snapshots");
    }
    Ok(ExitCode::SUCCESS)
}
