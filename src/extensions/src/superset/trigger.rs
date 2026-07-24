//! Directory triggers — `git ztrigger <DIR> <command>` and `git zwatch <DIR>`.
//!
//! A general "watch this directory, run this command on any change" mechanism,
//! independent of git: the DIR does **not** have to be a repository. Triggers are
//! keyed by canonical directory path in the index's `triggers` table; the daemon
//! watches each path recursively and runs its command the instant any file under
//! it is created, modified, or removed. The command runs via `sh -c` with the
//! watched directory as cwd and `ZVCS_DIR` in the environment.
//!
//! `zwatch DIR` is the command-less form — it installs a trigger that just logs
//! each change to the daemon log, so you can watch a directory's activity without
//! writing a command. (For a repo's *git* hook — ref-change semantics stored in
//! `.git/config` — use `git zhook` instead.)

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::process::ExitCode;

/// Resolve a directory argument to its canonical path, requiring that it exists
/// and is a directory. Any directory is valid — no git repository is required.
fn resolve_dir(dir: &str) -> Result<PathBuf> {
    let canon = PathBuf::from(dir)
        .canonicalize()
        .map_err(|e| anyhow!("{dir}: {e}"))?;
    if !canon.is_dir() {
        bail!("{dir}: not a directory");
    }
    Ok(canon)
}

/// `git ztrigger <DIR <command>... | list | rm DIR | test DIR>`.
pub fn ztrigger(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None | Some("list") => list(),
        Some("rm") | Some("unset") | Some("remove") => rm(args.get(1).map(String::as_str)),
        Some("test") | Some("run") => test(args.get(1).map(String::as_str)),
        _ => set(&args[0], &args[1..].join(" ")),
    }
}

/// `git ztrigger DIR CMD...` — run `CMD` on any file change under DIR.
fn set(dir: &str, cmd: &str) -> Result<ExitCode> {
    if cmd.trim().is_empty() {
        bail!("usage: git ztrigger <DIR> <command>...");
    }
    let dir = resolve_dir(dir)?;
    let conn = crate::db::open_rw()?;
    crate::db::set_trigger(&conn, &dir, cmd)?;
    crate::superset::hooks::reload_daemon();
    println!("trigger set: {} -> {cmd}", dir.display());
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger list` — every directory trigger (path + command).
fn list() -> Result<ExitCode> {
    let Ok(conn) = crate::db::open_ro() else {
        return Ok(ExitCode::SUCCESS);
    };
    for (path, cmd) in crate::db::list_triggers(&conn)? {
        println!("{path}\t{cmd}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger rm DIR` — remove DIR's trigger.
fn rm(dir: Option<&str>) -> Result<ExitCode> {
    let dir = resolve_dir(dir.unwrap_or("."))?;
    let conn = crate::db::open_rw()?;
    let n = crate::db::remove_trigger(&conn, &dir)?;
    crate::superset::hooks::reload_daemon();
    if n > 0 {
        println!("trigger removed: {}", dir.display());
    } else {
        eprintln!("no trigger set for {}", dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger test DIR` — run DIR's trigger command once now.
fn test(dir: Option<&str>) -> Result<ExitCode> {
    let dir = resolve_dir(dir.unwrap_or("."))?;
    let key = dir.to_string_lossy().into_owned();
    let conn = crate::db::open_ro()?;
    let cmd = crate::db::list_triggers(&conn)?
        .into_iter()
        .find(|(p, _)| *p == key)
        .map(|(_, c)| c);
    let Some(cmd) = cmd else {
        bail!("no trigger set for {} (add one with `git ztrigger {} <command>`)", dir.display(), dir.display());
    };
    crate::superset::hooks::run_command(&dir, &cmd);
    Ok(ExitCode::SUCCESS)
}

/// `git zwatch <DIR | list | rm DIR>` — watch DIR and log each change (a trigger
/// with a built-in logging command). `list`/`rm` share the trigger table.
pub fn zwatch(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None => bail!("usage: git zwatch <DIR> | git zwatch <list|rm DIR>"),
        Some("list") => list(),
        Some("rm") | Some("remove") => rm(args.get(1).map(String::as_str)),
        _ => set(&args[0], "echo \"[zwatch] $ZVCS_DIR changed\""),
    }
}
