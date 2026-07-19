//! `git pull` — fetch the configured (or named) remote, then fast-forward the
//! current branch onto the fetched upstream.
//!
//! `pull` is `fetch` followed by a merge. Only the fast-forward case is served
//! natively: the fetched upstream tip must be a descendant of the current
//! `HEAD`. The fetch is a blocking network operation (like `zsync`); the
//! integration step is delegated to the already-ported [`merge`](super::merge),
//! so the stdout (`Updating <old>..<new>` / `Fast-forward`, or `Already up to
//! date.`) and the diverged-history refusal are identical to `git merge`'s.
//!
//! Supported invocation forms:
//!   * `git pull`                  — use the current branch's configured upstream.
//!   * `git pull <remote>`         — fetch `<remote>`, merge the configured upstream branch.
//!   * `git pull <remote> <branch>`— fetch `<remote>`, merge `refs/remotes/<remote>/<branch>`.
//!
//! Anything that would need a real merge commit or a rewrite of history
//! (`--rebase`, `--no-ff`, a diverged branch) is refused with a precise message
//! rather than faked. The fetch progress summary git prints to stderr (`From
//! …`, per-ref update lines) is not reproduced; refs, index and worktree are
//! fully correct.

use anyhow::{bail, Context, Result};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::remote::Direction;

pub fn pull(args: &[String]) -> Result<ExitCode> {
    // Split flags from positionals. Only fast-forward-compatible flags are
    // accepted; anything implying a merge commit or history rewrite is refused
    // so its semantics are never silently dropped.
    let mut positionals: Vec<&str> = Vec::new();
    for arg in args {
        let a = arg.as_str();
        if let Some(flag) = a.strip_prefix("--") {
            let key = flag.split('=').next().unwrap_or(flag);
            match key {
                // We only ever fast-forward, so these are already satisfied.
                "ff" | "ff-only" | "no-rebase" | "quiet" | "verbose" => {}
                "rebase" => bail!("--rebase is not supported (fast-forward only)"),
                "no-ff" => bail!("--no-ff requires a merge commit, unsupported"),
                other => bail!("unsupported flag --{other}"),
            }
        } else if a.starts_with('-') && a != "-" {
            match a {
                "-q" | "-v" => {}
                "-r" => bail!("--rebase is not supported (fast-forward only)"),
                other => bail!("unsupported flag {other}"),
            }
        } else {
            positionals.push(a);
        }
    }

    let repo = gix::discover(".")?;
    let head_name = repo.head_name()?;

    // Resolve which remote to fetch and which remote-tracking ref to merge.
    let (remote_name, target_ref) = if positionals.len() >= 2 {
        // Explicit `<remote> <branch>`: after a default-refspec fetch the branch
        // lands at refs/remotes/<remote>/<branch>.
        let remote = positionals[0].to_string();
        let target = format!("refs/remotes/{}/{}", remote, positionals[1]);
        (remote, target)
    } else {
        // No explicit branch: derive everything from the current branch's
        // upstream configuration (branch.<name>.remote / .merge).
        let head = head_name.as_ref().ok_or_else(|| {
            anyhow::anyhow!("You are not currently on a branch. Please specify which branch to pull.")
        })?;

        let remote = match positionals.first() {
            Some(r) => r.to_string(),
            None => match repo.branch_remote_name(head.shorten(), Direction::Fetch) {
                Some(name) => name.as_bstr().to_string(),
                None => bail!("There is no tracking information for the current branch."),
            },
        };

        let target = match repo.branch_remote_tracking_ref_name(head.as_ref(), Direction::Fetch) {
            Some(Ok(name)) => name.as_bstr().to_string(),
            Some(Err(err)) => return Err(err.into()),
            None => bail!("There is no tracking information for the current branch."),
        };
        (remote, target)
    };

    // Phase 1: fetch. Wrap the ref-mutating fetch in the repo lock, then release
    // it before delegating to `merge` (which re-acquires it) to avoid nesting a
    // second acquisition inside the first — that would deadlock a live daemon.
    {
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let should_interrupt = AtomicBool::new(false);
        let remote = repo
            .find_remote(remote_name.as_str())
            .with_context(|| format!("'{remote_name}' does not appear to be a configured remote"))?;
        remote
            .connect(Direction::Fetch)?
            .prepare_fetch(gix::progress::Discard, gix::remote::ref_map::Options::default())?
            .receive(gix::progress::Discard, &should_interrupt)?;
    }

    // The upstream ref must now exist locally; if the fetch produced no such
    // tracking ref the requested branch does not exist on the remote.
    if repo.try_find_reference(target_ref.as_str())?.is_none() {
        bail!("couldn't find remote ref {target_ref}");
    }

    // Phase 2: integrate. Delegate the fast-forward, dirty check, worktree/index
    // update and git-identical stdout to the ported `merge`.
    super::merge(&[target_ref])
}
