//! `git zon` — run a command when a semantic feed event fires.
//!
//! Where `ztrigger` reacts to raw filesystem events under a directory, `zon`
//! reacts to TYPED events from the live feed (`zevents`): commit / stage / status
//! / reconcile, optionally filtered by repo. Subscriptions live in the
//! `subscriptions` table; the daemon watches the feed and runs each match via
//! `sh -c` with `ZVCS_EVENT` / `ZVCS_REPO` / `ZVCS_DETAIL` / `ZVCS_SHA` set.
//!
//!   git zon [--kind commit|stage|status|reconcile] [--repo <substr>] -- <cmd>
//!   git zon list
//!   git zon rm <id>
//!
//! Caveat (shared with `ztrigger`): a command that itself commits or stages can
//! match its own subscription and re-fire — scope with `--kind`/`--repo`.

use anyhow::{anyhow, Result};
use std::process::ExitCode;
use std::time::Duration;

pub fn zon(args: &[String]) -> Result<ExitCode> {
    match args.first().map(|s| s.as_str()) {
        Some("list") => return list(),
        Some("rm") => {
            let id: i64 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: git zon rm <id>"))?;
            let conn = crate::db::open_rw()?;
            if crate::db::remove_subscription(&conn, id)? {
                println!("removed subscription #{id}");
            } else {
                println!("no subscription #{id}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    // [--kind K] [--repo R] -- <cmd...>
    let mut kind = None;
    let mut repo = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" | "-k" => {
                kind = args.get(i + 1).cloned();
                i += 2;
            }
            "--repo" | "-r" => {
                repo = args.get(i + 1).cloned();
                i += 2;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let command = args[i..].join(" ");
    if command.trim().is_empty() {
        return Err(anyhow!("usage: git zon [--kind <k>] [--repo <substr>] -- <command>"));
    }
    let conn = crate::db::open_rw()?;
    let id = crate::db::add_subscription(&conn, kind.as_deref(), repo.as_deref(), &command)?;
    println!(
        "subscription #{id}: on {}{} run `{command}`",
        kind.as_deref().unwrap_or("any event"),
        repo.map(|r| format!(" in repo~{r}")).unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

fn list() -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    let subs = crate::db::list_subscriptions(&conn)?;
    if subs.is_empty() {
        println!("no subscriptions");
        return Ok(ExitCode::SUCCESS);
    }
    for (id, kind, repo, cmd) in subs {
        println!(
            "#{id}  {}{}  -> {cmd}",
            kind.as_deref().unwrap_or("any"),
            repo.map(|r| format!(" repo~{r}")).unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Daemon loop: watch the event feed and run subscription commands on matches.
/// Spawned by the daemon at startup; cheap when there are no subscriptions (it
/// just keeps the cursor at the feed tip).
pub fn spawn_daemon_loop() {
    std::thread::spawn(|| {
        // `None` until the first successful db open baselines the cursor at the
        // feed tip. Initializing to 0 on a transient open failure at boot would
        // replay the entire event history the moment a subscription exists.
        let mut last: Option<i64> = None;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let Ok(conn) = crate::db::open_rw() else { continue };
            let Some(cursor) = last else {
                last = Some(crate::db::max_event_id(&conn)); // baseline: never replay history
                continue;
            };
            let subs = crate::db::list_subscriptions(&conn).unwrap_or_default();
            if subs.is_empty() {
                last = Some(crate::db::max_event_id(&conn)); // don't replay when a sub is added later
                continue;
            }
            let events = crate::db::events_since(&conn, cursor, None, None).unwrap_or_default();
            for e in &events {
                last = Some(cursor.max(e.id).max(last.unwrap_or(0)));
                for (_sid, kind, repo_like, command) in &subs {
                    let kind_ok = kind.as_deref().is_none_or(|k| k == e.kind);
                    let repo_ok = repo_like.as_deref().is_none_or(|p| {
                        e.workdir.as_deref().is_some_and(|w| w.contains(p))
                            || e.git_dir.as_deref().is_some_and(|g| g.contains(p))
                    });
                    if kind_ok && repo_ok {
                        run_command(command, e);
                    }
                }
            }
        }
    });
}

fn run_command(command: &str, e: &crate::db::EventRow) {
    let cwd = e.workdir.clone().unwrap_or_else(|| ".".into());
    let repo = e.workdir.as_deref().or(e.git_dir.as_deref()).unwrap_or("");
    if let Ok(child) = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&cwd)
        .env("ZVCS_EVENT", &e.kind)
        .env("ZVCS_REPO", repo)
        .env("ZVCS_DETAIL", e.detail.as_deref().unwrap_or(""))
        .env("ZVCS_SHA", e.sha_after.as_deref().unwrap_or(""))
        .spawn()
    {
        // The daemon is long-lived, so a dropped Child would leak a zombie on
        // Unix. Reap it on a detached waiter thread (fire-and-forget semantics
        // preserved — we don't gate the loop on the command finishing).
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
    }
}
