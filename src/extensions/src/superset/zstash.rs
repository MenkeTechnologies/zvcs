//! Tree-wide stash: `zstash`, `zunstash`, `zstashes`.
//!
//! `git stash` is per-repo and does not stash a submodule's dirty state through
//! the parent. `zstash [<name>]` walks the current repo + every nested submodule,
//! stashes each dirty one (reusing the faithful porcelain `git stash push`), and
//! records the set under `<name>` (default `wip`). `zunstash [<name>]` pops them
//! back (LIFO). `zstashes` lists them. Complements `zsnapshot` (committed HEADs);
//! this parks *uncommitted* work across the whole tree as one unit.
//!
//! Boundary: zvcs's `git stash pop` applies only onto an unchanged HEAD (3-way
//! apply is not ported yet). So `zunstash` restores WIP onto the same commits it
//! was stashed on; a repo whose HEAD moved in between is reported and its stash
//! kept intact (never lost).

use anyhow::{anyhow, bail, Result};
use std::path::Path;
use std::process::{Command, ExitCode};

fn stash_name(args: &[String]) -> String {
    args.iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "wip".to_string())
}

/// `git zstash [<name>]` — stash every dirty repo in the tree under `<name>`.
pub fn zstash(args: &[String]) -> Result<ExitCode> {
    let name = stash_name(args);
    let repo = gix::discover(".")?;
    let conn = crate::db::open_rw()?;
    crate::db::stash_begin(&conn, &name)?;
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))?;
    let mut n = 0usize;
    walk_stash(&repo, &name, &conn, &exe, &mut n)?;
    println!("stashed {n} repo(s) as '{name}'");
    Ok(ExitCode::SUCCESS)
}

fn walk_stash(
    repo: &gix::Repository,
    name: &str,
    conn: &rusqlite::Connection,
    exe: &Path,
    n: &mut usize,
) -> Result<()> {
    if repo.is_dirty().unwrap_or(false) {
        if let Some(wd) = repo.workdir() {
            let out = Command::new(exe)
                .args(["stash", "push", "-m", &format!("zstash:{name}")])
                .current_dir(wd)
                .output();
            if out.map(|o| o.status.success()).unwrap_or(false) {
                let git_dir = repo
                    .git_dir()
                    .canonicalize()
                    .unwrap_or_else(|_| repo.git_dir().to_path_buf());
                let wdc = wd.canonicalize().unwrap_or_else(|_| wd.to_path_buf());
                crate::db::stash_add(conn, name, &git_dir.to_string_lossy(), &wdc.to_string_lossy())?;
                *n += 1;
            }
        }
    }
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                walk_stash(&sub, name, conn, exe, n)?;
            }
        }
    }
    Ok(())
}

/// `git zunstash [<name>]` — pop the tree-wide stash back (LIFO). A repo whose
/// pop fails (e.g. HEAD moved → needs 3-way) is reported and kept.
pub fn zunstash(args: &[String]) -> Result<ExitCode> {
    let name = stash_name(args);
    let conn = crate::db::open_rw()?;
    let entries = crate::db::stash_entries(&conn, &name)?;
    if entries.is_empty() {
        bail!("no stash named '{name}'");
    }
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))?;
    let mut popped = 0usize;
    let mut kept = 0usize;
    for wd in &entries {
        let out = Command::new(&exe).args(["stash", "pop"]).current_dir(wd).output();
        match out {
            Ok(o) if o.status.success() => {
                crate::db::stash_remove_entry(&conn, &name, wd)?;
                popped += 1;
            }
            other => {
                let err = other
                    .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                    .unwrap_or_default();
                println!("{wd}: pop kept ({err})");
                kept += 1;
            }
        }
    }
    println!("restored {popped} repo(s) from '{name}', {kept} kept");
    Ok(if kept > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// `git zstashes` — list tree-wide stashes and their repo counts.
pub fn zstashes(_args: &[String]) -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    for (name, count) in crate::db::list_stashes(&conn)? {
        println!("{name}\t{count}");
    }
    Ok(ExitCode::SUCCESS)
}
