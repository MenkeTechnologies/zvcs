use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// Reconcile a single already-open repository (a submodule or a top-level repo)
/// to its tracked mainline and return a one-line human status.
///
/// The mainline is `origin/main`, falling back to `origin/master`. The operation
/// is fast-forward only and never touches a dirty worktree, so an unpushed local
/// commit is never regressed or clobbered. On a genuine fast-forward the local
/// mainline branch is advanced, `HEAD` is (re)attached to it, and the clean
/// worktree plus index are moved to the new tree by writing only the files that
/// actually changed.
///
/// This function performs no terminal output; it returns the status string and
/// leaves printing to the caller.
pub fn reconcile_repo(repo: &gix::Repository) -> Result<String> {
    reconcile_repo_inner(repo, true)
}

/// Reconcile WITHOUT fetching — fast-forward the local mainline to the
/// **already-present** `origin/main` remote-tracking ref. This is the reactive,
/// no-network path the daemon watcher uses after a local `git pull` updated the
/// remote-tracking ref; the daemon never contacts a remote itself.
pub fn reconcile_repo_local(repo: &gix::Repository) -> Result<String> {
    reconcile_repo_inner(repo, false)
}

fn reconcile_repo_inner(repo: &gix::Repository, do_fetch: bool) -> Result<String> {
    // Serialize the whole check-fetch-ff-write through the repo coordinator, so an
    // autonomous reconcile can't race a concurrent writer. Held for the function;
    // a no-op if no daemon is running (ff-only + skip-dirty still protect).
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // (a) Mainline detection: prefer origin/main, else origin/master. Neither is
    // an error — the repo simply has no mainline to track.
    let mainline = if repo.try_find_reference("refs/remotes/origin/main")?.is_some() {
        "main"
    } else if repo.try_find_reference("refs/remotes/origin/master")?.is_some() {
        "master"
    } else {
        return Ok("no origin/main or origin/master, skipped".to_string());
    };

    // (b) Clean check — never touch a dirty worktree.
    if repo.is_dirty()? {
        return Ok("dirty, skipped".to_string());
    }

    // (c) Fetch origin so the remote-tracking ref is current (blocking fetch).
    // Skipped on the reactive path: the watcher only runs after a local pull has
    // already updated the remote-tracking ref, and the daemon must never poll.
    let should_interrupt = AtomicBool::new(false);
    if do_fetch {
        let remote = repo
            .find_remote("origin")
            .context("a configured `origin` remote is required to fetch")?;
        remote
            .connect(gix::remote::Direction::Fetch)?
            .prepare_fetch(gix::progress::Discard, gix::remote::ref_map::Options::default())?
            .receive(gix::progress::Discard, &should_interrupt)?;
    }

    // (d) Fast-forward decision. `local` is the commit HEAD currently resolves to
    // (the actual clean worktree state); `remote` is the freshly-fetched tip.
    let mut head = repo.head()?;
    let local_id = match head.try_peel_to_id()? {
        Some(id) => id.detach(),
        None => return Ok("unborn HEAD, skipped".to_string()),
    };
    let remote_ref_name = format!("refs/remotes/origin/{mainline}");
    let remote_id = repo
        .find_reference(&remote_ref_name)?
        .into_fully_peeled_id()?
        .detach();

    if local_id == remote_id {
        // Already current — but a detached HEAD (the state `git submodule update`
        // leaves, and the single most common one) must still be re-attached, or
        // this early return would leave it detached forever. Local, no-clobber.
        if repo.head_name()?.is_none() {
            if let crate::superset::Attached::Attached { .. } = crate::superset::ensure_attached(repo)? {
                return Ok(format!("up to date, re-attached to {mainline}"));
            }
        }
        return Ok(format!("up to date (origin/{mainline})"));
    }
    // A true fast-forward requires the local tip to be an ancestor of the remote
    // tip, i.e. their merge-base is the local tip itself. Anything else (local
    // ahead, or diverged) is left untouched.
    let base = repo.merge_base(local_id, remote_id)?.detach();
    if base != local_id {
        return Ok(format!("local ahead/diverged of origin/{mainline}, skipped"));
    }

    // Guard the BRANCH we are about to force-move (refs/heads/<mainline>), not only
    // HEAD. A detached HEAD — the state `git submodule update` leaves — can sit at
    // an ancestor of the remote while `refs/heads/<mainline>` itself carries
    // unpushed commits. The HEAD check above passes, but force-moving the branch
    // with `PreviousValue::Any` would then orphan those commits — the exact commits
    // `ensure_attached` deliberately refuses to touch. Only advance a branch that is
    // itself behind (an ancestor of) the remote tip; otherwise leave it untouched.
    if let Some(branch_ref) = repo.try_find_reference(&format!("refs/heads/{mainline}"))? {
        if let Ok(tip) = branch_ref.into_fully_peeled_id() {
            let branch_tip = tip.detach();
            let branch_behind = branch_tip == remote_id
                || repo
                    .merge_base(branch_tip, remote_id)
                    .map(|b| b.detach() == branch_tip)
                    .unwrap_or(false);
            if !branch_behind {
                return Ok(format!("local {mainline} ahead/diverged of origin/{mainline}, skipped"));
            }
        }
    }

    // Capture the current (clean) index BEFORE mutating any ref. It mirrors the
    // worktree and the old tree, and carries real filesystem stats we can reuse
    // for the files that don't change.
    let old = repo.index_or_load_from_head()?.into_owned();

    // Refuse the fast-forward BEFORE moving any ref if applying the new tree would
    // overwrite an untracked path on disk. `is_dirty()` (the clean gate above)
    // ignores untracked files, so without this a headless reconcile would clobber
    // an untracked file the new tree happens to add — silent data loss. This also
    // catches a dir->file change (the new file path already exists as a directory),
    // which would otherwise fail the checkout *after* the refs had already moved.
    {
        let new_tree_id = repo.find_object(remote_id)?.peel_to_tree()?.id;
        let new_index = repo.index_from_tree(&new_tree_id)?;
        let old_paths: HashSet<BString> = {
            let backing = old.path_backing();
            old.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        let backing = new_index.path_backing();
        for e in new_index.entries() {
            let path = e.path_in(backing);
            // Only additions can collide with an on-disk path; modified/deleted
            // paths were tracked and clean.
            if !old_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    if full.exists() {
                        // Distinguish a genuine untracked clobber from a dir->file
                        // change (the addition lands on a directory that only holds
                        // tracked files). Both are skipped without moving refs, but
                        // don't mislabel a fully-tracked directory as "untracked".
                        let mut prefix = path.to_owned();
                        prefix.push(b'/');
                        let tracked_dir = full.is_dir()
                            && old_paths.iter().any(|op| op.starts_with(prefix.as_slice()));
                        if tracked_dir {
                            return Ok(format!("dir->file change at '{path}' unsupported, skipped"));
                        }
                        return Ok(format!("would overwrite untracked '{path}', skipped"));
                    }
                }
            }
        }
    }

    // (e) Update the clean worktree + index to the new tree FIRST, and advance the
    // refs only once it succeeds. If the checkout fails part-way (a file<->dir
    // transition, a read-only tracked file, ENOSPC, …), the branch/HEAD stay at the
    // old commit — leaving the repo self-consistent (refs match the still-old
    // index; at worst an ordinary dirty worktree) instead of pointing the refs at
    // the new commit over a stale/partial worktree the daemon never repairs.
    update_clean_worktree(repo, &old, remote_id, &should_interrupt)?;

    // Advance the local mainline branch to the remote tip, then attach HEAD to that
    // branch so the repository is left on `main`/`master`, not detached.
    let branch_name: FullName = format!("refs/heads/{mainline}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name refs/heads/{mainline}: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("zsync: fast-forward {mainline} to origin/{mainline}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(remote_id),
        },
        name: branch_name.clone(),
        deref: false,
    })?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("zsync: attach HEAD to {mainline}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_name),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;

    Ok(format!(
        "synced origin/{mainline} {}..{}",
        local_id.to_hex_with_len(12),
        remote_id.to_hex_with_len(12)
    ))
}

