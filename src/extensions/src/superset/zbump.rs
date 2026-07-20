//! `git zbump [<submodule-path>...]` — forward-only submodule gitlink bumps.
//!
//! For each target submodule this advances the parent's recorded gitlink to the
//! submodule worktree's current HEAD, but ONLY when that HEAD is a descendant of
//! the pointer already recorded in the parent (a fast-forward). It never
//! regresses or diverges a pointer. Served natively via the vendored gitoxide
//! crates so tools on PATH see the same staged index.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::BStr;

/// Outcome of a bump pass: how many pointers advanced, and any refusals as
/// `(submodule-path, reason)` — the daemon records refusals for
/// notify-on-next-command.
pub struct BumpOutcome {
    pub bumped: usize,
    pub refusals: Vec<(String, String)>,
}

/// `git zbump` — exit-code wrapper over [`zbump_run`].
pub fn zbump(args: &[String]) -> Result<ExitCode> {
    let outcome = zbump_run(args)?;
    Ok(if outcome.refusals.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Forward-only submodule gitlink bumps, coalesced and committed. Returns the
/// [`BumpOutcome`] so callers (the watcher) can surface refusals.
pub fn zbump_run(args: &[String]) -> Result<BumpOutcome> {
    // 1. Parent repo.
    let repo = gix::discover(".")?;

    // Serialize the whole index read-modify-write through the repo coordinator,
    // so concurrent zvcs writers queue FCFS instead of racing `index.lock`. Held
    // for the rest of the function; a no-op if no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // 2. Target submodules: all of them, or the ones named on the command line.
    let submodules: Vec<_> = match repo.submodules()? {
        Some(iter) => iter.collect(),
        None => anyhow::bail!("no submodules configured in this repository"),
    };

    // Requested paths, normalized (trailing slash stripped). `None` == all.
    let wanted: Option<Vec<String>> = if args.is_empty() {
        None
    } else {
        Some(
            args.iter()
                .map(|a| a.trim_end_matches('/').to_string())
                .collect(),
        )
    };

    // Owned, mutable copy of the parent index; staged once at the end.
    let mut index = repo.open_index()?;
    let mut staged = false;
    let mut bumped = 0usize;
    let mut refusals: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for sub in submodules {
        let path = sub.path()?; // repo-relative, slash-separated (BString)
        let path_str = path.to_string();

        if let Some(w) = &wanted {
            if !w.iter().any(|x| *x == path_str) {
                continue;
            }
        }
        seen.push(path_str.clone());

        // 3a. `old` = gitlink recorded in the parent HEAD tree for this path.
        let old = match sub.head_id()? {
            Some(id) => id,
            None => {
                println!("{path_str}: refused (not recorded in parent HEAD)");
                refusals.push((path_str.clone(), "not recorded in parent HEAD".into()));
                continue;
            }
        };

        // 3b. `new` = the submodule worktree's current HEAD commit.
        let subrepo = match sub.open()? {
            Some(r) => r,
            None => {
                println!("{path_str}: refused (submodule not initialized)");
                refusals.push((path_str.clone(), "submodule not initialized".into()));
                continue;
            }
        };
        let new = match subrepo.head_id() {
            Ok(id) => id.detach(),
            Err(_) => {
                // Unborn submodule HEAD (freshly init'd, no commits). Refuse this
                // path only — don't abort the whole bump pass (daemon autobump).
                println!("{path_str}: refused (submodule HEAD unborn)");
                refusals.push((path_str.clone(), "submodule HEAD unborn".into()));
                continue;
            }
        };

        if new == old {
            println!("{path_str}: already up to date");
            continue;
        }

        // 3c. Ancestry gate: fast-forward only. The merge-base is computed in
        // the submodule's object database, which holds both commits. `old` must
        // be the merge-base (i.e. an ancestor of `new`) for the bump to proceed.
        let base = match subrepo.merge_base(old, new) {
            Ok(id) => id.detach(),
            Err(err) => {
                println!("{path_str}: refused (cannot compute merge-base: {err})");
                refusals.push((path_str.clone(), format!("cannot compute merge-base: {err}")));
                continue;
            }
        };
        if base != old {
            println!(
                "{path_str}: refused (not a fast-forward: {} is not an ancestor of {})",
                old.to_hex_with_len(12),
                new.to_hex_with_len(12)
            );
            refusals.push((path_str.clone(), "not a fast-forward".into()));
            continue;
        }

        // 3d. Stage the new gitlink into the parent index at `path`.
        let idx = match index.entry_index_by_path(BStr::new(&path)) {
            Ok(idx) => idx,
            Err(_) => {
                println!("{path_str}: refused (no index entry at path)");
                refusals.push((path_str.clone(), "no index entry at path".into()));
                continue;
            }
        };
        let entry = &mut index.entries_mut()[idx];
        if entry.mode != gix::index::entry::Mode::COMMIT {
            println!("{path_str}: refused (index entry is not a gitlink)");
            refusals.push((path_str.clone(), "index entry is not a gitlink".into()));
            continue;
        }
        entry.id = new;
        staged = true;
        bumped += 1;
        println!(
            "bumped {path_str}: {}..{}",
            old.to_hex_with_len(12),
            new.to_hex_with_len(12)
        );
    }

    // Report any requested path that matched no submodule.
    if let Some(w) = &wanted {
        for a in w {
            if !seen.contains(a) {
                println!("{a}: refused (no such submodule)");
                refusals.push((a.clone(), "no such submodule".into()));
            }
        }
    }

    // 4. Persist the index once if anything was staged. The tree-cache extension
    // is written as-is by `File::write`, so drop it after mutating entries or a
    // later commit could capture the stale subtree (see gix File::write docs).
    //
    // Then close the loop: record the bumped pointers in a commit. Staging alone
    // leaves the parent's `modified: <sub> (new commits)` marker in place (it
    // only moves from unstaged to staged); committing is what clears it. The
    // commit is local, forward-only, and coalesces every bump into one revision.
    if staged {
        index.remove_tree();
        index.write(gix::index::write::Options::default())?;

        let plural = if bumped == 1 { "" } else { "s" };
        let message = format!("zvcs: autobump {bumped} submodule pointer{plural}");
        let commit_id = crate::index_commit::commit_index(&repo, &index, &message)?;
        println!("committed {} ({} pointer{})", commit_id.to_hex_with_len(12), bumped, plural);
    }

    Ok(BumpOutcome { bumped, refusals })
}
