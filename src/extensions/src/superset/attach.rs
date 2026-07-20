//! Re-attach a repository's `HEAD` to its mainline branch.
//!
//! `git submodule update` leaves every submodule on a **detached HEAD** at the
//! recorded pointer; committing there orphans the work. The autonomous daemon
//! guarantees attachment so an agent never meets a detached HEAD and never runs
//! the stash → `checkout -B main` → stash-pop dance.
//!
//! [`ensure_attached`] is a **purely local, no-clobber** operation: it never
//! contacts a remote, never moves the checked-out commit, and never touches the
//! worktree or index. It only creates/advances the local mainline branch to the
//! commit `HEAD` is already at and makes `HEAD` symbolic to it. Because it does
//! not move the commit, it is safe on a **dirty** worktree — the in-flight
//! changes are preserved untouched. Fast-forwarding the branch onward to
//! `origin/main` is a separate, clean-only concern (`reconcile_repo`).

use anyhow::{anyhow, Result};

use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// Outcome of an attach attempt.
pub enum Attached {
    /// `HEAD` was already symbolic (on a branch) — nothing to do.
    AlreadyAttached,
    /// `HEAD` was detached and is now attached to `mainline`.
    Attached { mainline: String },
    /// No mainline branch could be determined (no `main`/`master` locally or on
    /// `origin`), or `HEAD` is unborn — nothing to attach to.
    NoMainline,
    /// Attaching would move the branch backward or need a worktree move; refused
    /// to avoid clobbering. Carries a human reason.
    Refused(String),
}

/// Ensure `repo`'s `HEAD` is attached to its mainline branch at `HEAD`'s current
/// commit. Local, no network, no worktree/index mutation.
pub fn ensure_attached(repo: &gix::Repository) -> Result<Attached> {
    // Symbolic HEAD (`ref: refs/heads/...`) → already attached.
    if repo.head_name()?.is_some() {
        return Ok(Attached::AlreadyAttached);
    }

    // Detached or unborn: resolve the commit HEAD points at. Unborn → nothing.
    let Some(head_id) = repo.head()?.try_peel_to_id()? else {
        return Ok(Attached::NoMainline);
    };
    let head_id = head_id.detach();

    let Some(mainline) = mainline_name(repo)? else {
        return Ok(Attached::NoMainline);
    };
    let branch_ref = format!("refs/heads/{mainline}");

    match repo.try_find_reference(&branch_ref)? {
        // Branch already exists.
        Some(r) => {
            let branch_id = r.into_fully_peeled_id()?.detach();
            if branch_id == head_id {
                // Pure relabel: HEAD → branch, same commit.
                attach_symbolic(repo, &mainline)?;
                return Ok(Attached::Attached { mainline });
            }
            // Different commit. Forward-move the branch to HEAD only if HEAD is a
            // descendant of the branch (branch is the merge-base) — that needs no
            // worktree move (HEAD is already checked out there). Anything else
            // (branch ahead of, or diverged from, HEAD) is refused: moving the
            // branch backward could drop commits, and catching the worktree up is
            // reconcile's clean-only job, not attach's.
            let is_ff = matches!(repo.merge_base(branch_id, head_id), Ok(base) if base.detach() == branch_id);
            if is_ff {
                update_branch(repo, &mainline, head_id)?;
                attach_symbolic(repo, &mainline)?;
                Ok(Attached::Attached { mainline })
            } else {
                Ok(Attached::Refused(format!(
                    "local {mainline} is ahead of or diverged from HEAD"
                )))
            }
        }
        // No branch yet: create it at HEAD and attach.
        None => {
            update_branch(repo, &mainline, head_id)?;
            attach_symbolic(repo, &mainline)?;
            Ok(Attached::Attached { mainline })
        }
    }
}

/// Mainline branch name for `repo`: `main` if a local `refs/heads/main` or a
/// `refs/remotes/origin/main` exists, else `master` on the same test, else none.
fn mainline_name(repo: &gix::Repository) -> Result<Option<String>> {
    // Prefer a name backed by a LOCAL branch (the repo's actual mainline) before
    // falling back to remote-only evidence, so a repo whose real mainline is
    // `master` isn't attached to a fresh `main` conjured from a stray
    // refs/remotes/origin/main.
    for name in ["main", "master"] {
        if repo
            .try_find_reference(&format!("refs/heads/{name}"))?
            .is_some()
        {
            return Ok(Some(name.to_string()));
        }
    }
    for name in ["main", "master"] {
        if repo
            .try_find_reference(&format!("refs/remotes/origin/{name}"))?
            .is_some()
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Point `refs/heads/<mainline>` at `id` (create or forward-move). Local.
fn update_branch(repo: &gix::Repository, mainline: &str, id: ObjectId) -> Result<()> {
    let name: FullName = format!("refs/heads/{mainline}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name refs/heads/{mainline}: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("zvcs attach: point {mainline} at HEAD").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// Make `HEAD` symbolic to `refs/heads/<mainline>` (no worktree change).
fn attach_symbolic(repo: &gix::Repository, mainline: &str) -> Result<()> {
    let branch: FullName = format!("refs/heads/{mainline}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name refs/heads/{mainline}: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("zvcs attach: HEAD -> {mainline}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::mainline_name;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?}");
    }

    #[test]
    fn prefers_local_master_over_stray_remote_main() {
        let dir = std::env::temp_dir().join(format!("zvcs-mainline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        git(&dir, &["init", "-q", "-b", "master"]);
        git(&dir, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "c0"]);
        let head = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap().stdout).unwrap().trim().to_string();
        // A stray remote-tracking origin/main with NO local main and NO origin/master.
        git(&dir, &["update-ref", "refs/remotes/origin/main", &head]);

        let repo = gix::open(&dir).unwrap();
        // The real mainline is the local `master`; a bare "prefer main" would pick
        // the stray remote ref and attach to the wrong branch.
        assert_eq!(mainline_name(&repo).unwrap().as_deref(), Some("master"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefers_local_main_when_present() {
        let dir = std::env::temp_dir().join(format!("zvcs-mainline2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "c0"]);
        let repo = gix::open(&dir).unwrap();
        assert_eq!(mainline_name(&repo).unwrap().as_deref(), Some("main"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
