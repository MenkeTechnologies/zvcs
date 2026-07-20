//! Auto-spawn the per-repo coordinator when `[zvcs]` autonomy is enabled, so the
//! user never starts `zdaemon` by hand. Invoked once per `git` command.
//!
//! It is cheap and idempotent: if the daemon is already listening it returns
//! immediately; it only spawns when autonomy is configured AND no daemon is up.
//! The child is detached (own process group, stdio to `<git-dir>/zvcs.log`) so it
//! outlives the invoking command and never holds the terminal.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Ensure a coordinator is running for the current repo, if `[zvcs]` autonomy is
/// enabled. Silent no-op when not in a repo, not configured, or already running.
pub fn ensure_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    if !crate::config::ZvcsConfig::load(&repo).any_autonomous() {
        return;
    }

    let git_dir = repo.git_dir();
    let sock = crate::superset::zdaemon::socket_path();
    // Already listening? A successful connect means the singleton daemon is up.
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return;
    }

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let workdir = repo.workdir().unwrap_or(git_dir).to_path_buf();

    let mut cmd = Command::new(exe);
    cmd.args(["zdaemon", "start"])
        .current_dir(&workdir)
        .stdin(Stdio::null());

    // Route daemon output to the singleton log; never the terminal (no chatter).
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

    // Detach into its own process group so a terminal signal to the invoking
    // command does not also kill the daemon.
    cmd.process_group(0);

    // Fire and forget: a race with another instance spawning simultaneously is
    // harmless — the loser's `start` bails "daemon already running".
    let _ = cmd.spawn();
}
