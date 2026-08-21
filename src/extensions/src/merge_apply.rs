//! Shared three-way tree merge + worktree/index application — the core behind
//! `git merge` (of diverged histories), `cherry-pick`, `revert`, and `rebase`
//! picks.
//!
//! Ported from git's merge-ort application path: a three-way [`merge_trees`]
//! produces the merged tree (conflict markers embedded for unresolved paths); the
//! merged tree is checked out over the *changed* subset of the worktree (so
//! unrelated local files are never touched); and, on conflict, the returned index
//! carries the unmerged stage 1/2/3 entries. The `Auto-merging` / `CONFLICT (…)`
//! lines git prints during the merge are rendered by [`crate::merge_msg`] — the
//! one renderer every merge verb shares — in its permissive mode, so a conflict
//! class whose git text is not ported degrades to the plain content notice
//! rather than aborting a merge whose worktree has already moved.
//!
//! [`three_way_merge_guarded`] adds the last step of
//! `merge_switch_to_result()`: the `unpack_trees()` pass that refuses the
//! checkout — rather than overwrite it — when local work sits on a path the
//! merge touches. Callers that gate on a dirty worktree themselves keep using
//! the unguarded entry points.
//!
//! [`merge_trees`]: gix::Repository::merge_trees
//!
//! [`StrategyOptions`] is the `-X`/`--strategy-option` half: a port of
//! merge-ort's `parse_merge_opt()` plus the `match-trees.c` tree shifting that
//! `-Xsubtree[=<prefix>]` drives and the `xdl_recmatch()` whitespace rules
//! `-Xignore-*` drives (those live in [`crate::merge_ws`], shared with
//! `merge-tree`). Every branch of `parse_merge_opt()` is honoured, so the only
//! `-X` that fails is one git itself rejects.

use anyhow::{anyhow, Result};
// The `Auto-merging` / `CONFLICT (…)` lines below go through git's stdout
// buffer, so a caller that armed it (`merge`) keeps them in step with its own
// stdout. A caller that has not (`stash apply`, `am`, `pull`, `merge-octopus`)
// never buffers, and these stay unbuffered writes exactly as before.
use crate::cstdio::println;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::Algorithm;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::merge::blob::builtin_driver::text::ConflictStyle;

/// What [`three_way_merge_guarded`] did: the merge was applied, or the checkout
/// that would have applied it was refused because it would have clobbered local
/// work. A refusal prints nothing and moves nothing — `merge_switch_to_result()`
/// runs its `checkout()` before `merge_display_update_messages()` and `return`s
/// on failure, so the `Auto-merging`/`CONFLICT` lines the merge produced are
/// dropped along with the merge itself.
// One of these is produced per merge, on the stack, and both variants are
// consumed immediately — boxing the applied side would only add an allocation.
#[allow(clippy::large_enum_variant)]
pub enum Merged {
    Applied(Applied),
    Refused(crate::merge_guard::Clobber),
}

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
    /// The `Auto-merging <path>` / `CONFLICT (<kind>): Merge conflict in <path>`
    /// lines this merge produced, in order, whether or not `show_msgs` let them
    /// be printed.
    ///
    /// They are returned as well as printed because git's decision to show them
    /// is not always taken before the merge: the sequencer computes
    /// `show_output = !is_rebase_i(opts) || !result.clean` *after*
    /// `merge_incore_nonrecursive()` (sequencer.c:783), and `do_merge()` does the
    /// same with `if (ret <= 0) fputs(o.obuf.buf, stdout)` (sequencer.c:4360-4361)
    /// — that is what makes `git rebase -Xours` silent when the option resolves
    /// every hunk and noisy when it does not. Such a caller passes
    /// `show_msgs = false` and prints these itself.
    pub messages: Vec<String>,
}

/// `merge_switch_to_result()`'s last act: record the merged tree as `AUTO_MERGE`
/// (merge-ort.c:4950, the `write_auto_merge` region).
///
/// git writes it for **every** result merge-ort checks out — clean or
/// conflicted, committed or not — which is why `.git/AUTO_MERGE` survives a
/// successful `git cherry-pick <commit>` and a `git merge` that a refused
/// checkout aborted, and why `git rev-parse AUTO_MERGE` resolves in both. It is
/// removed only by `remove_merge_branch_state()` (branch.c:829) and
/// `sequencer_post_commit_cleanup()`, which is why a merge that goes on to
/// *commit* leaves none.
///
/// One function rather than a `fs::write` at each merge site: git has exactly
/// one, and copies of the same three lines is how most of them came to be
/// missing — `merge`, `pull`, `rebase`, `merge-recursive`, `merge-subtree` and
/// `stash apply` all left the file unwritten while `cherry-pick` wrote it.
///
/// **Called by each merge site rather than from [`three_way_merge`] itself.**
/// This module's tree-merge is used by two kinds of caller: the ones that stand
/// in for merge-ort (`merge`, `pull`, `cherry-pick`, `rebase`'s picks, `stash
/// apply`) and the ones that stand in for something else git implements with
/// `read-tree`/`merge-index` — `git-merge-octopus.sh` above all, which leaves no
/// `AUTO_MERGE` because no merge-ort ever runs. Writing from inside would put
/// the file where git has none. The wrappers also return *before*
/// `merge_switch_to_result()` in cases this port still merges through:
/// `merge_ort_nonrecursive()` short-circuits on `merge_base == merge`
/// ("Already up to date."), which is why a `git stash apply` of an
/// untracked-only stash writes nothing.
///
/// Written as a loose root ref file, not through the ref store: that is what the
/// files backend produces for a name outside `refs/`, and what
/// [`crate::sequencer::delete_state_ref`] expects to remove.
pub fn write_auto_merge(repo: &gix::Repository, tree_id: ObjectId) -> Result<()> {
    std::fs::write(repo.git_dir().join("AUTO_MERGE"), format!("{tree_id}\n"))?;
    Ok(())
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
    three_way_merge_verbose(
        repo,
        base_tree,
        ours_tree,
        theirs_tree,
        old_index,
        labels,
        should_interrupt,
        true,
    )
}

