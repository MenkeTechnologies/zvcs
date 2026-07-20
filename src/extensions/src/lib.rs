//! zvcs — the git-shadowing superset engine, as a library.
//!
//! The `git` binary (`src/main.rs`) is a thin entry point over [`run`]. Exposing
//! the engine as a library lets integration tests drive the coordination layer
//! (e.g. [`lock::RepoLock`] against a live `zdaemon`) directly.

pub mod autostart;
pub mod config;
pub mod crawler;
pub mod db;
pub mod dispatch;
pub mod index_commit;
pub mod jobpool;
pub mod jobrun;
pub mod lock;
pub mod porcelain;
pub mod superset;
pub mod worktree;

use std::process::ExitCode;

/// Parse `argv`, dispatch the subcommand, and return the process exit code.
/// Errors are reported terse on stderr as `zvcs: <command>: <reason>`.
pub fn run() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    // Consume the leading git-global options we support, so `git -C <dir> <verb>`
    // (extremely common in scripts and tooling) reaches the verb instead of
    // treating `-C` as the subcommand. `-C <dir>` chdirs (before autostart /
    // failure-surfacing, which key off the cwd); the pager flags are accepted and
    // ignored (zvcs never pages). Unrecognized globals (`-c`, `--git-dir`, …) are
    // left in place and surface as an error rather than being silently mishandled.
    let mut idx = 0;
    while idx < raw.len() {
        match raw[idx].as_str() {
            "-C" => {
                let Some(dir) = raw.get(idx + 1) else { break };
                if std::env::set_current_dir(dir).is_err() {
                    eprintln!("zvcs: -C: cannot chdir to {dir}");
                    return ExitCode::FAILURE;
                }
                idx += 2;
            }
            "-p" | "-P" | "--paginate" | "--no-pager" => idx += 1,
            _ => break,
        }
    }
    let args = &raw[idx..];

    let Some(sub) = args.first() else {
        eprintln!("zvcs: no subcommand given");
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];

    // Surface any headless autonomous-op failures recorded since last time, on
    // this next `git` invocation. Async/daemon failures carry no exit code back,
    // so this at-least-once notification is their only channel. stderr only, so
    // `$(git …)` capture stays clean. Skipped for `zdaemon` to avoid self-noise.
    if sub != "zdaemon" {
        surface_pending_failures();
    }

    // Bring up the singleton coordinator when `[zvcs]` autonomy is configured, so
    // the user never starts it by hand. Skipped for `zdaemon` (it would self-race).
    if sub != "zdaemon" {
        autostart::ensure_if_configured();
    }

    match dispatch::run(sub, rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zvcs: {sub}: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The current session key for attributing operations to an agent: `ZVCS_SESSION`
/// if set (export `ZVCS_SESSION=$$` per shell), else the parent process id. Used
/// by claims, job submission, and the op ledger.
pub fn session_key() -> String {
    std::env::var("ZVCS_SESSION")
        .unwrap_or_else(|_| format!("pid-{}", std::os::unix::process::parent_id()))
}

/// Print (once) any unnotified failed autonomous jobs for the current repo, then
/// mark them notified. Cheap no-op when there is no ledger or no failures; never
/// creates the ledger (only reads/updates one the daemon already made).
fn surface_pending_failures() {
    if !db::db_path().exists() {
        return;
    }
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    let git_dir = match repo.git_dir().canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };
    // Read with the cheap RO handle: this runs on EVERY git invocation across all
    // concurrent instances, and the common case is zero pending failures. Opening
    // RW here would replay the whole schema DDL and take a write lock every time,
    // purely to run a SELECT. Only take the RW handle when there is something to
    // clear.
    let Ok(conn) = db::open_ro() else {
        return;
    };
    let Ok(pending) = db::pending_failures(&conn, &git_dir) else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    let ids: Vec<i64> = pending.iter().map(|(id, _, _)| *id).collect();
    for (_, kind, reason) in &pending {
        if reason.is_empty() {
            eprintln!("zvcs: {kind} failed");
        } else {
            eprintln!("zvcs: {kind} failed: {reason}");
        }
    }
    if let Ok(wconn) = db::open_rw() {
        let _ = db::mark_notified(&wconn, &ids);
    }
}
