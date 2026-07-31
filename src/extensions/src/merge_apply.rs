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
//! `-Xsubtree[=<prefix>]` drives.

use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::Algorithm;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};

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
    )
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

    // `-Xrenormalize`/`-Xno-renormalize` override the `merge.renormalize` config
    // that decides the blob pipeline mode, which `Repository::merge_resource_cache`
    // reads from the repository — so the override goes into an in-memory config
    // layer on a private clone of the repo.
    let renormalized;
    let repo = match xopts.renormalize {
        Some(on) => {
            let mut with_override = repo.clone();
            {
                let mut config = with_override.config_snapshot_mut();
                config.append_config(
                    Some(format!("merge.renormalize={on}")),
                    gix::config::Source::Cli,
                )?;
                config.commit()?;
            }
            renormalized = with_override;
            &renormalized
        }
        None => repo,
    };

    let mut merge = repo.merge_trees(
        base_tree,
        ours_tree,
        theirs_tree,
        labels,
        tree_merge_options(repo, xopts)?,
    )?;
    let tree_id = merge.tree.write()?.detach();

    // git's merge-ort emits an `Auto-merging <path>` line for every attempted blob
    // merge, then `CONFLICT (<kind>): Merge conflict in <path>` for the unresolved
    // ones. Trivially-identical changes resolve silently. The lines are collected
    // rather than printed here because merge-ort collects them too — `path_msg()`
    // appends to `opt->priv->output` and only `merge_display_update_messages()`
    // flushes it, which `merge_switch_to_result()` reaches *after* its checkout.
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let mut conflicts: Vec<BString> = Vec::new();
    let mut messages: Vec<String> = Vec::new();
    for conflict in &merge.conflicts {
        let path = conflict.changes_in_resolution().0.location().to_owned();
        if show_msgs && conflict.content_merge().is_some() {
            messages.push(format!("Auto-merging {path}"));
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
        if show_msgs {
            messages.push(format!("CONFLICT ({kind}): Merge conflict in {path}"));
        }
        conflicts.push(path);
    }

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

    for line in messages {
        println!("{line}");
    }

    let mut index = update_worktree_to_tree(repo, old_index, tree_id, should_interrupt)?;
    if !conflicts.is_empty() {
        merge.index_changed_after_applying_conflicts(
            &mut index,
            unresolved,
            gix::merge::tree::apply_index_entries::RemovalMode::Prune,
        );
    }

    Ok(Merged::Applied(Applied {
        tree_id,
        conflicts,
        index,
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
#[derive(Debug)]
pub enum StrategyOptionError {
    /// git's own `parse_merge_opt()` rejects it.
    Unknown(String),
    /// git accepts it, but the vendored merge engine has no knob for it, and
    /// quietly merging without it would change the merge result.
    Unsupported {
        /// The `-X` value as spelled on the command line.
        spec: String,
        /// What is missing, in the phrasing of the module that lacks it.
        reason: &'static str,
    },
}

impl std::fmt::Display for StrategyOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `die(_("unknown strategy option: -X%s"), …)` in builtin/merge.c.
            StrategyOptionError::Unknown(spec) => write!(f, "unknown strategy option: -X{spec}"),
            StrategyOptionError::Unsupported { spec, reason } => {
                write!(f, "strategy option -X{spec} is unsupported ({reason})")
            }
        }
    }
}

/// The vendored `imara-diff` has Myers, minimal and histogram, but no patience.
const NO_PATIENCE: &str = "the vendored imara-diff has no patience implementation, \
                           and substituting another algorithm would change the merge result";

/// git's whitespace-insensitive merge comes from `xdl_recmatch()` hashing and
/// comparing records under `XDF_IGNORE_*`; `gix-merge`'s text driver interns
/// whole lines and has no equivalent.
const NO_WHITESPACE_FLAGS: &str = "the vendored gix-merge text driver has no \
                                   xdl_recmatch whitespace flags";

impl StrategyOptions {
    /// Apply every `-X <value>` in order, as `try_merge_strategy()` does.
    pub fn parse(specs: &[String]) -> std::result::Result<Self, StrategyOptionError> {
        let mut out = StrategyOptions::default();
        for spec in specs {
            out.apply(spec)?;
        }
        Ok(out)
    }

    /// One `parse_merge_opt()` call (merge-ort.c), kept in the C's branch order.
    fn apply(&mut self, s: &str) -> std::result::Result<(), StrategyOptionError> {
        let unknown = || StrategyOptionError::Unknown(s.to_owned());
        let unsupported = |reason| StrategyOptionError::Unsupported {
            spec: s.to_owned(),
            reason,
        };

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
            return Err(unsupported(NO_PATIENCE));
        } else if s == "histogram" {
            self.diff_algorithm = Some(Algorithm::Histogram);
        } else if let Some(arg) = s.strip_prefix("diff-algorithm=") {
            match parse_algorithm_value(arg) {
                Some(Some(algo)) => self.diff_algorithm = Some(algo),
                Some(None) => return Err(unsupported(NO_PATIENCE)),
                None => return Err(unknown()),
            }
        } else if matches!(
            s,
            "ignore-space-change" | "ignore-all-space" | "ignore-space-at-eol" | "ignore-cr-at-eol"
        ) {
            return Err(unsupported(NO_WHITESPACE_FLAGS));
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

/// `parse_algorithm_value()` (diff.c). `None` is git's `-1`; `Some(None)` is
/// patience, which git accepts and this build cannot honour.
fn parse_algorithm_value(value: &str) -> Option<Option<Algorithm>> {
    if value.eq_ignore_ascii_case("myers") || value.eq_ignore_ascii_case("default") {
        Some(Some(Algorithm::Myers))
    } else if value.eq_ignore_ascii_case("minimal") {
        Some(Some(Algorithm::MyersMinimal))
    } else if value.eq_ignore_ascii_case("patience") {
        Some(None)
    } else if value.eq_ignore_ascii_case("histogram") {
        Some(Some(Algorithm::Histogram))
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

/// `Repository::tree_merge_options()` with the `-X` knobs folded in.
fn tree_merge_options(
    repo: &gix::Repository,
    xopts: &StrategyOptions,
) -> Result<gix::merge::tree::Options> {
    use gix::merge::plumbing::blob::builtin_driver::{binary, text};

    let mut opts: gix::merge::plumbing::tree::Options = repo.tree_merge_options()?.into();

    if let Some(algorithm) = xopts.diff_algorithm {
        opts.blob_merge.text.diff_algorithm = algorithm;
    }

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
fn shift_tree_object(
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