/// [`three_way_merge`] with git's `show_msgs` switch made explicit.
///
/// `merge-ort-wrappers.c` computes `show_msgs = !!opt->verbosity` and passes it
/// to `merge_switch_to_result()`, so a caller that reads `merge.verbosity` (only
/// `git merge` does — the sequencer's picks print unconditionally) can silence
/// the `Auto-merging` / `CONFLICT (…)` block by passing `false`. Everything else
/// — the merged tree, the conflicted index, the worktree update — is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn three_way_merge_verbose(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
    show_msgs: bool,
) -> Result<Applied> {
    three_way_merge_with_options(
        repo,
        base_tree,
        ours_tree,
        theirs_tree,
        old_index,
        labels,
        should_interrupt,
        show_msgs,
        &StrategyOptions::default(),
    )
}

/// [`three_way_merge_verbose`] with the `-X`/`--strategy-option` knobs applied.
///
/// `xopts` is the parsed result of git's `parse_merge_opt()`; it decides the
/// blob merge's conflict resolution (`-Xours`/`-Xtheirs`), the diff algorithm,
/// rename detection, `merge.renormalize`, and whether *their* tree and the base
/// are shifted first (`-Xsubtree[=<prefix>]`).
#[allow(clippy::too_many_arguments)]
pub fn three_way_merge_with_options(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
    show_msgs: bool,
    xopts: &StrategyOptions,
) -> Result<Applied> {
    let merged = merge_and_apply(
        repo,
        base_tree,
        ours_tree,
        theirs_tree,
        old_index,
        labels,
        should_interrupt,
        show_msgs,
        xopts,
        None,
        None,
    )?;
    let Merged::Applied(applied) = merged else {
        unreachable!("no worktree tree was handed over, so no checkout guard ran")
    };
    Ok(applied)
}

/// [`three_way_merge_with_options`] with git's checkout guard armed.
///
/// `worktree_tree` is the tree the worktree currently holds (`HEAD` for a
/// merge). Before the merged tree is written out, the move from that tree to the
/// merged one is put through [`crate::merge_guard::verify_two_way`] — the
/// `unpack_trees()` pass `merge_switch_to_result()` ends with — so local work
/// the merge would overwrite stops it instead of being lost. Paths outside the
/// merge's footprint are never consulted, so unrelated local edits survive.
#[allow(clippy::too_many_arguments)]
pub fn three_way_merge_guarded(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
    show_msgs: bool,
    xopts: &StrategyOptions,
    worktree_tree: ObjectId,
) -> Result<Merged> {
    merge_and_apply(
        repo,
        base_tree,
        ours_tree,
        theirs_tree,
        old_index,
        labels,
        should_interrupt,
        show_msgs,
        xopts,
        Some(worktree_tree),
        None,
    )
}

/// [`three_way_merge_verbose`] with git's `--conflict=<style>` applied to the
/// markers this merge writes.
///
/// git threads the style through `ll_merge()`'s `struct ll_merge_options`, where
/// `--conflict=diff3`/`zdiff3` is a per-invocation override of
/// `merge.conflictStyle` — the same knob, one layer up. This is the entry point
/// for the `git checkout -m`/`git switch -m` stash re-apply, which is a real
/// three-way merge and therefore has to mark up in the style the switch asked
/// for; the `<paths>` form of `checkout` merges blob by blob and sets the style
/// on `builtin_driver::text` directly.
///
/// `None` leaves `merge.conflictStyle` alone. `-Xours`/`-Xtheirs` still win over
/// it, as in git: `recursive_variant` resolves conflicting hunks outright, so
/// there are no markers left for a style to shape.
#[allow(clippy::too_many_arguments)]
pub fn three_way_merge_styled(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
    show_msgs: bool,
    style: Option<ConflictStyle>,
) -> Result<Applied> {
    let merged = merge_and_apply(
        repo,
        base_tree,
        ours_tree,
        theirs_tree,
        old_index,
        labels,
        should_interrupt,
        show_msgs,
        &StrategyOptions::default(),
        None,
        style,
    )?;
    let Merged::Applied(applied) = merged else {
        unreachable!("no worktree tree was handed over, so no checkout guard ran")
    };
    Ok(applied)
}

/// `--conflict=<style>` / `merge.conflictStyle` as git spells it. Returns `None`
/// for anything else, which is a caller's error to report.
pub fn conflict_style(name: &str) -> Option<ConflictStyle> {
    Some(match name {
        "merge" => ConflictStyle::Merge,
        "diff3" => ConflictStyle::Diff3,
        "zdiff3" => ConflictStyle::ZealousDiff3,
        _ => return None,
    })
}

