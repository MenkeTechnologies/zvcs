use anyhow::{bail, Result};
use std::process::ExitCode;

/// `git zsync [<submodule-path>...]` — reconcile submodules to their tracked
/// mainline, kept ATTACHED, fast-forward only, skipping any dirty worktree.
///
/// Targets default to every configured submodule; pass one or more paths to
/// restrict the set. For each target the mainline is `origin/main`, falling
/// back to `origin/master`. A submodule with local modifications is never
/// touched — it is reported and skipped.
///
/// Reconciling requires the remote-tracking ref to be current, i.e. a network
/// fetch, which in turn needs the `blocking-network-client` gix feature. That
/// feature is not compiled in, so the fetch (and the fast-forward + attach +
/// worktree update that follow it) cannot run; every non-network step is still
/// performed and reported, and the command bails at the fetch step naming the
/// exact feature required rather than faking a fetch.
pub fn zsync(args: &[String]) -> Result<ExitCode> {
    let parent = gix::discover(".")?;

    // Explicitly requested submodule paths (trailing slashes trimmed).
    // An empty set means "all submodules".
    let requested: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|a| a.trim_end_matches('/'))
        .collect();

    let submodules = match parent.submodules()? {
        Some(iter) => iter,
        None => {
            if requested.is_empty() {
                println!("no submodules configured");
                return Ok(ExitCode::SUCCESS);
            }
            bail!("no submodules configured");
        }
    };

    // Materialize each submodule together with its worktree-relative path so we
    // can validate the requested set before doing any work.
    let mut items = Vec::new();
    for sm in submodules {
        let path = sm.path()?.to_string();
        items.push((sm, path));
    }

    for req in &requested {
        if !items.iter().any(|(_, path)| path.as_str() == *req) {
            bail!("{req}: no such submodule");
        }
    }

    let mut any_error = false;
    let mut clean_pending: Vec<String> = Vec::new();

    for (sm, path) in &items {
        // Restrict to the requested set when paths were given.
        if !requested.is_empty() && !requested.iter().any(|req| *req == path.as_str()) {
            continue;
        }

        // Open the submodule repository; `None` means it was never initialized.
        let sm_repo = match sm.open()? {
            Some(repo) => repo,
            None => {
                println!("{path}: not initialized, skipped");
                continue;
            }
        };

        // (2a) Mainline detection: prefer origin/main, else origin/master.
        let mainline = if sm_repo
            .try_find_reference("refs/remotes/origin/main")?
            .is_some()
        {
            "main"
        } else if sm_repo
            .try_find_reference("refs/remotes/origin/master")?
            .is_some()
        {
            "master"
        } else {
            println!("{path}: no origin/main or origin/master, skipped");
            any_error = true;
            continue;
        };

        // (2c) Clean check — never touch a dirty worktree. `is_dirty()` reports
        // index-vs-worktree and tree-vs-index changes (untracked files, which a
        // fast-forward checkout would not clobber, are intentionally ignored).
        if sm_repo.is_dirty()? {
            println!("{path}: dirty, skipped");
            continue;
        }

        // Clean and resolvable. The remaining work — (2b) fetch origin so the
        // remote-tracking ref is current, then (2d) fast-forward the local
        // mainline branch, re-attach HEAD, and update the worktree — all hinges
        // on the fetch, which is unavailable in this build. Report the
        // clean-check for this submodule and defer the bail until every target
        // has been inspected.
        println!("{path}: clean, needs fetch (origin/{mainline})");
        clean_pending.push(path.clone());
    }

    // (2b) The fetch step. Bail precisely — do not fake a fetch.
    if !clean_pending.is_empty() {
        bail!(
            "fetch requires gix feature `blocking-network-client` (not compiled in); \
             {} clean submodule(s) ready to sync once enabled: {}",
            clean_pending.len(),
            clean_pending.join(", ")
        );
    }

    if any_error {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
