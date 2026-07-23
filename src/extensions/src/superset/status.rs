//! Machine-wide instant status: `zstatus` (this repo, live) and `zstatus --all`
//! (every indexed repo, from the db).
//!
//! The daemon keeps each watched repo's status fresh in the db on ref-change
//! ([`record`]), so `--all` is a pre-computed read across thousands of repos with
//! no walking and no forks — the state is maintained reactively, not recomputed
//! per query. `sync` is derived from the merge-base against `origin/main` (cheap,
//! no revwalk): `up-to-date` / `ahead` / `behind` / `diverged` / `no-upstream`.

use anyhow::Result;
use std::io::IsTerminal;
use std::process::ExitCode;

/// Compute `(dirty, detached, sync, head)` for an open repo.
pub fn compute(repo: &gix::Repository) -> (bool, bool, String, String) {
    let detached = repo.head_name().ok().flatten().is_none();
    let dirty = repo.is_dirty().unwrap_or(false);

    let head = match repo.head_name().ok().flatten() {
        Some(name) => name.shorten().to_string(),
        None => match repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten()) {
            Some(id) => format!("detached@{}", id.to_hex_with_len(12)),
            None => "unborn".to_string(),
        },
    };

    (dirty, detached, sync_state(repo), head)
}

/// Sync state vs `origin/main` (else `origin/master`), from the merge-base only.
fn sync_state(repo: &gix::Repository) -> String {
    let local = match repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten()) {
        Some(id) => id.detach(),
        None => return "unborn".to_string(),
    };
    for m in ["main", "master"] {
        let Ok(Some(r)) = repo.try_find_reference(&format!("refs/remotes/origin/{m}")) else {
            continue;
        };
        let Ok(rid) = r.into_fully_peeled_id() else { continue };
        let remote = rid.detach();
        if local == remote {
            return "up-to-date".to_string();
        }
        return match repo.merge_base(local, remote).map(|b| b.detach()) {
            Ok(b) if b == remote => "ahead".to_string(),
            Ok(b) if b == local => "behind".to_string(),
            Ok(_) => "diverged".to_string(),
            Err(_) => "unrelated".to_string(),
        };
    }
    "no-upstream".to_string()
}

/// Record `repo`'s status into the db (used by the daemon watcher).
pub fn record(conn: &rusqlite::Connection, git_dir: &std::path::Path, workdir: &std::path::Path) {
    let Ok(repo) = gix::open(git_dir) else { return };
    let (dirty, detached, sync, head) = compute(&repo);
    if let Ok(repo_id) = crate::db::upsert_repo(conn, git_dir, Some(workdir)) {
        let _ = crate::db::upsert_status(conn, repo_id, dirty, detached, &sync, &head);
    }
}

/// `git zstatus [--all]`.
pub fn zstatus(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "--all" || a == "-a") {
        return zstatus_all();
    }

    // Live status for the current repo (and cache it under canonical paths).
    let repo = gix::discover(".")?;
    let (dirty, detached, sync, head) = compute(&repo);
    let git_dir = repo
        .git_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.git_dir().to_path_buf());
    let workdir = repo
        .workdir()
        .map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()));
    if let (Ok(conn), Some(wd)) = (crate::db::open_rw(), workdir) {
        if let Ok(id) = crate::db::upsert_repo(&conn, &git_dir, Some(&wd)) {
            let _ = crate::db::upsert_status(&conn, id, dirty, detached, &sync, &head);
        }
    }
    let dirt = if dirty { "dirty" } else { "clean" };
    let det = if detached { " detached" } else { "" };
    println!("{head}: {sync}, {dirt}{det}");
    Ok(ExitCode::SUCCESS)
}

/// `git zstatus --all` — every indexed repo's cached status. Pipe-clean:
/// `<sync>\t<clean|dirty>\t<head>\t<path>` per line; hints to stderr if a tty.
fn zstatus_all() -> Result<ExitCode> {
    let interactive = std::io::stdout().is_terminal();
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => {
            if interactive {
                eprintln!("zvcs: no status index yet (needs the daemon or `git zstatus`)");
            }
            return Ok(ExitCode::SUCCESS);
        }
    };
    let rows = crate::db::list_status(&conn)?;
    for s in &rows {
        let dirt = if s.dirty { "dirty" } else { "clean" };
        let det = if s.detached { "+detached" } else { "" };
        println!("{}{}\t{}\t{}\t{}", s.sync, det, dirt, s.head, s.path);
    }
    if interactive {
        eprintln!("{} repo(s)", rows.len());
    }
    Ok(ExitCode::SUCCESS)
}