/// The shared body: merge the trees, report, then (unless the guard refuses)
/// move the worktree and index onto the result.
#[allow(clippy::too_many_arguments)]
fn merge_and_apply(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    old_index: &gix::index::File,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
    should_interrupt: &AtomicBool,
    show_msgs: bool,
    xopts: &StrategyOptions,
    guard: Option<ObjectId>,
    // `--conflict=<style>`: the per-invocation override of
    // `merge.conflictStyle`, or `None` to leave the configured style alone.
    style: Option<ConflictStyle>,
) -> Result<Merged> {
    // `merge_ort_nonrecursive_internal()` (merge-ort.c) shifts *their* tree and
    // the merge base to match the shape of *our* tree, before any merge info is
    // collected — the merged tree therefore comes out in our shape.
    let (base_tree, theirs_tree) = match &xopts.subtree_shift {
        Some(prefix) => (
            shift_tree_object(repo, ours_tree, base_tree, prefix.as_ref())?,
            shift_tree_object(repo, ours_tree, theirs_tree, prefix.as_ref())?,
        ),
        None => (base_tree, theirs_tree),
    };

    let renormalized = renormalized_repo(repo, xopts)?;
    let repo = renormalized.as_ref().unwrap_or(repo);

    // `opt->branch1`/`opt->branch2`, needed again below to attribute a
    // conflicting path to the operand git names it after. `Labels` borrows, and
    // the merge call consumes it, so take owned copies first.
    let label1 = labels.current.unwrap_or_default().to_string();
    let label2 = labels.other.unwrap_or_default().to_string();

    let mut merge = repo.merge_trees(
        base_tree,
        ours_tree,
        theirs_tree,
        labels,
        // `git merge` and every sequencer verb that reaches here go through
        // `init_ui_merge_options()`, so `diff.algorithm` applies.
        tree_merge_options(repo, xopts, style, true)?,
    )?;
    let tree_id = merge.tree.write()?.detach();

    // git's merge-ort emits an `Auto-merging <path>` line for every blob merge it
    // actually runs, then the conflict notice for each class it could not
    // resolve. The lines are collected rather than printed here because merge-ort
    // collects them too — `path_msg()` appends to `opt->priv->output` and only
    // `merge_display_update_messages()` flushes it, which
    // `merge_switch_to_result()` reaches *after* its checkout.
    //
    // Rendering runs through [`crate::merge_msg`], shared with `merge-tree`,
    // `merge-recursive` and `merge-subtree`, in its permissive mode: this caller
    // has a worktree to move, so a class whose git text is not ported degrades to
    // the plain content notice rather than aborting a merge that already happened.
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let conflicts: Vec<BString> = merge
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(unresolved))
        .map(crate::merge_msg::conflict_location)
        .collect();
    // Operand 1 is *our* tree as merged, not as spelled: `-Xsubtree` shifts the
    // other two onto its shape, so this is the tree the conflicts are reported
    // against.
    let messages: Vec<String> = crate::merge_msg::render(
        repo,
        &merge.conflicts,
        &label1,
        &label2,
        crate::merge_msg::Operand1::Tree(ours_tree),
        unresolved,
        crate::merge_msg::Strictness::Approximate,
    )?
    .into_iter()
    // These are printed with `println!`, so drop the newline git's `puts()` adds.
    .map(|m| m.text.trim_end_matches('\n').to_owned())
    .collect();

    // `merge_switch_to_result()` ends in `checkout()`, an `unpack_trees()` from
    // the worktree's current tree to the merged one — which refuses rather than
    // overwrite local work on any path the merge touches. A refused checkout
    // `return`s straight away, so the collected messages are never displayed: the
    // merge did not happen, and saying `Auto-merging f` before `Aborting` would
    // claim it did.
    if let Some(worktree_tree) = guard {
        let clobber = crate::merge_guard::verify_two_way(repo, worktree_tree, tree_id, old_index)?;
        if !clobber.is_empty() {
            return Ok(Merged::Refused(clobber));
        }
    }

    if show_msgs {
        for line in &messages {
            println!("{line}");
        }
    }

    let mut index = update_worktree_to_tree(repo, old_index, tree_id, should_interrupt)?;
    if !conflicts.is_empty() {
        merge.index_changed_after_applying_conflicts(
            &mut index,
            unresolved,
            gix::merge::tree::apply_index_entries::RemovalMode::Prune,
        );
    }
    // Every merge-shaped verb — `merge` and its strategies, `pull`, `am`, `rebase`,
    // `cherry-pick`, `revert`, `stash apply`, `checkout`'s autostash — reaches its
    // finished index through this one function and then writes it, so this is where
    // git's parting `cache_tree_update(&o->internal.result, WRITE_TREE_SILENT |
    // WRITE_TREE_REPAIR)` belongs (unpack-trees.c:2088-2092). Doing it here rather
    // than at each writer is what keeps the twelve of them from drifting apart.
    //
    // It runs *after* the conflict entries are applied, because a conflicted index
    // has no tree at all: `verify_cache()` refuses an unmerged entry
    // (cache-tree.c:218-234) and the update leaves the extension off, which is the
    // right answer for an index that cannot be turned into a tree.
    crate::porcelain::write_tree::rebuild_cache_tree(repo, &mut index);

    Ok(Merged::Applied(Applied {
        tree_id,
        conflicts,
        index,
        messages,
    }))
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

// ---------------------------------------------------------------------------
// `-X` / `--strategy-option` — a port of merge-ort's `parse_merge_opt()`
// ---------------------------------------------------------------------------

/// merge-ort's `recursive_variant`: which side wins a conflicting hunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    /// `-Xours`.
    Ours,
    /// `-Xtheirs`.
    Theirs,
}