/// Move a clean worktree and its index from the state captured in `old` to the
/// tree of commit `new_commit`, writing only the files that changed.
///
/// The change set is derived by comparing two flattened tree-indices — the old
/// `HEAD` tree (`old`) against the new tree — rather than `gix_diff`, because a
/// tree diff can report directory-granular additions/deletions, whereas the
/// worktree write needs per-file granularity. Comparing the two indices yields
/// exactly the file-level added/modified/deleted set, independent of how a tree
/// diff would choose to recurse.
///
/// Added/modified files (content or mode changed) are checked out through
/// `gix-worktree-state`, which applies the correct mode, symlink and filter
/// handling. Removed files are deleted from disk. The new index is written with
/// fresh stats for the changed files and the previous stats reused for the
/// unchanged ones, so a later status check stays cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index by path of the current entries, for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // The full target index (all new-tree entries, stats zeroed) — this is what
    // is eventually written to disk. It is exactly the new tree, so deletions are
    // naturally absent from it.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;

    // A second copy reduced to just the changed entries (added, or content/mode
    // differs from `old`) — the subset actually checked out to the worktree.
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        // Present before with identical content and mode → unchanged, drop it.
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        // Absent before → an addition, keep it.
        None => false,
    });

    // Write the changed files into the (clean) worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files present in the old tree but not the new one.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Fill in the target index stats: checked-out (changed) entries get their
    // fresh stat; unchanged entries reuse the previous stat.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}

/// `git zsync [<submodule-path>...]` — reconcile submodules to their tracked
/// mainline, kept ATTACHED, fast-forward only, skipping any dirty worktree.
///
/// Targets default to every configured submodule; pass one or more paths to
/// restrict the set. For each target the mainline is `origin/main`, falling
/// back to `origin/master`. A submodule with local modifications is never
/// touched — it is reported and skipped.
///
/// Each submodule is reconciled by [`reconcile_repo`], which fetches `origin`,
/// fast-forwards the local mainline, re-attaches `HEAD`, and updates the clean
/// worktree. One status line is printed per target. A single submodule failing
/// is reported and does not abort the others; the command exits non-zero if any
/// target errored.
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

        match reconcile_repo(&sm_repo) {
            Ok(status) => println!("{path}: {status}"),
            Err(err) => {
                println!("{path}: error: {err:#}");
                any_error = true;
            }
        }
    }

    if any_error {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
