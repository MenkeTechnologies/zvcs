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

/// `git zunclaim [<path>]` — release this session's lease on a repo.
pub fn zunclaim(args: &[String]) -> Result<ExitCode> {
    let (git_dir, workdir) = target(args)?;
    let session = crate::session_key();
    let conn = crate::db::open_rw()?;
    let repo_id = crate::db::upsert_repo(&conn, &git_dir, Some(&workdir))?;
    if crate::db::unclaim(&conn, repo_id, &session)? {
        println!("released {}", workdir.display());
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("zvcs: no claim of yours to release here");
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
