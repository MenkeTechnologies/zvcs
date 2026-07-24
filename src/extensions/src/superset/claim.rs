//! Multi-agent claim/lease coordination: `zclaim`, `zunclaim`, `zwho`.
//!
//! With N agents working one meta tree, an advisory **lease** lets a bot signal
//! "I'm working this repo" so peers don't step on it. The daemon/db holds one
//! claim per repo (race-safe via the primary key), attributed to the caller's
//! session ([`crate::session_key`]). It is advisory — it coordinates, it does not
//! physically block writes (that's the per-repo fair lane's job).

use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;

/// Resolve the repo at `args[0]` (or cwd) → `(git_dir, workdir)` canonicalized.
fn target(args: &[String]) -> Result<(PathBuf, PathBuf)> {
    let at = args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from);
    let repo = match at {
        Some(p) => gix::discover(p)?,
        None => gix::discover(".")?,
    };
    let git_dir = repo.git_dir().canonicalize().unwrap_or_else(|_| repo.git_dir().to_path_buf());
    let workdir = repo
        .workdir()
        .map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()))
        .unwrap_or_else(|| git_dir.clone());
    Ok((git_dir, workdir))
}

/// `git zclaim [<path>]` — lease a repo for this session.
pub fn zclaim(args: &[String]) -> Result<ExitCode> {
    let (git_dir, workdir) = target(args)?;
    let session = crate::session_key();
    let conn = crate::db::open_rw()?;
    let repo_id = crate::db::upsert_repo(&conn, &git_dir, Some(&workdir))?;
    let wd = workdir.to_string_lossy();
    match crate::db::claim(&conn, repo_id, &session, Some(&wd))? {
        crate::db::ClaimResult::Acquired => {
            println!("claimed {wd} for {session}");
            Ok(ExitCode::SUCCESS)
        }
        crate::db::ClaimResult::AlreadyMine => {
            println!("already yours ({session})");
            Ok(ExitCode::SUCCESS)
        }
        crate::db::ClaimResult::HeldBy(other) => {
            eprintln!("zvcs: {wd} is claimed by {other}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `git zunclaim [--force] [<path>]` — release a lease on a repo. Without
/// `--force` only this session's own claim is released; `--force` (`-f`) clears
/// whoever holds it — the escape hatch for a lease left by a dead agent, since
/// claims have no TTL.
pub fn zunclaim(args: &[String]) -> Result<ExitCode> {
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let (git_dir, workdir) = target(args)?;
    let session = crate::session_key();
    let conn = crate::db::open_rw()?;
    let repo_id = crate::db::upsert_repo(&conn, &git_dir, Some(&workdir))?;
    let released = if force {
        crate::db::unclaim_force(&conn, repo_id)?
    } else {
        crate::db::unclaim(&conn, repo_id, &session)?
    };
    if released {
        println!("released {}", workdir.display());
        Ok(ExitCode::SUCCESS)
    } else if force {
        eprintln!("zvcs: no claim to release here");
        Ok(ExitCode::FAILURE)
    } else {
        eprintln!("zvcs: no claim of yours to release here (use --force to clear another session's)");
        Ok(ExitCode::FAILURE)
    }
}

/// `git zwho` — list active claims (who is working what).
pub fn zwho(_args: &[String]) -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS), // no ledger → no claims
    };
    for (path, session, _ts) in crate::db::list_claims(&conn)? {
        println!("{session}\t{path}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zsessions` — active sessions ranked by how many repos each holds, so it
/// is clear which agent is working the most of the tree.
pub fn zsessions(_args: &[String]) -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => {
            println!("no active claims");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let claims = crate::db::list_claims(&conn)?;
    if claims.is_empty() {
        println!("no active claims");
        return Ok(ExitCode::SUCCESS);
    }
    let mut by_session: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, session, _) in claims {
        *by_session.entry(session).or_default() += 1;
    }
    let mut rows: Vec<(usize, String)> = by_session.into_iter().map(|(s, n)| (n, s)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (n, session) in &rows {
        println!("{n:>4}  {session}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zidle [selectors]` — indexed repos with no active claim: the ones free
/// for an agent to pick up. Selectors narrow the candidate set as in `zforeach`.
pub fn zidle(args: &[String]) -> Result<ExitCode> {
    let (sel, _rest) = crate::superset::select::Selector::parse(args);
    let repos = sel.select()?;
    if repos.is_empty() {
        println!("no repos matched");
        return Ok(ExitCode::SUCCESS);
    }
    let claimed: std::collections::HashSet<String> = match crate::db::open_ro() {
        Ok(conn) => crate::db::list_claims(&conn)?.into_iter().map(|(p, _, _)| p).collect(),
        Err(_) => std::collections::HashSet::new(),
    };
    let mut shown = 0usize;
    for (_, workdir) in &repos {
        let path = workdir.display().to_string();
        if !claimed.contains(&path) {
            println!("{path}");
            shown += 1;
        }
    }
    eprintln!("zidle: {shown} of {} indexed unclaimed", repos.len());
    Ok(ExitCode::SUCCESS)
}