/// The subset of merge-ort's `struct merge_options` that `-X` can reach.
///
/// Each field is `None`/`0` when no `-X` set it, in which case the repository
/// configuration decides, exactly as in git.
#[derive(Clone, Debug, Default)]
pub struct StrategyOptions {
    /// `-Xours` / `-Xtheirs`.
    pub variant: Option<Variant>,
    /// `-Xsubtree` (an empty prefix — shift automatically) or
    /// `-Xsubtree=<prefix>` (shift by the named prefix).
    pub subtree_shift: Option<BString>,
    /// `-Xhistogram` / `-Xdiff-algorithm=<algo>`.
    pub diff_algorithm: Option<Algorithm>,
    /// `-Xrenormalize` / `-Xno-renormalize`.
    pub renormalize: Option<bool>,
    /// `-Xno-renames` / `-Xfind-renames[=<n>]` / `-Xrename-threshold=<n>`.
    pub detect_renames: Option<bool>,
    /// `opt->xdl_opts & XDF_WHITESPACE_FLAGS` — the `-Xignore-*-space*` /
    /// `-Xignore-cr-at-eol` family. `parse_merge_opt()` ORs each into the same
    /// word (merge-ort.c:5567-5574), so more than one can be in force; which one
    /// then decides is `crate::merge_ws::canonicalize_for`'s job.
    pub xdl_opts: u32,
    /// merge-ort's `rename_score`, on git's `0..=MAX_SCORE` scale. `0` means
    /// "unset", which diffcore reads as `DEFAULT_RENAME_SCORE` (50%).
    pub rename_score: u32,
}

/// git's `#define MAX_SCORE 60000.0` (diffcore.h) — a rename score of
/// `MAX_SCORE` is a 100% similarity requirement.
const MAX_SCORE: f64 = 60000.0;

/// git's `DEFAULT_RENAME_SCORE` (diffcore.h), the similarity `diffcore_rename()`
/// falls back to when `rename_score` is left at 0.
const DEFAULT_RENAME_SCORE: f64 = 30000.0;

/// Why a `-X <value>` did not take effect.
///
/// Every branch of `parse_merge_opt()` (merge-ort.c:5541-5598) is honoured here,
/// so git's own refusal is the only way to fail.
#[derive(Debug)]
pub enum StrategyOptionError {
    /// git's own `parse_merge_opt()` rejects it.
    Unknown(String),
}

impl std::fmt::Display for StrategyOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `die(_("unknown strategy option: -X%s"), …)` in builtin/merge.c.
            StrategyOptionError::Unknown(spec) => write!(f, "unknown strategy option: -X{spec}"),
        }
    }
}

impl StrategyOptions {
    /// Apply every `-X <value>` in order, as `try_merge_strategy()` does.
    pub fn parse(specs: &[String]) -> std::result::Result<Self, StrategyOptionError> {
        Self::parse_from(Self::default(), specs)
    }

    /// [`parse`](Self::parse) on top of a seed the strategy already set.
    ///
    /// `try_merge_strategy()` assigns `o.subtree_shift = ""` for `-s subtree`
    /// *before* the `for (x = 0; x < xopts.nr; x++) parse_merge_opt(…)` loop
    /// (builtin/merge.c:815-822), so `-s subtree -Xsubtree=<prefix>` ends up with
    /// the explicit prefix rather than the automatic shift.
    pub fn parse_from(
        seed: Self,
        specs: &[String],
    ) -> std::result::Result<Self, StrategyOptionError> {
        let mut out = seed;
        for spec in specs {
            out.apply(spec)?;
        }
        Ok(out)
    }

    /// One `parse_merge_opt()` call (merge-ort.c), kept in the C's branch order.
    fn apply(&mut self, s: &str) -> std::result::Result<(), StrategyOptionError> {
        let unknown = || StrategyOptionError::Unknown(s.to_owned());

        if s.is_empty() {
            return Err(unknown());
        } else if s == "ours" {
            self.variant = Some(Variant::Ours);
        } else if s == "theirs" {
            self.variant = Some(Variant::Theirs);
        } else if s == "subtree" {
            self.subtree_shift = Some(BString::default());
        } else if let Some(arg) = s.strip_prefix("subtree=") {
            self.subtree_shift = Some(BString::from(arg));
        } else if s == "patience" {
            self.diff_algorithm = Some(Algorithm::Patience);
        } else if s == "histogram" {
            self.diff_algorithm = Some(Algorithm::Histogram);
        } else if let Some(arg) = s.strip_prefix("diff-algorithm=") {
            match parse_algorithm_value(arg) {
                Some(algo) => self.diff_algorithm = Some(algo),
                None => return Err(unknown()),
            }
        } else if s == "ignore-space-change" {
            self.xdl_opts |= crate::merge_ws::XDF_IGNORE_WHITESPACE_CHANGE;
        } else if s == "ignore-all-space" {
            self.xdl_opts |= crate::merge_ws::XDF_IGNORE_WHITESPACE;
        } else if s == "ignore-space-at-eol" {
            self.xdl_opts |= crate::merge_ws::XDF_IGNORE_WHITESPACE_AT_EOL;
        } else if s == "ignore-cr-at-eol" {
            self.xdl_opts |= crate::merge_ws::XDF_IGNORE_CR_AT_EOL;
        } else if s == "renormalize" {
            self.renormalize = Some(true);
        } else if s == "no-renormalize" {
            self.renormalize = Some(false);
        } else if s == "no-renames" {
            self.detect_renames = Some(false);
        } else if s == "find-renames" {
            self.detect_renames = Some(true);
            self.rename_score = 0;
        } else if let Some(arg) = s
            .strip_prefix("find-renames=")
            .or_else(|| s.strip_prefix("rename-threshold="))
        {
            let (score, consumed) = parse_rename_score(arg.as_bytes());
            if consumed != arg.len() {
                return Err(unknown());
            }
            self.rename_score = score;
            self.detect_renames = Some(true);
        } else {
            return Err(unknown());
        }
        Ok(())
    }
}

/// `parse_algorithm_value()` (diff.c). `None` is git's `-1`, the value it
/// rejects; every name it accepts maps onto a `gix-imara-diff` algorithm.
fn parse_algorithm_value(value: &str) -> Option<Algorithm> {
    if value.eq_ignore_ascii_case("myers") || value.eq_ignore_ascii_case("default") {
        Some(Algorithm::Myers)
    } else if value.eq_ignore_ascii_case("minimal") {
        Some(Algorithm::MyersMinimal)
    } else if value.eq_ignore_ascii_case("patience") {
        Some(Algorithm::Patience)
    } else if value.eq_ignore_ascii_case("histogram") {
        Some(Algorithm::Histogram)
    } else {
        None
    }
}

