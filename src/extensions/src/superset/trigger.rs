//! Directory-addressed trigger and watch verbs — `git ztrigger DIR CMD` and
//! `git zwatch DIR`.
//!
//! These are the DIR-addressed front-end to the hook system: unlike `zhook`
//! (which only touches the current repo), they take an explicit path so any repo
//! on the machine can be wired without `cd`-ing into it, and they flip the master
//! switches themselves so **no raw `git config` is ever required** to arm a
//! trigger.
//!
//! `ztrigger DIR CMD` writes DIR's *local* `zvcs.hook`, indexes DIR, turns on the
//! global `zvcs.autohook` switch, and reloads the daemon. Because the daemon
//! runs each repo's *own* local hook and is a no-op for repos without one, only
//! the DIRs you triggered ever fire — a curated set with no extra bookkeeping.
//! `zwatch DIR` is the command-less form: index DIR and turn on `zvcs.autostatus`
//! so the daemon maintains its cached status on every ref-change.

use crate::superset::hooks;
use anyhow::{bail, Result};
use std::process::ExitCode;

/// `git ztrigger <DIR CMD... | list | rm DIR | test DIR>`.
pub fn ztrigger(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None | Some("list") => list(),
        Some("rm") | Some("unset") | Some("remove") => rm(args.get(1).map(String::as_str)),
        Some("test") | Some("run") => test(args.get(1).map(String::as_str)),
        _ => set(args),
    }
}

/// `git ztrigger DIR CMD...` — arm DIR so `CMD` runs on **any** file change in
/// the directory (worktree *and* `.git`), not just ref moves: an armed repo is
/// watched over its whole directory.
fn set(args: &[String]) -> Result<ExitCode> {
    if args.len() < 2 {
        bail!("usage: git ztrigger <DIR> <command>...");
    }
    let dir = &args[0];
    let cmd = args[1..].join(" ");
    let (git_dir, workdir) = hooks::resolve(dir)?;

    hooks::set_hook(&workdir, &cmd)?;
    hooks::index(&git_dir, &workdir)?;
    hooks::enable_autohook()?;
    hooks::reload_daemon();

    println!("trigger set: {} -> {cmd}", workdir.display());
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger list` — every armed repo (path + command).
fn list() -> Result<ExitCode> {
    for (path, cmd) in hooks::list()? {
        println!("{path}\t{cmd}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger rm DIR` — disarm DIR (drop its local hook).
fn rm(dir: Option<&str>) -> Result<ExitCode> {
    let (_, workdir) = hooks::resolve(dir.unwrap_or("."))?;
    hooks::unset_hook(&workdir)?;
    hooks::reload_daemon();
    println!("trigger removed: {}", workdir.display());
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger test DIR` — fire DIR's trigger once now (from its latest reflog
/// event), for testing without waiting for a real change.
fn test(dir: Option<&str>) -> Result<ExitCode> {
    let (git_dir, workdir) = hooks::resolve(dir.unwrap_or("."))?;
    if hooks::hook_for(&workdir).is_none() {
        bail!("no trigger set for {} (add one with `git ztrigger {} <command>`)",
            workdir.display(), workdir.display());
    }
    hooks::run(&git_dir, &workdir);
    Ok(ExitCode::SUCCESS)
}

/// `git zwatch <DIR | list | rm DIR>` — watch DIR (index + status maintenance)
/// without attaching a command.
pub fn zwatch(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None => bail!("usage: git zwatch <DIR> | git zwatch <list|rm DIR>"),
        Some("list") => watch_list(),
        Some("rm") | Some("remove") => watch_rm(args.get(1).map(String::as_str)),
        _ => watch_add(&args[0]),
    }
}

/// `git zwatch DIR` — index DIR and enable status maintenance for watched repos.
fn watch_add(dir: &str) -> Result<ExitCode> {
    let (git_dir, workdir) = hooks::resolve(dir)?;
    hooks::index(&git_dir, &workdir)?;
    hooks::enable_autostatus()?;
    hooks::reload_daemon();
    println!("watching: {}", workdir.display());
    Ok(ExitCode::SUCCESS)
}

/// `git zwatch list` — every indexed repo, flagged when it carries a trigger.
fn watch_list() -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    for r in crate::db::list_repos(&conn)? {
        let armed = gix::open(&r.git_dir)
            .ok()
            .and_then(|repo| repo.config_snapshot().string("zvcs.hook").map(|_| ()))
            .is_some();
        let path = r.workdir.unwrap_or(r.git_dir);
        println!("{path}\t{}", if armed { "trigger" } else { "watch" });
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zwatch rm DIR` — stop watching DIR (drop it from the index).
fn watch_rm(dir: Option<&str>) -> Result<ExitCode> {
    let (git_dir, workdir) = hooks::resolve(dir.unwrap_or("."))?;
    let conn = crate::db::open_rw()?;
    crate::db::remove_repo(&conn, &git_dir)?;
    hooks::reload_daemon();
    println!("unwatched: {}", workdir.display());
    Ok(ExitCode::SUCCESS)
}
