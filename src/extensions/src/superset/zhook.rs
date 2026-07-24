//! Hook management verbs: `zhook <set|unset|show|list|test>`.
//!
//! Hooks are `zvcs.hook` in a repo's merged config; the daemon fires them on
//! ref-change when hooks are enabled (`zvcs.hook` set, or the `zvcs.autohook`
//! master switch — which fires each repo's *own* local hook). These verbs manage
//! and test them without hand-editing config.

use anyhow::{anyhow, bail, Result};
use std::io::IsTerminal;
use std::process::ExitCode;

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

/// `git zhook set <command>` — set the current repo's `zvcs.hook`. The
/// DIR-addressed `git ztrigger` is the same operation for an arbitrary path.
fn set(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        bail!("usage: git zhook set <command>");
    }
    let cmd = args.join(" ");
    let (_, workdir) = crate::superset::hooks::resolve(".")?;
    crate::superset::hooks::set_hook(&workdir, &cmd)?;
    println!("hook set: {cmd}");
    // Auto-enable firing so no raw `git config` is ever needed.
    if let Ok(repo) = gix::discover(".") {
        if !crate::config::ZvcsConfig::load(&repo).hooks_enabled() {
            crate::superset::hooks::enable_autohook()?;
            crate::superset::hooks::reload_daemon();
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zhook unset` — remove the current repo's `zvcs.hook`.
fn unset() -> Result<ExitCode> {
    let (_, workdir) = crate::superset::hooks::resolve(".")?;
    crate::superset::hooks::unset_hook(&workdir)?;
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

/// `git zhook list` — every indexed repo that has a hook (pipe-clean). Shares
/// its source with `git ztrigger list`.
fn list() -> Result<ExitCode> {
    for (path, cmd) in crate::superset::hooks::list()? {
        println!("{path}\t{cmd}");
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