/// `parse_rename_score()` (diff.c): read `<num>[.<frac>][%]` and scale it onto
/// git's `MAX_SCORE`. Returns the score and how many bytes were consumed, since
/// the caller rejects trailing garbage.
fn parse_rename_score(cp: &[u8]) -> (u32, usize) {
    let (mut num, mut scale) = (0u64, 1u64);
    let mut dot = false;
    let mut at = 0usize;
    loop {
        match cp.get(at) {
            Some(b'.') if !dot => {
                at += 1;
                dot = true;
            }
            Some(b'%') => {
                scale = if dot { scale * 100 } else { 100 };
                at += 1; // `%` is always at the end
                break;
            }
            Some(c @ b'0'..=b'9') => {
                if scale < 100_000 {
                    scale *= 10;
                    num = num * 10 + u64::from(c - b'0');
                }
                at += 1;
            }
            _ => break,
        }
    }
    let score = if num >= scale {
        MAX_SCORE
    } else {
        MAX_SCORE * num as f64 / scale as f64
    };
    (score as u32, at)
}

/// `-Xrenormalize`/`-Xno-renormalize` override the `merge.renormalize` config
/// that decides the blob pipeline mode, which `Repository::merge_resource_cache`
/// reads from the repository — so the override goes into an in-memory config
/// layer on a private clone of the repo. `None` means there was nothing to
/// override and the caller's own repo stands.
pub(crate) fn renormalized_repo(
    repo: &gix::Repository,
    xopts: &StrategyOptions,
) -> Result<Option<gix::Repository>> {
    let Some(on) = xopts.renormalize else {
        return Ok(None);
    };
    let mut with_override = repo.clone();
    {
        let mut config = with_override.config_snapshot_mut();
        config.append_config(Some(format!("merge.renormalize={on}")), gix::config::Source::Cli)?;
        config.commit()?;
    }
    Ok(Some(with_override))
}

/// merge-ort's diff algorithm for a merge with no `-X` of its own.
///
/// `init_merge_options()` opens with `opt->xdl_opts = DIFF_WITH_ALG(opt,
/// HISTOGRAM_DIFF)` (merge-ort.c:5502), so **every** merge is a histogram diff
/// unless something moves it — not a Myers one, which is what gitoxide's
/// configuration cache defaults to and what this port inherited on every merge
/// that did not spell an algorithm out. On a fixture where the two disagree,
/// stock 2.55.0 with no configuration writes the same conflicted blob as
/// `-Xhistogram` and this port wrote `-Xdiff-algorithm=myers`'s.
///
/// `diff.algorithm` then replaces it **only for the `ui` callers**
/// (merge-ort.c:5472-5480) — `init_ui_merge_options()`, used by `git merge`
/// (builtin/merge.c:814), `cherry-pick`/`revert`/`rebase` (sequencer.c:764,
/// 4348), `am` (builtin/am.c:1640), `stash` (builtin/stash.c:696) and
/// `log --remerge-diff` (log-tree.c:1052). `merge-tree`
/// (builtin/merge-tree.c:593) and `merge-recursive`/`merge-subtree`
/// (builtin/merge-recursive.c:37) take `init_basic_merge_options()` and never
/// read the key at all: measured, stock's `merge-recursive` writes the same
/// bytes under `diff.algorithm=patience` as under no configuration, while
/// `git merge` writes different ones.
///
/// An unparseable value is left alone here. git rejects it in the configuration
/// layer, before any verb runs (`error: unknown value for config
/// 'diff.algorithm': …` plus a `fatal:` naming the config source, exit 128),
/// which is not this function's to reproduce.
pub(crate) fn merge_diff_algorithm(repo: &gix::Repository, ui: bool) -> Algorithm {
    if !ui {
        return Algorithm::Histogram;
    }
    repo.config_snapshot()
        .string("diff.algorithm")
        .and_then(|v| parse_algorithm_value(&v.to_str_lossy()))
        .unwrap_or(Algorithm::Histogram)
}

