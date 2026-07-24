//! Shell-convenience verbs for the `zrepl` console — `zcd`, `zpwd`, `zenv`,
//! `zunset`, `zecho`. (`zls`, the git-aware listing, lives in
//! [`crate::superset::gitls`].)
//!
//! `zrepl` runs each line as `git <line>` in one long-lived process, so a verb
//! that mutates process state persists across lines: `zcd` navigates like a
//! shell's cd, `zenv NAME=VALUE` sets a variable every later `git` line sees, and
//! `zunset` clears one. `zpwd` and `zecho` round out the basics. Outside the
//! console these still run, but the mutating ones (`zcd`, `zenv`, `zunset`) only
//! affect this process (they cannot change the parent shell), so they are aimed
//! at the interactive console.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// `git zcd [<dir>]` — change the working directory. No argument goes to `$HOME`;
/// `-` goes to `$OLDPWD` (the previous directory). A leading `~` is expanded to
/// `$HOME`. `OLDPWD`/`PWD` are updated so a subsequent `zcd -` works, exactly as
/// a shell's `cd` does. Nothing is printed on success (shell-like).
pub fn zcd(args: &[String]) -> Result<ExitCode> {
    let old = std::env::current_dir().ok();
    let target: PathBuf = match args.first().map(String::as_str) {
        None | Some("~") => home()?,
        Some("-") => match std::env::var_os("OLDPWD") {
            Some(p) => PathBuf::from(p),
            None => bail!("OLDPWD not set"),
        },
        Some(dir) => expand_tilde(dir)?,
    };

    std::env::set_current_dir(&target)
        .map_err(|e| anyhow::anyhow!("{}: {e}", target.display()))?;

    // Track OLDPWD/PWD so `zcd -` round-trips, matching a shell.
    if let Some(old) = old {
        std::env::set_var("OLDPWD", old);
    }
    if let Ok(now) = std::env::current_dir() {
        std::env::set_var("PWD", now);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zpwd` — print the current working directory.
pub fn zpwd(_args: &[String]) -> Result<ExitCode> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("cannot read cwd: {e}"))?;
    println!("{}", cwd.display());
    Ok(ExitCode::SUCCESS)
}

/// `git zenv [NAME=VALUE...|NAME...]` — with no arguments, print every
/// environment variable as `NAME=VALUE`, sorted. A `NAME=VALUE` argument sets
/// that variable (persisting for later `git` lines in the zrepl console); a bare
/// `NAME` prints that variable's value, or nothing if it is unset.
pub fn zenv(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        let mut vars: Vec<(String, String)> = std::env::vars().collect();
        vars.sort();
        for (name, value) in vars {
            println!("{name}={value}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    for arg in args {
        match arg.split_once('=') {
            Some((name, value)) => std::env::set_var(name, value),
            None => {
                if let Ok(value) = std::env::var(arg) {
                    println!("{value}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zunset <NAME>...` — remove one or more environment variables.
pub fn zunset(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        bail!("usage: git zunset <NAME>...");
    }
    for name in args {
        std::env::remove_var(name);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zecho [-n] [<arg>...]` — print the arguments joined by a single space.
/// `-n` as the first argument suppresses the trailing newline. Arguments are
/// printed literally; there is no shell variable or glob expansion.
pub fn zecho(args: &[String]) -> Result<ExitCode> {
    let (newline, rest) = match args.split_first() {
        Some((flag, rest)) if flag == "-n" => (false, rest),
        _ => (true, args),
    };
    let line = rest.join(" ");
    if newline {
        println!("{line}");
    } else {
        print!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `$HOME` as a path, or an error if it is unset.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))
}

/// Expand a leading `~` (`~` or `~/rest`) to `$HOME`; other paths pass through.
fn expand_tilde(dir: &str) -> Result<PathBuf> {
    if dir == "~" {
        return home();
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return Ok(home()?.join(rest));
    }
    Ok(Path::new(dir).to_path_buf())
}
