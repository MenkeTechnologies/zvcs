//! `git zdoctor` — a health check of the zvcs environment.
//!
//! One screen that answers "is my zvcs set up correctly": is this binary the
//! `git` on PATH, is `$ZVCS_HOME` present, is the coordinator running, is there a
//! ledger, are the man pages and dashed symlinks installed, and is `~/.zvcs/man`
//! on MANPATH. Each check reports OK / WARN / FAIL; the process exits non-zero
//! only if a hard FAIL is found, so it is scriptable.

use anyhow::Result;
use std::io::{IsTerminal, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::db;
use crate::superset::{manpage, zdaemon};

/// Outcome of a single check.
enum Level {
    Ok,
    Warn,
    Fail,
}

/// `git zdoctor` — run every check, print a report, exit non-zero on any FAIL.
pub fn zdoctor(_args: &[String]) -> Result<ExitCode> {
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut any_fail = false;
    let mut report = |level: Level, label: &str, detail: String| {
        if matches!(level, Level::Fail) {
            any_fail = true;
        }
        println!("{}  {label}: {detail}", marker(&level, colored));
    };

    report(Level::Ok, "version", env!("CARGO_PKG_VERSION").to_string());

    // Is this binary the `git` PATH resolves to?
    match (first_git_on_path(), std::env::current_exe().ok()) {
        (Some(found), Some(me)) => {
            let same = canon(&found) == canon(&me);
            if same {
                report(Level::Ok, "git shadow", format!("zvcs is the git on PATH ({})", found.display()));
            } else {
                report(
                    Level::Warn,
                    "git shadow",
                    format!("PATH git is {}, not this binary ({})", found.display(), me.display()),
                );
            }
        }
        _ => report(Level::Warn, "git shadow", "no `git` found on PATH".to_string()),
    }

    // ZVCS_HOME.
    let home = zdaemon::zvcs_home();
    if home.is_dir() {
        report(Level::Ok, "home", home.display().to_string());
    } else {
        report(Level::Warn, "home", format!("{} does not exist yet (created on demand)", home.display()));
    }

    // Coordinator daemon.
    let sock = zdaemon::socket_path();
    match daemon_status(&sock) {
        Some(state) => report(Level::Ok, "daemon", format!("running ({})", state.trim())),
        None => report(Level::Warn, "daemon", "not running (autostarts when [zvcs] autonomy is configured)".to_string()),
    }

    // Ledger db.
    let db = db::db_path();
    if db.exists() {
        report(Level::Ok, "ledger", db.display().to_string());
    } else {
        report(Level::Warn, "ledger", "no ledger yet (created by the daemon / first zreindex)".to_string());
    }

    // Man pages.
    let man1 = manpage::man_dir().join("man1");
    let installed = manpage::DOCS
        .iter()
        .filter(|d| man1.join(format!("git-{}.1", d.verb)).exists())
        .count();
    let total = manpage::DOCS.len();
    if installed == total {
        report(Level::Ok, "man pages", format!("{total} installed in {}", man1.display()));
    } else {
        report(
            Level::Warn,
            "man pages",
            format!("{installed}/{total} installed — run `git zdashed` (git help <zverb> installs on demand)"),
        );
    }

    // MANPATH includes the man dir.
    let man_root = manpage::man_dir();
    if path_list_contains("MANPATH", &man_root) {
        report(Level::Ok, "MANPATH", format!("includes {}", man_root.display()));
    } else {
        report(
            Level::Warn,
            "MANPATH",
            format!("does not include {} (`man git-<verb>` needs it; `git help` works regardless)", man_root.display()),
        );
    }

    // Dashed symlinks.
    let bin = home.join("bin");
    if bin.join("git-status").exists() {
        report(Level::Ok, "dashed forms", format!("installed in {}", bin.display()));
    } else {
        report(Level::Warn, "dashed forms", "not installed — run `git zdashed` (needed once stock git is removed)".to_string());
    }

    Ok(if any_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// The colored (or plain) status marker for a level.
fn marker(level: &Level, colored: bool) -> String {
    let (text, color) = match level {
        Level::Ok => ("[ OK ]", "\x1b[32m"),
        Level::Warn => ("[WARN]", "\x1b[33m"),
        Level::Fail => ("[FAIL]", "\x1b[31m"),
    };
    if colored {
        format!("{color}{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// The first `git` executable on `PATH`, or `None`.
fn first_git_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|p| p.is_file())
}

/// Canonicalize, falling back to the path as-is so a comparison still works.
fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Whether the `$<var>` PATH-style list contains `dir`.
fn path_list_contains(var: &str, dir: &Path) -> bool {
    let Some(val) = std::env::var_os(var) else {
        return false;
    };
    let target = canon(dir);
    std::env::split_paths(&val).any(|p| canon(&p) == target)
}

/// Ask the coordinator for its one-line STATUS, with a short timeout. `None` if
/// nothing is listening (daemon down). Mirrors the daemon's `STATUS` protocol
/// without pulling in its client internals.
fn daemon_status(sock: &Path) -> Option<String> {
    let mut stream = UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok();
    stream.write_all(b"STATUS\n").ok()?;
    stream.flush().ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    let line = buf.lines().next()?.to_string();
    (!line.is_empty()).then_some(line)
}