/// `Repository::tree_merge_options()` with the `-X` knobs folded in.
///
/// Shared with the strategy *plumbing* (`git merge-recursive` and its three
/// alias names), which receives the same options — `try_merge_command()`
/// re-spells each `-X<value>` as `--<value>` on their command line, and
/// `cmd_merge_recursive` feeds them to the very same `parse_merge_opt()`
/// (builtin/merge-recursive.c:55-58). `ui` is the one thing that does *not*
/// carry across: see [`merge_diff_algorithm`].
pub(crate) fn tree_merge_options(
    repo: &gix::Repository,
    xopts: &StrategyOptions,
    style: Option<ConflictStyle>,
    ui: bool,
) -> Result<gix::merge::tree::Options> {
    use gix::merge::plumbing::blob::builtin_driver::{binary, text};

    let mut opts: gix::merge::plumbing::tree::Options = repo.tree_merge_options()?.into();

    // Set unconditionally: leaving it to gitoxide's configuration cache is what
    // made an unconfigured merge a Myers merge, and a `diff.algorithm=patience`
    // one a histogram merge (the cache reports patience as unimplemented and
    // silently falls back, even though this tree's `gix-imara-diff` has it).
    opts.blob_merge.text.diff_algorithm = xopts
        .diff_algorithm
        .unwrap_or_else(|| merge_diff_algorithm(repo, ui));

    // `--conflict=<style>` is git's per-invocation override of
    // `merge.conflictStyle` (`git_xmerge_config()` seeds the default;
    // `parse_opt_conflict()` replaces it for this run only). Only the style is
    // replaced — `conflict.marker_size`, which `merge.conflictStyle` never
    // touches, keeps whatever the configuration gave it.
    //
    // Both fields have to move. `blob_merge_options()` fills
    // `text.style` from the configuration (gix/src/repository/merge.rs:82) and
    // `Rendering::style()` prefers that field over the one inside
    // `Conflict::Keep` (gix-merge/src/blob/builtin_driver/text/mod.rs:169-176),
    // so patching only `Conflict::Keep` left the configured value winning and
    // the flag inert. Leaving `style` at `None` still lets the configured value
    // through untouched, which is what keeps `-c merge.conflictStyle=diff3`
    // working.
    if let Some(style) = style {
        if let text::Conflict::Keep { style: current, .. } = &mut opts.blob_merge.text.conflict {
            *current = style;
        }
        opts.blob_merge.text.style = Some(style);
    }

    // `xpp.flags & XDF_WHITESPACE_FLAGS`: the text driver interns lines by the
    // class `xdl_recmatch()` would put them in, so the rule arrives as the
    // canonical image rather than as a comparison. See `crate::merge_ws`.
    opts.blob_merge.text.canonicalize = crate::merge_ws::canonicalize_for(xopts.xdl_opts);

    // `recursive_variant`: merge-ort resolves *every* conflicting hunk, binary
    // blob and symlink towards the chosen side rather than writing markers.
    if let Some(variant) = xopts.variant {
        let (binary, text) = match variant {
            Variant::Ours => (binary::ResolveWith::Ours, text::Conflict::ResolveWithOurs),
            Variant::Theirs => (binary::ResolveWith::Theirs, text::Conflict::ResolveWithTheirs),
        };
        opts.blob_merge.resolve_binary_with = Some(binary);
        opts.blob_merge.text.conflict = text;
        opts.symlink_conflicts = Some(binary);
    }

    match xopts.detect_renames {
        Some(false) => opts.rewrites = None,
        Some(true) => {
            let mut rewrites = opts.rewrites.unwrap_or_default();
            // `diffcore_rename()` reads an unset (`0`) score as DEFAULT_RENAME_SCORE.
            let score = if xopts.rename_score == 0 {
                DEFAULT_RENAME_SCORE
            } else {
                f64::from(xopts.rename_score)
            };
            rewrites.percentage = Some((score / MAX_SCORE) as f32);
            opts.rewrites = Some(rewrites);
        }
        None => {}
    }

    Ok(opts.into())
}

// ---------------------------------------------------------------------------
// `-Xsubtree[=<prefix>]` — a port of match-trees.c
// ---------------------------------------------------------------------------

/// One entry of a raw tree object, with the offset of its binary object id so
/// that [`splice_tree`] can overwrite it in place the way git's C does.
struct RawEntry<'a> {
    mode: u32,
    name: &'a [u8],
    id: ObjectId,
    id_at: usize,
}

/// `S_ISDIR()` on a tree entry mode.
fn is_dir(mode: u32) -> bool {
    mode & 0o170000 == 0o040000
}

/// `S_ISLNK()` on a tree entry mode.
fn is_lnk(mode: u32) -> bool {
    mode & 0o170000 == 0o120000
}

/// `fill_tree_desc_strict()`: the raw bytes of a tree, or git's own diagnostics.
fn read_tree(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let object = repo
        .find_object(id)
        .map_err(|_| anyhow!("unable to read tree ({id})"))?;
    if object.kind != gix::object::Kind::Tree {
        anyhow::bail!("{id} is not a tree");
    }
    Ok(object.data.clone())
}

/// Split a raw tree object into its entries, in stored (`base_name_compare`) order.
fn parse_tree<'a>(data: &'a [u8], hash_len: usize) -> Result<Vec<RawEntry<'a>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let space = data[at..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| anyhow!("corrupt tree object"))?
            + at;
        let mode = std::str::from_utf8(&data[at..space])
            .ok()
            .and_then(|s| u32::from_str_radix(s, 8).ok())
            .ok_or_else(|| anyhow!("corrupt tree object"))?;
        let nul = data[space + 1..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("corrupt tree object"))?
            + space
            + 1;
        let id_at = nul + 1;
        if id_at + hash_len > data.len() {
            anyhow::bail!("corrupt tree object");
        }
        out.push(RawEntry {
            mode,
            name: &data[space + 1..nul],
            id: ObjectId::from_bytes_or_panic(&data[id_at..id_at + hash_len]),
            id_at,
        });
        at = id_at + hash_len;
    }
    Ok(out)
}

/// `base_name_compare()` (read-cache.c): plain byte order, except that a tree
/// entry sorts as though its name ended in `/`.
fn base_name_compare(name1: &[u8], mode1: u32, name2: &[u8], mode2: u32) -> std::cmp::Ordering {
    let len = name1.len().min(name2.len());
    let cmp = name1[..len].cmp(&name2[..len]);
    if cmp != std::cmp::Ordering::Equal {
        return cmp;
    }
    let byte_at = |name: &[u8], mode: u32| -> u8 {
        match name.get(len) {
            Some(&c) => c,
            None if is_dir(mode) => b'/',
            None => 0,
        }
    };
    byte_at(name1, mode1).cmp(&byte_at(name2, mode2))
}

/// `score_missing()`.
fn score_missing(mode: u32) -> i32 {
    if is_dir(mode) {
        -1000
    } else if is_lnk(mode) {
        -500
    } else {
        -50
    }
}

/// `score_differs()`.
fn score_differs(mode1: u32, mode2: u32) -> i32 {
    if is_dir(mode1) != is_dir(mode2) {
        -100
    } else if is_lnk(mode1) != is_lnk(mode2) {
        -50
    } else {
        -5
    }
}

