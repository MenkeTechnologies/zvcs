//! `zup` — bring the whole tree to latest `origin/main`.
//!
//! One command fetches and fast-forwards the current repo AND every nested
//! submodule to its tracked mainline (`origin/main`, else `origin/master`),
//! keeping `HEAD` attached — `git zsync` covers submodules only; `zup` includes
//! the top-level repo and recurses through nested submodules. Fast-forward only;
//! a dirty or diverged repo is reported and skipped, never clobbered. Reuses
//! [`reconcile_repo`], which fetches, ff-advances, re-attaches, and updates the
//! clean worktree.

use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;

/// `git zup [<path>]` — reconcile the tree at cwd (or `<path>`) to latest.
pub fn zup(args: &[String]) -> Result<ExitCode> {
    let at = args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from);
    let repo = match at {
        Some(p) => gix::discover(p)?,
        None => gix::discover(".")?,
    };
    let mut out: Vec<(String, String)> = Vec::new();
    up(&repo, &mut out);

    let mut any_error = false;
    for (label, status) in &out {
        if status.starts_with("error") {
            any_error = true;
        }
        println!("{label}: {status}");
    }
    Ok(if any_error { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// Reconcile `repo` then recurse into its initialized submodules.
fn up(repo: &gix::Repository, out: &mut Vec<(String, String)>) {
    let label = repo
        .workdir()
        .map(|w| w.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    match crate::superset::reconcile_repo(repo) {
        Ok(status) => out.push((label, status)),
        Err(e) => out.push((label, format!("error: {e:#}"))),
    }
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                up(&sub, out);
            }
        }
    }
}
