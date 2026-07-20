//! Hook management verbs: `zhook <set|unset|show|list|test>`.
//!
//! Hooks are `zvcs.hook` in a repo's merged config; the daemon fires them on
//! ref-change when hooks are enabled (`zvcs.hook` set, or the `zvcs.autohook`
//! master switch — which fires each repo's *own* local hook). These verbs manage
//! and test them without hand-editing config.

use anyhow::{anyhow, bail, Result};
use std::io::IsTerminal;
use std::process::{Command, ExitCode};

pub fn zhook(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("set") => set(&args[1..]),
        Some("unset") => unset(),
        Some("show") => show(),
        Some("list") => list(),
        Some("test") | Some("run") => test(),
        _ => bail!("usage: git zhook <set <command>|unset|show|list|test>"),
    }
}

/// The zvcs `git` binary, to shell `git config` (writes go through porcelain).
fn exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))
}

/// `git zhook set <command>` — set this repo's `zvcs.hook`.
fn set(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        bail!("usage: git zhook set <command>");
    }
    let cmd = args.join(" ");
    let ok = Command::new(exe()?)
        .args(["config", "zvcs.hook", &cmd])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("hook set: {cmd}");
        // Remind if hook-firing isn't enabled anywhere resolvable here.
        if let Ok(repo) = gix::discover(".") {
            if !crate::config::ZvcsConfig::load(&repo).hooks_enabled() {
                eprintln!("note: enable firing with `git config --global zvcs.autohook true` (or set zvcs.hook)");
            }
        }
        Ok(ExitCode::SUCCESS)
    } else {
        bail!("failed to set zvcs.hook");
    }
}

/// `git zhook unset` — remove this repo's `zvcs.hook`.
fn unset() -> Result<ExitCode> {
    let _ = Command::new(exe()?).args(["config", "--unset", "zvcs.hook"]).status();
    println!("hook unset");
    Ok(ExitCode::SUCCESS)
}

/// `git zhook show` — the current repo's effective (merged) hook.
fn show() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    match repo.config_snapshot().string("zvcs.hook") {
        Some(c) => println!("{c}"),
        None => {
            if std::io::stdout().is_terminal() {
                eprintln!("no zvcs.hook set here");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zhook list` — every indexed repo that has a hook (pipe-clean).
fn list() -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    for r in crate::db::list_repos(&conn)? {
        if let Ok(repo) = gix::open(&r.git_dir) {
            if let Some(hook) = repo.config_snapshot().string("zvcs.hook") {
                let path = r.workdir.unwrap_or(r.git_dir);
                println!("{path}\t{hook}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zhook test` — fire the current repo's hook once (typed event from the
/// latest reflog), for testing.
fn test() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let git_dir = repo.git_dir().to_path_buf();
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("a working tree is required"))?
        .to_path_buf();
    if crate::superset::hooks::hook_for(&workdir).is_none() {
        bail!("no zvcs.hook set here (set one with `git zhook set <command>`)");
    }
    crate::superset::hooks::run(&git_dir, &workdir);
    Ok(ExitCode::SUCCESS)
}