/// `score_matches()`.
fn score_matches(mode1: u32, mode2: u32) -> i32 {
    if is_dir(mode1) != is_dir(mode2) {
        -100
    } else if is_lnk(mode1) != is_lnk(mode2) {
        -50
    } else if is_dir(mode1) {
        1000
    } else if is_lnk(mode1) {
        500
    } else {
        250
    }
}

/// `score_trees()`: how similar two trees are, one shared merge walk deep.
fn score_trees(repo: &gix::Repository, hash1: ObjectId, hash2: ObjectId) -> Result<i32> {
    let hash_len = repo.object_hash().len_in_bytes();
    let one_buf = read_tree(repo, hash1)?;
    let two_buf = read_tree(repo, hash2)?;
    let one = parse_tree(&one_buf, hash_len)?;
    let two = parse_tree(&two_buf, hash_len)?;

    let (mut i, mut j) = (0usize, 0usize);
    let mut score = 0i32;
    loop {
        let cmp = match (one.get(i), two.get(j)) {
            (Some(a), Some(b)) => base_name_compare(a.name, a.mode, b.name, b.mode),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        match cmp {
            std::cmp::Ordering::Less => {
                score += score_missing(one[i].mode);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                score += score_missing(two[j].mode);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                score += if one[i].id == two[j].id {
                    score_matches(one[i].mode, two[j].mode)
                } else {
                    score_differs(one[i].mode, two[j].mode)
                };
                i += 1;
                j += 1;
            }
        }
    }
    Ok(score)
}

/// `match_trees()`: find the subtree of `hash1` that best resembles `hash2`,
/// recording its path in `best_match`.
fn match_trees(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    best_score: &mut i32,
    best_match: &mut BString,
    base: &BStr,
    recurse_limit: u32,
) -> Result<()> {
    let hash_len = repo.object_hash().len_in_bytes();
    let one_buf = read_tree(repo, hash1)?;
    for entry in parse_tree(&one_buf, hash_len)? {
        if !is_dir(entry.mode) {
            continue;
        }
        let score = score_trees(repo, entry.id, hash2)?;
        if *best_score < score {
            let mut path = BString::from(base.to_vec());
            path.extend_from_slice(entry.name);
            *best_match = path;
            *best_score = score;
        }
        if recurse_limit > 0 {
            let mut newbase = BString::from(base.to_vec());
            newbase.extend_from_slice(entry.name);
            newbase.push(b'/');
            match_trees(
                repo,
                entry.id,
                hash2,
                best_score,
                best_match,
                newbase.as_ref(),
                recurse_limit - 1,
            )?;
        }
    }
    Ok(())
}

/// `splice_tree()`: `oid1` has a subdirectory at `prefix`; write out the tree
/// that results from replacing it with `oid2`.
///
/// Like the C, this rewrites the object id in place in the raw tree buffer, so
/// entry order, modes and padding are preserved byte for byte.
fn splice_tree(
    repo: &gix::Repository,
    oid1: ObjectId,
    prefix: &BStr,
    oid2: ObjectId,
) -> Result<ObjectId> {
    let hash_len = repo.object_hash().len_in_bytes();
    let (top, subpath) = match prefix.iter().position(|&b| b == b'/') {
        Some(at) => (&prefix[..at], &prefix[at + 1..]),
        None => (&prefix[..], &prefix[prefix.len()..]),
    };

    let mut buf = read_tree(repo, oid1).map_err(|_| anyhow!("cannot read tree {oid1}"))?;
    let found = parse_tree(&buf, hash_len)?
        .into_iter()
        .find(|e| e.name == top)
        .map(|e| (e.mode, e.id, e.id_at));
    let (mode, current, id_at) = found.ok_or_else(|| {
        anyhow!(
            "entry {} not found in tree {oid1}",
            top.as_bstr()
        )
    })?;
    if !is_dir(mode) {
        anyhow::bail!("entry {} in tree {oid1} is not a tree", top.as_bstr());
    }

    let rewrite_with = if subpath.is_empty() {
        oid2
    } else {
        splice_tree(repo, current, subpath.as_bstr(), oid2)?
    };
    buf[id_at..id_at + hash_len].copy_from_slice(rewrite_with.as_bytes());

    use gix::objs::Write;
    repo.objects
        .write_buf(gix::object::Kind::Tree, &buf)
        .map_err(|e| anyhow!("unable to write tree: {e}"))
}

/// `get_tree_entry()`: resolve a slash-separated path inside a tree.
fn get_tree_entry(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &BStr,
) -> Result<Option<(ObjectId, u32)>> {
    let hash_len = repo.object_hash().len_in_bytes();
    // `find_tree_entry()` (tree-walk.c) consumes one trailing `/`: after a
    // directory entry matches, a remainder of exactly `/` ends the walk on that
    // entry (`if (++entrylen == namelen)`), which it reaches only past
    // `if (!S_ISDIR(*mode)) break`, so `src/` names the `src` tree while
    // `file/` names nothing. Every other empty component — leading, or doubled —
    // is a name no tree entry carries, and is left to fail in the walk below.
    let (path, needs_dir) = match path.strip_suffix(b"/") {
        Some(rest) if !rest.is_empty() => (rest.as_bstr(), true),
        _ => (path, false),
    };
    let mut current = tree;
    let mut mode = 0o040000u32;
    for component in path.split(|&b| b == b'/') {
        if !is_dir(mode) {
            return Ok(None);
        }
        let buf = read_tree(repo, current)?;
        let found = parse_tree(&buf, hash_len)?
            .into_iter()
            .find(|e| e.name == component)
            .map(|e| (e.id, e.mode));
        match found {
            Some((id, m)) => {
                current = id;
                mode = m;
            }
            None => return Ok(None),
        }
    }
    if needs_dir && !is_dir(mode) {
        return Ok(None);
    }
    Ok(Some((current, mode)))
}

/// `shift_tree()`: come up with a version of `hash2` whose shape resembles
/// `hash1`, either by burying it under fake directories or by picking a subtree
/// out of it.
fn shift_tree(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    depth_limit: u32,
) -> Result<ObjectId> {
    // "NEEDSWORK: this limits the recursion depth to hardcoded value '2'".
    let depth_limit = if depth_limit == 0 { 2 } else { depth_limit };

    let base = score_trees(repo, hash1, hash2)?;
    let (mut add_score, mut del_score) = (base, base);
    let mut add_prefix = BString::default();
    let mut del_prefix = BString::default();

    // Does a subtree of *one* resemble two? Then two needs prefixing.
    match_trees(
        repo,
        hash1,
        hash2,
        &mut add_score,
        &mut add_prefix,
        BStr::new(b""),
        depth_limit,
    )?;
    // Does a subtree of *two* resemble one? Then pick that subtree out of two.
    match_trees(
        repo,
        hash2,
        hash1,
        &mut del_score,
        &mut del_prefix,
        BStr::new(b""),
        depth_limit,
    )?;

    if add_score < del_score {
        if del_prefix.is_empty() {
            return Ok(hash2);
        }
        return match get_tree_entry(repo, hash2, del_prefix.as_ref())? {
            Some((id, _)) => Ok(id),
            None => Err(anyhow!("cannot find path {del_prefix} in tree {hash2}")),
        };
    }
    if add_prefix.is_empty() {
        return Ok(hash2);
    }
    splice_tree(repo, hash1, add_prefix.as_ref(), hash2)
}

/// `shift_tree_by()`: the user named the prefix, but not which side it applies
/// to, so try both and keep the more plausible one.
fn shift_tree_by(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    shift_prefix: &BStr,
) -> Result<ObjectId> {
    let sub1 = get_tree_entry(repo, hash1, shift_prefix)?.filter(|(_, mode)| is_dir(*mode));
    let sub2 = get_tree_entry(repo, hash2, shift_prefix)?.filter(|(_, mode)| is_dir(*mode));

    let mut candidate = u8::from(sub1.is_some()) | (u8::from(sub2.is_some()) << 1);
    if candidate == 3 {
        let (sub1, sub2) = (sub1.expect("set").0, sub2.expect("set").0);
        let mut best_score = score_trees(repo, hash1, hash2)?;
        candidate = 0;
        let score = score_trees(repo, sub1, hash2)?;
        if score > best_score {
            candidate = 1;
            best_score = score;
        }
        if score_trees(repo, sub2, hash1)? > best_score {
            candidate = 2;
        }
    }

    match candidate {
        // Bury two under `shift_prefix` so it lines up with one.
        1 => splice_tree(repo, hash1, shift_prefix, hash2),
        // Lift `shift_prefix` out of two so it lines up with one.
        2 => Ok(sub2.expect("set").0),
        // Neither is plausible — do not shift.
        _ => Ok(hash2),
    }
}

/// `shift_tree_object()` (merge-ort.c): the entry point `-Xsubtree` uses.
///
/// Shared with `merge-tree`, which runs its own merge rather than going through
/// [`merge_and_apply`] but shifts exactly the same way, because
/// `merge_ort_nonrecursive_internal()` is the single place git shifts from.
pub(crate) fn shift_tree_object(
    repo: &gix::Repository,
    one: ObjectId,
    two: ObjectId,
    subtree_shift: &BStr,
) -> Result<ObjectId> {
    if subtree_shift.is_empty() {
        shift_tree(repo, one, two, 0)
    } else {
        shift_tree_by(repo, one, two, subtree_shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `base_name_compare()`'s only real rule: a tree entry compares as though
    /// its name ended in `/`, which is why `score_trees()` can walk two trees in
    /// lockstep without re-sorting them.
    #[test]
    fn base_name_compare_treats_dirs_as_slash_suffixed() {
        const DIR: u32 = 0o040000;
        const REG: u32 = 0o100644;
        assert_eq!(base_name_compare(b"a", REG, b"a", DIR), std::cmp::Ordering::Less);
        assert_eq!(base_name_compare(b"a", DIR, b"a", REG), std::cmp::Ordering::Greater);
        assert_eq!(base_name_compare(b"a", DIR, b"a", DIR), std::cmp::Ordering::Equal);
        assert_eq!(base_name_compare(b"ab", REG, b"a", REG), std::cmp::Ordering::Greater);
        assert_eq!(base_name_compare(b"a", REG, b"ab", REG), std::cmp::Ordering::Less);
    }

    /// The three scoring tables from `match-trees.c:13-53`, which decide which
    /// way `-Xsubtree` shifts and are therefore what a wrong constant would
    /// silently mis-merge on.
    #[test]
    fn tree_similarity_scores_match_git() {
        const DIR: u32 = 0o040000;
        const LNK: u32 = 0o120000;
        const REG: u32 = 0o100644;

        assert_eq!(score_missing(DIR), -1000);
        assert_eq!(score_missing(LNK), -500);
        assert_eq!(score_missing(REG), -50);

        assert_eq!(score_differs(DIR, REG), -100);
        assert_eq!(score_differs(LNK, REG), -50);
        assert_eq!(score_differs(REG, REG), -5);

        assert_eq!(score_matches(DIR, REG), -100);
        assert_eq!(score_matches(LNK, REG), -50);
        assert_eq!(score_matches(DIR, DIR), 1000);
        assert_eq!(score_matches(LNK, LNK), 500);
        assert_eq!(score_matches(REG, REG), 250);
    }
}
