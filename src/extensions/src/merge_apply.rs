//! Shared three-way tree merge + worktree/index application — the core behind
//! `git merge` (of diverged histories), `cherry-pick`, `revert`, and `rebase`
//! picks.
//!
//! Ported from git's merge-ort application path: a three-way [`merge_trees`]
//! produces the merged tree (conflict markers embedded for unresolved paths); the
//! merged tree is checked out over the *changed* subset of the worktree (so
//! unrelated local files are never touched); and, on conflict, the returned index
//! carries the unmerged stage 1/2/3 entries. The `Auto-merging` / `CONFLICT (…)`
//! lines git prints during the merge are emitted here, since they are identical
//! across every caller.
//!
//! [`merge_trees`]: gix::Repository::merge_trees

use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};

/// The result of applying a three-way merge to the worktree and index.
pub struct Applied {
    /// The merged tree, with conflict markers embedded for unresolved paths.
    pub tree_id: ObjectId,
    /// Paths left with unresolved conflicts; empty on a clean merge.
    pub conflicts: Vec<BString>,
    /// The resulting index — clean stage-0 entries on a clean merge, or with the
    /// unmerged stage 1/2/3 entries applied on conflict. **Not yet written**; the
    /// caller writes it after deciding whether to commit or record merge state.
    pub index: gix::index::File,
}

/// Three-way merge `ours_tree` and `theirs_tree` against `base_tree`.
///
/// Prints git's `Auto-merging` / `CONFLICT (…)` lines, checks the merged tree out
/// over the changed subset of the worktree, and returns the merged tree plus the
/// (unwritten) index. `old_index` is the pre-merge index, used both to limit the
/// checkout to changed paths and to reuse stat data for unchanged ones.
pub fn three_way_merge(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
) -> Result<Applied> {
    let mut merge = repo.merge_trees(
        base_tree,
        ours_tree,
        theirs_tree,
        labels,
        repo.tree_merge_options()?,
    )?;
    let tree_id = merge.tree.write()?.detach();

    // git's merge-ort emits an `Auto-merging <path>` line for every attempted blob
    // merge, then `CONFLICT (<kind>): Merge conflict in <path>` for the unresolved
    // ones. Trivially-identical changes resolve silently.
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let mut conflicts: Vec<BString> = Vec::new();
    for conflict in &merge.conflicts {
        let path = conflict.changes_in_resolution().0.location().to_owned();
        if conflict.content_merge().is_some() {
            println!("Auto-merging {path}");
        }
        if !conflict.is_unresolved(unresolved) {
            continue;
        }
        // merge-ort's `filemask == 6`: no ancestor stage means both sides added
        // the path, reported as `add/add` rather than `content`.
        let kind = if conflict.entries()[0].is_none() {
            "add/add"
        } else {
            "content"
        };
        println!("CONFLICT ({kind}): Merge conflict in {path}");
        conflicts.push(path);
    }

    let mut index = update_worktree_to_tree(repo, old_index, tree_id, should_interrupt)?;
    if !conflicts.is_empty() {
        merge.index_changed_after_applying_conflicts(
            &mut index,
            unresolved,
            gix::merge::tree::apply_index_entries::RemovalMode::Prune,
        );
    }

    Ok(Applied {
        tree_id,
        conflicts,
        index,
    })
}

/// Check out `new_tree_id` over the worktree, touching only entries that differ
/// from `old`, deleting worktree files the new tree drops, and returning the
/// target index (with fresh stats, **unwritten**).
fn update_worktree_to_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree_id: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<gix::index::File> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    // Check out only the entries that actually changed from the old index.
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

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

    // Remove files tracked before the merge but absent from the new tree.
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

    // Backfill stats: from the just-checked-out subset for changed paths, or from
    // the old index for entries left unchanged.
    let subset_stats: HashMap<BString, Stat> = {
        let backing = subset.path_backing();
        subset
            .entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.stat))
            .collect()
    };
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

    new_index.remove_tree();
    Ok(new_index)
}
