//! Auto-spawn the per-repo coordinator when `[zvcs]` autonomy is enabled, so the
//! user never starts `zdaemon` by hand. Invoked once per `git` command.
//!
//! It is cheap and idempotent: if the daemon is already listening it returns
//! immediately; it only spawns when autonomy is configured AND no daemon is up.
//! The child is detached (own process group, stdio to `<git-dir>/zvcs.log`) so it
//! outlives the invoking command and never holds the terminal.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Ensure a coordinator is running for the current repo, if `[zvcs]` autonomy is
/// enabled. Silent no-op when not in a repo, not configured, or already running.
pub fn ensure_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    if !crate::config::ZvcsConfig::load(&repo).should_watch() {
        return;
    }

    let sock = crate::superset::zdaemon::socket_path();
    // Already listening? A successful connect means the singleton daemon is up.
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return;
    }

    let workdir = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    spawn_detached(&workdir);
}

/// Spawn `git zdaemon start` detached (own process group, stdio → the singleton
/// log), rooted at `workdir` so it discovers the right repo to watch. Fire and
/// forget — a race with another spawner is harmless (the loser's `start` bails
/// "daemon already running"). Used by autostart and by `zdaemon restart`.
pub fn spawn_detached(workdir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["zdaemon", "start"])
        .current_dir(workdir)
        .stdin(Stdio::null());
    route_stdio_to_log(&mut cmd);
    cmd.process_group(0);
    let _ = cmd.spawn();
}

/// Re-exec this binary with `args`, fully detached (own process group, stdio →
/// the singleton log), inheriting the caller's cwd. Turns a foreground command
/// into a background one so the prompt returns at once — e.g. `zreindex` handing
/// a whole-device crawl to a child. Fire-and-forget; output lands in `zvcs.log`.
pub fn spawn_detached_self(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null());
    route_stdio_to_log(&mut cmd);
    cmd.process_group(0);
    let _ = cmd.spawn();
}

/// Point a child's stdout/stderr at the singleton log; never the terminal (no
/// chatter). Falls back to `/dev/null` if the log can't be opened.
fn route_stdio_to_log(cmd: &mut Command) {
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::superset::zdaemon::zvcs_home().join("zvcs.log"))
    {
        Ok(out) => match out.try_clone() {
            Ok(err) => {
                cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
            }
            Err(_) => {
                cmd.stdout(Stdio::from(out)).stderr(Stdio::null());
            }
        },
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
}
