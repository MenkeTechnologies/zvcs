//! `git merge <ref>` — fast-forward-only merge of a ref into `HEAD`.
//!
//! Only the fast-forward case is served natively via the vendored gitoxide
//! crates: the ref being merged must be a descendant of the current `HEAD`
//! (their merge-base is `HEAD` itself). When that holds, the branch `HEAD`
//! points to is advanced to the ref (or, on a detached `HEAD`, `HEAD` itself
//! is moved), and the clean worktree + index are moved to the new tree.
//!
//! Anything that would require a real merge commit — a diverged history, an
//! octopus (multiple refs), or `--no-ff` — is refused with a precise message
//! rather than faked. A dirty worktree is likewise refused so no uncommitted
//! change is clobbered.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn merge(args: &[String]) -> Result<ExitCode> {
    // Split flags from positional refs. Only the fast-forward-compatible flags
    // are accepted; anything that implies a merge commit or is unrecognized is
    // refused so its semantics are never silently ignored.
    let mut refs: Vec<&str> = Vec::new();
    for arg in args {
        let a = arg.as_str();
        if let Some(flag) = a.strip_prefix("--") {
            match flag {
                // We only ever fast-forward, so these are no-ops for us.
                "ff" | "ff-only" => {}
                "no-ff" => anyhow::bail!("--no-ff requires a merge commit, unsupported"),
                other => anyhow::bail!("unsupported flag --{other}"),
            }
        } else if a.starts_with('-') && a != "-" {
            anyhow::bail!("unsupported flag {a}");
        } else {
            refs.push(a);
        }
    }

    if refs.is_empty() {
        anyhow::bail!("no commit specified");
    }
    if refs.len() > 1 {
        anyhow::bail!("octopus merge (multiple refs) is not supported");
    }
    let spec = refs[0];

    let repo = gix::discover(".")?;

    // Current HEAD state. An unborn branch has no commit to fast-forward from;
    // a real merge into it would be a checkout, which is out of scope.
    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot merge into an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached; `None` when detached.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);

    // Resolve the ref to merge and peel it to a commit (tags included).
    let target_id = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?.id;

    // Fast-forward decision.
    let base = repo.merge_base(local_id, target_id)?.detach();
    if base == target_id {
        // Target already reachable from HEAD (or identical) — nothing to do.
        println!("Already up to date.");
        return Ok(ExitCode::SUCCESS);
    }
    if base != local_id {
        // Histories diverged: a real merge commit would be required.
        anyhow::bail!("not possible to fast-forward, real merge unsupported");
    }

    // From here on we mutate a ref, the index and the worktree. Serialize the
    // whole read-modify-write through the repo coordinator (a no-op if no
    // daemon is running), matching the zsync/zbump write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Never clobber uncommitted work — refuse a dirty worktree.
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to fast-forward");
    }

    // Capture the current (clean) index BEFORE moving the ref; it mirrors the
    // old tree and carries filesystem stats reused for unchanged files.
    let old_index = repo.index_or_load_from_head()?.into_owned();

    // Advance the ref: the attached branch, or HEAD itself when detached. Both
    // are direct (non-symbolic) refs here, so `deref` is false either way.
    let name: FullName = match &branch {
        Some(b) => b.clone(),
        None => "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("merge {spec}: Fast-forward").into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(local_id)),
            new: Target::Object(target_id),
        },
        name,
        deref: false,
    })?;

    // Move the clean worktree + index onto the new tree.
    let should_interrupt = AtomicBool::new(false);
    update_clean_worktree(&repo, &old_index, target_id, &should_interrupt)?;

    // git prints the abbreviated span then the mode line, followed by a
    // diffstat. The span/mode are exact; the diffstat is not reproduced (see
    // the module note) — the refs, index and worktree are fully correct.
    println!(
        "Updating {}..{}",
        local_id.to_hex_with_len(7),
        target_id.to_hex_with_len(7)
    );
    println!("Fast-forward");

    Ok(ExitCode::SUCCESS)
}

/// Move a clean worktree and its index from the state captured in `old` to the
/// tree of commit `new_commit`, writing only the files that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the old `HEAD` tree-index against the new tree-index (file-level
/// granularity), added/modified files are checked out via `gix-worktree-state`,
/// removed files are deleted, and the new index is written reusing prior stats
/// for unchanged entries so a later status stays cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;
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
    gix::worktree::state::checkout(
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

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
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
