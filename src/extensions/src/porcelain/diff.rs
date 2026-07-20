//! `git diff` — show changes between trees, the index and the worktree.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). Supported invocations:
//!
//! * `git diff`                       — index vs. worktree (unstaged changes)
//! * `git diff --cached [<rev>]`      — `<rev>`-tree (default `HEAD`) vs. the index (staged)
//! * `git diff --staged [<rev>]`      — alias of `--cached`
//! * `git diff <rev>`                 — `<rev>`-tree vs. the worktree
//! * `git diff <revA> <revB>`         — tree vs. tree (also `<revA>..<revB>`)
//!
//! Output formats follow `diff.c`'s model: `--raw`, `--numstat`, `--stat`,
//! `--shortstat`, `--name-only`, `--name-status` and the unified patch can be
//! combined, are emitted in git's fixed order (raw, numstat, stat, shortstat,
//! blank line, patch), and `--name-only`/`--name-status`/`-s` suppress every
//! other format exactly like `diff_setup_done()` does.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * Merge-base ranges (`<revA>...<revB>`) are not supported.
//! * Rename/copy detection is not performed. `--find-renames`/`-M`/`-C` are accepted
//!   (they change nothing on a history without renames) but a real rename still renders
//!   as a deletion plus an addition.
//! * Submodule/gitlink (`160000`) changes are not diffable through the blob pipeline and bail.
//! * Hunk *section headings* (the text after the second `@@`, i.e. the enclosing function)
//!   are not emitted — gitoxide's unified-diff writer does not compute them.
//! * Magic pathspecs (`:(...)`) and glob pathspecs bail; literal path / directory-prefix
//!   filtering is supported.
//! * `git diff` on an unmerged path renders the combined (`--cc`) patch, and only that —
//!   the duplicate stage-2-vs-worktree pair the raw/name/stat formats also report is not
//!   given a `diff --git` section. `--cached` renders git's `* Unmerged path` line.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;

// ---------------------------------------------------------------------------
// output formats — mirrors DIFF_FORMAT_* in diff.h
// ---------------------------------------------------------------------------

const F_RAW: u32 = 1 << 0;
const F_NUMSTAT: u32 = 1 << 1;
const F_DIFFSTAT: u32 = 1 << 2;
const F_SHORTSTAT: u32 = 1 << 3;
const F_NAME: u32 = 1 << 4;
const F_NAME_STATUS: u32 = 1 << 5;
const F_PATCH: u32 = 1 << 6;
const F_NO_OUTPUT: u32 = 1 << 7;

/// How lines are compared, mirroring xdiff's `XDF_*` whitespace flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    Keep,
    /// `-w` / `--ignore-all-space`: every whitespace byte is ignored.
    IgnoreAll,
    /// `-b` / `--ignore-space-change`: runs of whitespace collapse to one space,
    /// trailing whitespace is ignored.
    IgnoreChange,
    /// `--ignore-space-at-eol`: only trailing whitespace is ignored.
    IgnoreAtEol,
}

/// The "new" side of a change.
enum NewSide {
    /// The path no longer exists (a deletion).
    Absent,
    /// A concrete object in the database (tree/index diffs).
    Blob(ObjectId, EntryKind),
    /// Content that must be read from the worktree at this path (worktree diffs).
    Worktree(EntryKind),
}

/// A single file-level change, normalized across all diff sources.
struct Delta {
    path: BString,
    /// `None` means the path did not exist before (an addition).
    old: Option<(ObjectId, EntryKind)>,
    new: NewSide,
    /// An unmerged (conflicted) index entry: rendered as status `U`, counted as
    /// zero changes by the stat formats, and never diffed through the blob pipeline.
    unmerged: bool,
    /// Stage 2 / stage 3 blobs, present only for the combined (`--cc`) patch of an
    /// unmerged worktree path.
    stages: Option<(ObjectId, ObjectId)>,
}

impl Delta {
    fn new_kind(&self) -> Option<EntryKind> {
        match self.new {
            NewSide::Absent => None,
            NewSide::Blob(_, k) | NewSide::Worktree(k) => Some(k),
        }
    }

    fn plain(path: BString, old: Option<(ObjectId, EntryKind)>, new: NewSide) -> Self {
        Delta {
            path,
            old,
            new,
            unmerged: false,
            stages: None,
        }
    }
}

/// Per-delta blob analysis: the new-side object id plus line counts and the
/// rendered hunks (only computed when a patch is actually requested).
struct Analysis {
    new_id: ObjectId,
    added: u32,
    deleted: u32,
    binary: bool,
    /// `None` when the two sides are byte-identical (e.g. a pure mode change).
    hunks: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn diff(args: &[String]) -> Result<ExitCode> {
    let mut cached = false;
    let mut ctx: u32 = 3;
    let mut ws = Whitespace::Keep;
    let mut fmt: u32 = 0;
    let mut raw_positional: Vec<String> = Vec::new();
    let mut trailing_paths: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    for a in args {
        if after_dashdash {
            trailing_paths.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--cached" | "--staged" => cached = true,
            "--raw" => fmt |= F_RAW,
            "--numstat" => fmt |= F_NUMSTAT,
            "--shortstat" => fmt |= F_SHORTSTAT,
            "--stat" => fmt |= F_DIFFSTAT,
            "--name-only" => fmt |= F_NAME,
            "--name-status" => fmt |= F_NAME_STATUS,
            "-p" | "-u" | "--patch" => fmt |= F_PATCH,
            "-s" | "--no-patch" => fmt |= F_NO_OUTPUT,
            "-w" | "--ignore-all-space" => ws = Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => ws = Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => ws = Whitespace::IgnoreAtEol,
            // Accepted no-ops: these describe behavior zvcs already produces, or
            // (for rename detection) make no difference without renames present.
            "--no-renames" | "--no-color" | "--color=never" | "--ignore-blank-lines"
            | "--ignore-cr-at-eol" | "--find-renames" | "--find-copies" | "-M" | "-C"
            | "--rename-empty" | "--no-rename-empty" | "--text" | "-a" => {}
            s if s.starts_with("--stat=") || s.starts_with("--stat-") => fmt |= F_DIFFSTAT,
            s if s.starts_with("--find-renames=") || s.starts_with("--find-copies=") => {}
            s if s.starts_with("-M") || s.starts_with("-C") => {}
            s if s.starts_with("-U") => ctx = parse_context(&s[2..])?,
            s if s.starts_with("--unified=") => ctx = parse_context(&s["--unified=".len()..])?,
            s if s.starts_with('-') => bail!("unsupported option {s:?}"),
            s => raw_positional.push(s.to_string()),
        }
    }

    // `diff_setup_done()`: --name-only / --name-status / -s are mutually exclusive
    // and, when present, suppress every other output format.
    if (fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT)).count_ones() > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
    if fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT) != 0 {
        fmt &= !(F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_PATCH);
    }
    if fmt == 0 {
        fmt = F_PATCH;
    }

    let repo = gix::discover(".")?;

    // ---- classify positionals into revisions and pathspecs ----------------
    // Leading positionals that resolve as revisions (up to two) are revisions;
    // the first that doesn't is a pathspec — and if it is not an existing path
    // either, git refuses with the "ambiguous argument" fatal.
    let mut revs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = trailing_paths;
    let mut in_rev_region = true;
    for tok in raw_positional {
        if in_rev_region && tok.contains("...") && looks_like_range(&tok) {
            bail!("merge-base ranges (<a>...<b>) are not supported");
        }
        if in_rev_region && tok.contains("..") && looks_like_range(&tok) {
            let (a, b) = tok.split_once("..").expect("checked contains");
            revs.push(if a.is_empty() { "HEAD".into() } else { a.into() });
            revs.push(if b.is_empty() { "HEAD".into() } else { b.into() });
            continue;
        }
        if in_rev_region && revs.len() < 2 && repo.rev_parse_single(tok.as_str()).is_ok() {
            revs.push(tok);
        } else {
            if std::fs::symlink_metadata(&tok).is_err() {
                eprintln!(
                    "fatal: ambiguous argument '{tok}': unknown revision or path not in the working tree."
                );
                eprintln!("Use '--' to separate paths from revisions, like this:");
                eprintln!("'git <command> [<revision>...] -- [<file>...]'");
                return Ok(ExitCode::from(128));
            }
            in_rev_region = false;
            paths.push(tok);
        }
    }

    for p in &paths {
        if p.starts_with(':') || p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("magic/glob pathspecs are not supported, got {p:?}");
        }
    }

    if revs.len() > 2 {
        bail!("at most two revisions may be given, got {}", revs.len());
    }

    // ---- collect the normalized change list -------------------------------
    let hash_kind = repo.object_hash();
    let mut deltas: Vec<Delta> = Vec::new();
    let mut worktree_mode = false;
    let mut cache;

    if cached {
        if revs.len() == 2 {
            bail!("--cached with two revisions is not supported");
        }
        collect_tree_index(&repo, revs.first(), &mut deltas)?;
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 2 {
        let old_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
        let new_tree = repo.rev_parse_single(revs[1].as_str())?.object()?.peel_to_tree()?;
        let changes =
            repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(gix::diff::Options::default()))?;
        for change in changes {
            collect_tree_change(change, &mut deltas)?;
        }
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else {
        let workdir = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?
            .to_owned();
        if revs.len() == 1 {
            collect_tree_worktree(&repo, &revs[0], &paths, &mut deltas)?;
        } else {
            collect_index_worktree(&repo, &workdir, &paths, &mut deltas)?;
        }
        cache = repo.diff_resource_cache(
            Mode::ToGit,
            WorktreeRoots {
                old_root: None,
                new_root: Some(workdir.clone()),
            },
        )?;
        worktree_mode = true;
    }

    // For tree/index sources, apply literal pathspec filtering here (the worktree
    // iterators already filtered via `patterns`).
    if !worktree_mode && !paths.is_empty() {
        deltas.retain(|d| paths.iter().any(|p| path_matches(&d.path, p)));
    }

    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // ---- analyze every delta once -----------------------------------------
    let workdir = repo.workdir().map(|p| p.to_owned());
    let want_patch = fmt & F_PATCH != 0;
    let mut analyses: Vec<Analysis> = Vec::with_capacity(deltas.len());
    for delta in &deltas {
        analyses.push(analyze(
            &mut cache,
            &repo.objects,
            delta,
            ctx,
            ws,
            hash_kind,
            workdir.as_deref(),
            want_patch,
        )?);
    }

    // ---- render, in `diff_flush()` order ----------------------------------
    // `diff_flush()` bails out before printing anything at all when the change
    // queue is empty, so even `--shortstat` stays silent on a clean tree.
    let mut out: Vec<u8> = Vec::new();
    let mut separator = false;
    if !deltas.is_empty() {
        if fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
            for delta in &deltas {
                if fmt & (F_RAW | F_NAME_STATUS) != 0 {
                    render_raw(&mut out, delta, fmt, hash_kind);
                } else {
                    out.extend_from_slice(&quoted_name(&delta.path));
                    out.push(b'\n');
                }
            }
            separator = true;
        }

        if fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT) != 0 {
            if fmt & F_NUMSTAT != 0 {
                render_numstat(&mut out, &deltas, &analyses);
            }
            if fmt & F_DIFFSTAT != 0 {
                render_stat(&mut out, &deltas, &analyses);
            }
            if fmt & F_SHORTSTAT != 0 {
                render_shortstat(&mut out, &deltas, &analyses);
            }
            separator = true;
        }

        if fmt & F_PATCH != 0 {
            if separator {
                out.push(b'\n');
            }
            // `run_diff_files()` queues an unmerged path twice — once as the `U`
            // pair and once as the ordinary stage-2-vs-worktree modification — and
            // the raw/name/stat formats above print both. The patch format prints
            // only the combined (`--cc`) patch for such a path; the duplicate pair
            // contributes no `diff --git` section of its own.
            let unmerged: BTreeSet<&BString> =
                deltas.iter().filter(|d| d.unmerged).map(|d| &d.path).collect();
            for (delta, an) in deltas.iter().zip(&analyses) {
                if !delta.unmerged && unmerged.contains(&delta.path) {
                    continue;
                }
                render_patch(&mut out, &repo, delta, an, ctx)?;
            }
        }
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// change collection
// ---------------------------------------------------------------------------

/// `<tree>` vs. the index (`--cached`). gitoxide's index diff skips unmerged
/// entries, so those are re-added here the way `do_oneway_diff()` does: a single
/// `U` pair whose old side comes from the tree.
fn collect_tree_index(
    repo: &gix::Repository,
    spec: Option<&String>,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = tree_id_for(repo, spec)?;
    let index = repo.index_or_load_from_head()?;
    let mut gitlink: Option<BString> = None;
    repo.tree_index_status(
        &tree_id,
        &index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            collect_index_change(change, deltas, &mut gitlink);
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;
    if let Some(p) = gitlink {
        bail!("submodule/gitlink change at {p:?} is not supported");
    }

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for path in unmerged_paths(&index) {
        let old = tree_entry(&tree, &path)?;
        deltas.push(Delta {
            path,
            old,
            new: NewSide::Absent,
            unmerged: true,
            stages: None,
        });
    }
    Ok(())
}

/// `<tree>` vs. the worktree. Reproduces `diff-index`: start from the tree-to-index
/// difference, then let the index-to-worktree difference override the "new" side.
fn collect_tree_worktree(
    repo: &gix::Repository,
    spec: &str,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = repo.rev_parse_single(spec)?.object()?.peel_to_tree()?.id;
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();

    // Path -> new side, in index order (the order `diff-index` reports in).
    let mut new_sides: BTreeMap<BString, NewSide> = BTreeMap::new();
    let mut gitlink: Option<BString> = None;

    let iter = repo
        .status(gix::progress::Discard)?
        .head_tree(tree_id)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_iter(patterns)?;

    for item in iter {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let deleted = matches!(change, ChangeRef::Deletion { .. });
                let (loc, _, entry_mode, oid) = change.fields();
                let (location, id) = (loc.to_owned(), oid.to_owned());
                match if deleted { None } else { index_mode_kind(entry_mode) } {
                    Some(EntryKind::Commit) => gitlink = Some(location),
                    Some(k) => {
                        new_sides.insert(location, NewSide::Blob(id, k));
                    }
                    None => {
                        new_sides.insert(location, NewSide::Absent);
                    }
                }
            }
            gix::status::Item::IndexWorktree(item) => {
                if let Some((path, new)) = worktree_new_side(item)? {
                    new_sides.insert(path, new);
                }
            }
        }
    }
    if let Some(p) = gitlink {
        bail!("submodule/gitlink change at {p:?} is not supported");
    }

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for (path, new) in new_sides {
        let old = tree_entry(&tree, &path)?;
        if matches!(old, Some((_, EntryKind::Commit))) {
            bail!("submodule/gitlink change at {path:?} is not supported");
        }
        // A path that neither existed in the tree nor exists now is not a change.
        if old.is_none() && matches!(new, NewSide::Absent) {
            continue;
        }
        // Unchanged content that only travelled through the index is not a change.
        if let (Some((oid, ok)), NewSide::Blob(nid, nk)) = (&old, &new) {
            if oid == nid && ok == nk {
                continue;
            }
        }
        deltas.push(Delta::plain(path, old, new));
    }
    Ok(())
}

/// The index vs. the worktree (plain `git diff`).
fn collect_index_worktree(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let index = repo.index_or_empty()?;
    let conflicts = conflict_stages(&index);
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();
    let iter = repo
        .status(gix::progress::Discard)?
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_index_worktree_iter(patterns)?;

    let mut seen_conflicts: Vec<BString> = Vec::new();
    for item in iter {
        let item = item?;
        if let gix::status::index_worktree::Item::Modification {
            rela_path, status, ..
        } = &item
        {
            if matches!(
                status,
                gix::status::plumbing::index_as_worktree::EntryStatus::Conflict { .. }
            ) {
                if !seen_conflicts.contains(rela_path) {
                    seen_conflicts.push(rela_path.clone());
                }
                continue;
            }
        }
        if let Some((path, new)) = worktree_new_side(item)? {
            // A worktree entry with no index counterpart cannot happen here (the
            // dirwalk is off), so the old side is always the index entry.
            let entry = index
                .entry_by_path(path.as_bstr())
                .ok_or_else(|| anyhow::anyhow!("no index entry for {path:?}"))?;
            let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
            deltas.push(Delta::plain(path, Some((entry.id, old_kind)), new));
        }
    }

    // `run_diff_files()` reports an unmerged path twice: once as the `U` pair, and
    // once as the ordinary stage-2-vs-worktree modification.
    for path in seen_conflicts {
        let stages = conflicts.get(&path);
        let wt_kind = worktree_kind(workdir, &path);
        deltas.push(Delta {
            path: path.clone(),
            old: None,
            new: match wt_kind {
                Some(k) => NewSide::Worktree(k),
                None => NewSide::Absent,
            },
            unmerged: true,
            stages: stages.map(|s| (s.ours.0, s.theirs.0)),
        });
        if let (Some(s), Some(k)) = (stages, wt_kind) {
            deltas.push(Delta::plain(path, Some((s.ours.0, s.ours.1)), NewSide::Worktree(k)));
        }
    }
    Ok(())
}

/// The "new" side an index-vs-worktree status item implies, or `None` when the
/// item carries no textual change.
fn worktree_new_side(
    item: gix::status::index_worktree::Item,
) -> Result<Option<(BString, NewSide)>> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let Item::Modification {
        entry,
        rela_path,
        status,
        ..
    } = item
    else {
        // Untracked/ignored entries never appear in `git diff` (the dirwalk is off),
        // and rename tracking is disabled.
        return Ok(None);
    };
    let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
    if matches!(old_kind, EntryKind::Commit) {
        // Submodule content change; `git diff` renders this specially. Skip.
        return Ok(None);
    }
    Ok(match status {
        EntryStatus::Change(Change::Modification {
            executable_bit_changed,
            ..
        }) => {
            let new_kind = if executable_bit_changed {
                toggle_exec(old_kind)
            } else {
                old_kind
            };
            Some((rela_path, NewSide::Worktree(new_kind)))
        }
        EntryStatus::Change(Change::Removed) => Some((rela_path, NewSide::Absent)),
        EntryStatus::Change(Change::Type { .. }) => {
            bail!("type change at {rela_path:?} is not supported")
        }
        // A conflicted path still has worktree content; only `git diff` with no
        // revision treats it specially, and that caller intercepts it first.
        EntryStatus::Conflict { .. } => Some((rela_path, NewSide::Worktree(old_kind))),
        // Submodule content modification, intent-to-add, and stat-only refreshes
        // produce no textual diff.
        EntryStatus::Change(Change::SubmoduleModification(_))
        | EntryStatus::IntentToAdd
        | EntryStatus::NeedsUpdate(_) => None,
    })
}

/// The stage 2 ("ours") and stage 3 ("theirs") blobs of a conflicted path.
struct Stages {
    ours: (ObjectId, EntryKind),
    theirs: (ObjectId, EntryKind),
}

fn conflict_stages(index: &gix::index::State) -> BTreeMap<BString, Stages> {
    let mut per_path: BTreeMap<BString, [Option<(ObjectId, EntryKind)>; 2]> = BTreeMap::new();
    for entry in index.entries() {
        let slot = match entry.stage() {
            gix::index::entry::Stage::Ours => 0,
            gix::index::entry::Stage::Theirs => 1,
            _ => continue,
        };
        let kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
        per_path
            .entry(entry.path(index).to_owned())
            .or_default()[slot] = Some((entry.id, kind));
    }
    per_path
        .into_iter()
        .filter_map(|(path, [ours, theirs])| {
            Some((
                path,
                Stages {
                    ours: ours?,
                    theirs: theirs?,
                },
            ))
        })
        .collect()
}

/// Every path with at least one non-zero stage, in index order.
fn unmerged_paths(index: &gix::index::State) -> Vec<BString> {
    let mut out: Vec<BString> = Vec::new();
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path(index).to_owned();
        if out.last() != Some(&path) {
            out.push(path);
        }
    }
    out
}

fn worktree_kind(workdir: &std::path::Path, path: &BString) -> Option<EntryKind> {
    let full = workdir.join(gix::path::from_bstr(path.as_bstr()));
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.is_symlink() {
        return Some(EntryKind::Link);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return Some(EntryKind::BlobExecutable);
        }
    }
    Some(EntryKind::Blob)
}

fn tree_entry(tree: &gix::Tree<'_>, path: &BString) -> Result<Option<(ObjectId, EntryKind)>> {
    let components: Vec<&[u8]> = path.as_slice().split(|b| *b == b'/').collect();
    let entry = tree.lookup_entry(components)?;
    Ok(entry.map(|e| (e.object_id(), e.mode().kind())))
}

/// A single revision spec into a tree id, defaulting to `HEAD^{tree}` (or the empty
/// tree if `HEAD` is unborn) when no spec is given.
fn tree_id_for(repo: &gix::Repository, spec: Option<&String>) -> Result<ObjectId> {
    Ok(match spec {
        Some(s) => repo.rev_parse_single(s.as_str())?.object()?.peel_to_tree()?.id,
        None => repo.head_tree_id_or_empty()?.detach(),
    })
}

/// `true` if a token looks like a revision range rather than a filename that merely
/// contains `..` (e.g. `../foo`). Ranges don't contain `/` and don't start with `.`.
fn looks_like_range(tok: &str) -> bool {
    !tok.starts_with('.') && !tok.contains('/')
}

fn parse_context(s: &str) -> Result<u32> {
    s.parse::<u32>().map_err(|_| anyhow::anyhow!("invalid context line count {s:?}"))
}

/// Convert an index-entry mode into an [`EntryKind`], or `None` for tree entries.
fn index_mode_kind(mode: gix::index::entry::Mode) -> Option<EntryKind> {
    mode.to_tree_entry_mode().map(|m| m.kind())
}

/// Record a change from a tree-vs-index diff, flagging gitlinks for the caller
/// to reject (the blob pipeline cannot diff `160000` entries).
fn collect_index_change(
    change: gix::diff::index::ChangeRef<'_, '_>,
    deltas: &mut Vec<Delta>,
    gitlink: &mut Option<BString>,
) {
    use gix::diff::index::ChangeRef;
    let is_gitlink = |k: Option<EntryKind>| matches!(k, Some(EntryKind::Commit));
    match change {
        ChangeRef::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = index_mode_kind(entry_mode);
            if is_gitlink(k) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let Some(k) = k {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    None,
                    NewSide::Blob(id.into_owned(), k),
                ));
            }
        }
        ChangeRef::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = index_mode_kind(entry_mode);
            if is_gitlink(k) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let Some(k) = k {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((id.into_owned(), k)),
                    NewSide::Absent,
                ));
            }
        }
        ChangeRef::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            let ok = index_mode_kind(previous_entry_mode);
            let nk = index_mode_kind(entry_mode);
            if is_gitlink(ok) || is_gitlink(nk) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let (Some(ok), Some(nk)) = (ok, nk) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((previous_id.into_owned(), ok)),
                    NewSide::Blob(id.into_owned(), nk),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeRef::Rewrite { .. } => {}
    }
}

/// Record a change from a tree-vs-tree diff.
fn collect_tree_change(
    change: gix::object::tree::diff::ChangeDetached,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = entry_mode.kind();
            reject_gitlink(k, &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, None, NewSide::Blob(id, k)));
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = entry_mode.kind();
            reject_gitlink(k, &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, Some((id, k)), NewSide::Absent));
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            reject_gitlink(entry_mode.kind(), &location)?;
            reject_gitlink(previous_entry_mode.kind(), &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(
                    location,
                    Some((previous_id, previous_entry_mode.kind())),
                    NewSide::Blob(id, entry_mode.kind()),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeDetached::Rewrite { .. } => {}
    }
    Ok(())
}

fn reject_gitlink(k: EntryKind, location: &BString) -> Result<()> {
    if matches!(k, EntryKind::Commit) {
        bail!("submodule/gitlink change at {location:?} is not supported");
    }
    Ok(())
}

fn toggle_exec(k: EntryKind) -> EntryKind {
    match k {
        EntryKind::Blob => EntryKind::BlobExecutable,
        EntryKind::BlobExecutable => EntryKind::Blob,
        other => other,
    }
}

/// `true` if `path` equals `pat` or lives under the directory `pat`.
fn path_matches(path: &BString, pat: &str) -> bool {
    let pat = pat.trim_end_matches('/').as_bytes();
    let path = path.as_slice();
    path == pat || (path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/')
}

// ---------------------------------------------------------------------------
// blob analysis
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn analyze(
    cache: &mut gix::diff::blob::Platform,
    objects: &gix::OdbHandle,
    delta: &Delta,
    ctx: u32,
    ws: Whitespace,
    hash_kind: gix::hash::Kind,
    workdir: Option<&std::path::Path>,
    want_patch: bool,
) -> Result<Analysis> {
    let null = hash_kind.null();
    if delta.unmerged {
        return Ok(Analysis {
            new_id: null,
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
        });
    }

    let path = delta.path.as_bstr();
    let old_kind = delta.old.map(|(_, k)| k).unwrap_or(EntryKind::Blob);
    match delta.old {
        Some((id, k)) => cache.set_resource(id, k, path, ResourceKind::OldOrSource, objects)?,
        None => cache.set_resource(null, old_kind, path, ResourceKind::OldOrSource, objects)?,
    };
    match &delta.new {
        NewSide::Blob(id, k) => {
            cache.set_resource(*id, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Worktree(k) => {
            // With `new_root` set on the cache, a null id reads from the worktree by path.
            cache.set_resource(null, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Absent => {
            cache.set_resource(null, old_kind, path, ResourceKind::NewOrDestination, objects)?;
        }
    };

    let prep = cache.prepare_diff()?;

    let new_id: ObjectId = match &delta.new {
        NewSide::Absent => null,
        NewSide::Blob(id, _) => *id,
        NewSide::Worktree(_) => {
            if !prep.new.id.is_null() {
                prep.new.id.to_owned()
            } else if let Some(buf) = prep.new.data.as_slice() {
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
            } else {
                // Binary worktree content: hash the raw file (filters not applied).
                let base = workdir.ok_or_else(|| anyhow::anyhow!("missing work tree"))?;
                let full = base.join(gix::path::from_bstr(path));
                let bytes = std::fs::read(&full)?;
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
            }
        }
    };

    match prep.operation {
        Operation::SourceOrDestinationIsBinary => Ok(Analysis {
            new_id,
            added: 0,
            deleted: 0,
            binary: true,
            hunks: None,
        }),
        Operation::ExternalCommand { .. } => {
            bail!("external diff drivers are not supported for {path:?}")
        }
        Operation::InternalDiff { algorithm } => {
            let before: Vec<&[u8]> = byte_lines(prep.old.data.as_slice().unwrap_or_default());
            let after: Vec<&[u8]> = byte_lines(prep.new.data.as_slice().unwrap_or_default());
            let mut input: InternedInput<Vec<u8>> = InternedInput::default();
            input.update_before(before.iter().map(|l| normalize(l, ws)));
            input.update_after(after.iter().map(|l| normalize(l, ws)));

            let diff = diff_with_slider_heuristics(algorithm, &input);
            let added = diff.count_additions();
            let deleted = diff.count_removals();
            let hunks = if want_patch && (added != 0 || deleted != 0) {
                let sink = PatchSink {
                    buf: Vec::new(),
                    before: &before,
                    after: &after,
                };
                Some(
                    UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx))
                        .consume()?,
                )
            } else {
                None
            };
            Ok(Analysis {
                new_id,
                added,
                deleted,
                binary: false,
                hunks,
            })
        }
    }
}

/// Split `data` into lines the way `imara_diff::sources::byte_lines` does: the
/// terminator stays attached, and a final line without one is still a line.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = rest.find_byte(b'\n').map_or(rest.len(), |i| i + 1);
        let (line, tail) = rest.split_at(len);
        out.push(line);
        rest = tail;
    }
    out
}

/// The form of a line used for *comparison* only; the original bytes are always
/// what gets printed.
fn normalize(line: &[u8], ws: Whitespace) -> Vec<u8> {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n');
    match ws {
        Whitespace::Keep => line.to_vec(),
        Whitespace::IgnoreAll => line.iter().copied().filter(|b| !is_space(*b)).collect(),
        Whitespace::IgnoreAtEol => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            line[..end].to_vec()
        }
        Whitespace::IgnoreChange => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            let mut out = Vec::with_capacity(end);
            let mut in_space = false;
            for &b in &line[..end] {
                if is_space(b) {
                    in_space = true;
                    continue;
                }
                if in_space {
                    out.push(b' ');
                    in_space = false;
                }
                out.push(b);
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

fn mode_octal(k: Option<EntryKind>) -> String {
    match k {
        None => "000000".to_string(),
        Some(k) => mode_str(k).to_string(),
    }
}

fn mode_str(k: EntryKind) -> &'static str {
    std::str::from_utf8(k.as_octal_str()).unwrap_or("100644")
}

/// `--raw` and `--name-status` (`diff_flush_raw()`).
fn render_raw(out: &mut Vec<u8>, delta: &Delta, fmt: u32, hash_kind: gix::hash::Kind) {
    let status = status_char(delta);
    if fmt & F_NAME_STATUS == 0 {
        let null = hash_kind.null().to_hex_with_len(7).to_string();
        let old_hash = delta
            .old
            .map(|(id, _)| id.to_hex_with_len(7).to_string())
            .unwrap_or_else(|| null.clone());
        // Worktree content has no object id yet, which git reports as all-zero.
        let new_hash = match (&delta.new, delta.unmerged) {
            (NewSide::Blob(id, _), false) => id.to_hex_with_len(7).to_string(),
            _ => null,
        };
        push_str(out, ":");
        push_str(out, &mode_octal(delta.old.map(|(_, k)| k)));
        push_str(out, " ");
        push_str(out, &mode_octal(delta.new_kind()));
        push_str(out, " ");
        push_str(out, &old_hash);
        push_str(out, " ");
        push_str(out, &new_hash);
        push_str(out, " ");
    }
    out.push(status);
    out.push(b'\t');
    out.extend_from_slice(&quoted_name(&delta.path));
    out.push(b'\n');
}

/// `--name-status` letter for a delta.
fn status_char(d: &Delta) -> u8 {
    if d.unmerged {
        return b'U';
    }
    match (&d.old, &d.new) {
        (None, _) => b'A',
        (_, NewSide::Absent) => b'D',
        _ => b'M',
    }
}

/// `--numstat` (`show_numstat()`).
fn render_numstat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) {
    for (d, an) in deltas.iter().zip(analyses) {
        if an.binary {
            push_str(out, "-\t-\t");
        } else {
            push_str(out, &format!("{}\t{}\t", an.added, an.deleted));
        }
        out.extend_from_slice(&quoted_name(&d.path));
        out.push(b'\n');
    }
}

/// `--shortstat` (`show_shortstats()`).
fn render_shortstat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) {
    let (files, adds, dels) = stat_totals(deltas, analyses);
    stat_summary(out, files, adds, dels);
}

fn stat_totals(deltas: &[Delta], analyses: &[Analysis]) -> (u32, u32, u32) {
    let mut files = deltas.len() as u32;
    let (mut adds, mut dels) = (0u32, 0u32);
    for (d, an) in deltas.iter().zip(analyses) {
        if d.unmerged {
            files -= 1;
        } else if !an.binary {
            adds += an.added;
            dels += an.deleted;
        }
    }
    (files, adds, dels)
}

/// `print_stat_summary_inserts_deletes()`.
fn stat_summary(out: &mut Vec<u8>, files: u32, insertions: u32, deletions: u32) {
    if files == 0 {
        push_str(out, " 0 files changed\n");
        return;
    }
    push_str(
        out,
        &format!(" {files} file{} changed", if files == 1 { "" } else { "s" }),
    );
    if insertions != 0 || deletions == 0 {
        push_str(
            out,
            &format!(
                ", {insertions} insertion{}(+)",
                if insertions == 1 { "" } else { "s" }
            ),
        );
    }
    if deletions != 0 || insertions == 0 {
        push_str(
            out,
            &format!(
                ", {deletions} deletion{}(-)",
                if deletions == 1 { "" } else { "s" }
            ),
        );
    }
    out.push(b'\n');
}

fn decimal_width(n: u32) -> i64 {
    let mut w = 1i64;
    let mut n = n / 10;
    while n > 0 {
        w += 1;
        n /= 10;
    }
    w
}

/// `scale_linear()` from `diff.c`.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// `--stat` (`show_stats()`), with git's default 80-column budget.
fn render_stat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) {
    let names: Vec<Vec<u8>> = deltas.iter().map(|d| quoted_name(&d.path)).collect();

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for (i, (d, an)) in deltas.iter().zip(analyses).enumerate() {
        let change = (an.added + an.deleted) as i64;
        max_len = max_len.max(names[i].len() as i64);
        if d.unmerged {
            bin_width = bin_width.max(8); // "Unmerged"
            continue;
        }
        if an.binary {
            let w = 14 + decimal_width(an.added) + decimal_width(an.deleted);
            bin_width = bin_width.max(w);
            number_width = 3;
            continue;
        }
        max_change = max_change.max(change);
    }

    // `width` is `options->stat_width ? options->stat_width : 80` for a plain `--stat`.
    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u32));
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = (width * 3 / 8 - number_width - 6).max(6);
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for (i, (d, an)) in deltas.iter().zip(analyses).enumerate() {
        let (added, deleted) = (an.added as i64, an.deleted as i64);
        // "scale" the filename: overlong names are truncated to "...<tail>".
        let full = &names[i];
        let (prefix, name): (&str, &[u8]) = if name_width < full.len() as i64 {
            let len = name_width - 3;
            let start = full.len() - len.max(0) as usize;
            let tail = &full[start..];
            let tail = match tail.iter().position(|b| *b == b'/') {
                Some(p) => &tail[p..],
                None => tail,
            };
            ("...", tail)
        } else {
            ("", full.as_slice())
        };
        let padding = (name_width - prefix.len() as i64 - name.len() as i64).max(0) as usize;

        push_str(out, " ");
        push_str(out, prefix);
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        push_str(out, " | ");

        if an.binary {
            push_str(out, &format!("{:>width$}", "Bin", width = number_width as usize));
            if added == 0 && deleted == 0 {
                out.push(b'\n');
                continue;
            }
            push_str(out, &format!(" {deleted} -> {added} bytes\n"));
            continue;
        }
        if d.unmerged {
            push_str(out, &format!("{:>width$}", "Unmerged", width = number_width as usize));
            out.push(b'\n');
            continue;
        }

        let (mut add, mut del) = (added, deleted);
        if graph_width <= max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total - del;
            }
        }
        push_str(
            out,
            &format!("{:>width$}", added + deleted, width = number_width as usize),
        );
        if added + deleted != 0 {
            push_str(out, " ");
        }
        out.extend_from_slice(&b"+".repeat(add.max(0) as usize));
        out.extend_from_slice(&b"-".repeat(del.max(0) as usize));
        out.push(b'\n');
    }

    let (files, adds, dels) = stat_totals(deltas, analyses);
    stat_summary(out, files, adds, dels);
}

/// Render one delta as a `git diff` file section into `out`.
fn render_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    an: &Analysis,
    ctx: u32,
) -> Result<()> {
    if delta.unmerged {
        return render_combined(out, repo, delta, ctx);
    }

    let old_hash = delta
        .old
        .map(|(id, _)| id.to_hex_with_len(7).to_string())
        .unwrap_or_else(|| "0000000".to_string());
    let new_hash = if matches!(delta.new, NewSide::Absent) {
        "0000000".to_string()
    } else {
        an.new_id.to_hex_with_len(7).to_string()
    };
    let content_differs = old_hash != new_hash;
    let new_kind = delta.new_kind();

    push_str(out, "diff --git ");
    out.extend_from_slice(&quote_two("a/", &delta.path, "b/", &delta.path));
    out.push(b'\n');

    // File-creation / deletion / mode-change lines.
    match (delta.old, new_kind) {
        (None, Some(nk)) => {
            push_str(out, "new file mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        (Some((_, ok)), None) => {
            push_str(out, "deleted file mode ");
            push_str(out, mode_str(ok));
            out.push(b'\n');
        }
        (Some((_, ok)), Some(nk)) if ok != nk => {
            push_str(out, "old mode ");
            push_str(out, mode_str(ok));
            push_str(out, "\nnew mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        _ => {}
    }

    // The `index <old>..<new>[ <mode>]` line only appears when content differs.
    if content_differs {
        push_str(out, "index ");
        push_str(out, &old_hash);
        push_str(out, "..");
        push_str(out, &new_hash);
        // Trailing mode only for an unchanged-mode modification (not add/delete/mode-change).
        if let (Some((_, ok)), Some(nk)) = (delta.old, new_kind) {
            if ok == nk {
                out.push(b' ');
                push_str(out, mode_str(nk));
            }
        }
        out.push(b'\n');
    }

    let old_label = if delta.old.is_some() {
        quote_one("a/", &delta.path)
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if matches!(delta.new, NewSide::Absent) {
        b"/dev/null".to_vec()
    } else {
        quote_one("b/", &delta.path)
    };

    if an.binary {
        push_str(out, "Binary files ");
        out.extend_from_slice(&old_label);
        push_str(out, " and ");
        out.extend_from_slice(&new_label);
        push_str(out, " differ\n");
    } else if let Some(hunks) = &an.hunks {
        emit_file_line(out, b"--- ", &old_label);
        emit_file_line(out, b"+++ ", &new_label);
        out.extend_from_slice(hunks);
    }
    Ok(())
}

/// `DIFF_SYMBOL_FILEPAIR_{MINUS,PLUS}`: a name containing a space gets a trailing
/// tab so the header stays unambiguously parseable.
fn emit_file_line(out: &mut Vec<u8>, lead: &[u8], label: &[u8]) {
    out.extend_from_slice(lead);
    out.extend_from_slice(label);
    if label.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

// ---------------------------------------------------------------------------
// path quoting (quote.c)
// ---------------------------------------------------------------------------

/// The escape character for `b`, or `None` if it can be emitted verbatim.
/// `Some(0)` means "octal-escape this byte".
fn cq_escape(b: u8) -> Option<u8> {
    match b {
        0x07 => Some(b'a'),
        0x08 => Some(b'b'),
        0x09 => Some(b't'),
        0x0a => Some(b'n'),
        0x0b => Some(b'v'),
        0x0c => Some(b'f'),
        0x0d => Some(b'r'),
        b'"' => Some(b'"'),
        b'\\' => Some(b'\\'),
        // Controls, DEL and (with the default `core.quotePath`) every high byte.
        0x00..=0x1f | 0x7f..=0xff => Some(0),
        _ => None,
    }
}

fn needs_quote(s: &[u8]) -> bool {
    s.iter().any(|b| cq_escape(*b).is_some())
}

/// The escaped body of `s`, without the surrounding double quotes.
fn cq_body(s: &[u8], out: &mut Vec<u8>) {
    for &b in s {
        match cq_escape(b) {
            None => out.push(b),
            Some(0) => {
                out.push(b'\\');
                out.push(((b >> 6) & 0o3) + b'0');
                out.push(((b >> 3) & 0o7) + b'0');
                out.push((b & 0o7) + b'0');
            }
            Some(c) => {
                out.push(b'\\');
                out.push(c);
            }
        }
    }
}

/// `write_name_quoted()`: the path, double-quoted and escaped only if needed.
fn quoted_name(path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(s) {
        return s.to_vec();
    }
    let mut out = vec![b'"'];
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// `quote_two_c_style()` for a single prefixed name (the `---`/`+++` lines).
fn quote_one(prefix: &str, path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(prefix.as_bytes()) && !needs_quote(s) {
        let mut out = prefix.as_bytes().to_vec();
        out.extend_from_slice(s);
        return out;
    }
    let mut out = vec![b'"'];
    cq_body(prefix.as_bytes(), &mut out);
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// The `diff --git <a> <b>` name pair.
fn quote_two(pa: &str, a: &BString, pb: &str, b: &BString) -> Vec<u8> {
    let mut out = quote_one(pa, a);
    out.push(b' ');
    out.extend_from_slice(&quote_one(pb, b));
    out
}

// ---------------------------------------------------------------------------
// combined ("--cc") diff for unmerged worktree paths
// ---------------------------------------------------------------------------

/// One line that a parent had but the merge result does not.
struct LostLine {
    line: Vec<u8>,
    /// Bit `n` set means parent `n` lost this line.
    parent_map: u32,
}

/// One line of the merge result, plus everything the parents lost in front of it.
/// Mirrors `struct sline` in `combine-diff.c`.
#[derive(Default)]
struct SLine {
    /// The line content without its terminator. Empty for the two trailer slots.
    bol: Vec<u8>,
    lost: Vec<LostLine>,
    /// Lines lost by the parent currently being processed, before coalescing.
    plost: Vec<Vec<u8>>,
    /// Bits `0..num_parent` mark parents that lack this line; bit `num_parent`
    /// is `mark` and bit `num_parent + 1` is `no_pre_delete`.
    flag: u32,
    /// Per-parent line number this sline starts at, filled by `combine_diff()`.
    p_lno: [u32; NUM_PARENT],
}

const NUM_PARENT: usize = 2;

/// A combined diff of the two conflict stages against the working-tree file, as
/// `show_combined_diff()` renders it for `git diff` on a conflicted path.
///
/// Port of `show_patch_diff()` / `combine_diff()` / `make_hunks()` / `dump_sline()`
/// from `combine-diff.c`, specialized to the two-parent (stage 2 / stage 3) case.
fn render_combined(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    ctx: u32,
) -> Result<()> {
    let Some((ours, theirs)) = delta.stages else {
        // No stage 2/3 pair to combine (e.g. `--cached`): git prints the notice.
        push_str(out, "* Unmerged path ");
        out.extend_from_slice(&delta.path);
        out.push(b'\n');
        return Ok(());
    };
    let workdir = match repo.workdir() {
        Some(w) => w,
        None => {
            push_str(out, "* Unmerged path ");
            out.extend_from_slice(&delta.path);
            out.push(b'\n');
            return Ok(());
        }
    };
    let result = std::fs::read(workdir.join(gix::path::from_bstr(delta.path.as_bstr())))?;
    let parents = [blob_bytes(repo, ours)?, blob_bytes(repo, theirs)?];

    // Result lines, terminators stripped; a trailing incomplete line still counts.
    let mut cnt = result.iter().filter(|b| **b == b'\n').count();
    if !result.is_empty() && *result.last().expect("non-empty") != b'\n' {
        cnt += 1;
    }
    let mut sline: Vec<SLine> = (0..cnt + 2).map(|_| SLine::default()).collect();
    for (i, line) in byte_lines(&result).into_iter().enumerate() {
        let end = line.len() - usize::from(line.last() == Some(&b'\n'));
        sline[i].bol = line[..end].to_vec();
    }

    let result_lines = byte_lines(&result);
    for (n, parent) in parents.iter().enumerate() {
        let nmask = 1u32 << n;
        let before = byte_lines(parent);
        let mut input: InternedInput<Vec<u8>> = InternedInput::default();
        input.update_before(before.iter().map(|l| l.to_vec()));
        input.update_after(result_lines.iter().map(|l| l.to_vec()));
        // `xdi_diff_outf()` runs with git's default algorithm.
        let diff = diff_with_slider_heuristics(gix::diff::blob::Algorithm::Myers, &input);

        for hunk in diff.hunks() {
            // Removals hang off the result line that follows them, which for both
            // an empty and a non-empty "after" range is `after.start`.
            let bucket = hunk.after.start as usize;
            for i in hunk.before.clone() {
                let line = before[i as usize];
                let end = line.len() - usize::from(line.last() == Some(&b'\n'));
                sline[bucket].plost.push(line[..end].to_vec());
            }
            for i in hunk.after.clone() {
                sline[i as usize].flag |= nmask;
            }
        }

        // Assign per-parent line numbers, coalescing this parent's lost lines in.
        let mut p_lno: u32 = 1;
        for lno in 0..=cnt {
            sline[lno].p_lno[n] = p_lno;
            let fresh = std::mem::take(&mut sline[lno].plost);
            coalesce_lines(&mut sline[lno].lost, fresh, n as u32);
            for ll in &sline[lno].lost {
                if ll.parent_map & nmask != 0 {
                    p_lno += 1;
                }
            }
            if lno < cnt && sline[lno].flag & nmask == 0 {
                p_lno += 1;
            }
        }
        sline[cnt + 1].p_lno[n] = p_lno;
    }

    make_hunks(&mut sline, cnt, ctx);

    // ---- header (`show_combined_header()`) --------------------------------
    push_str(out, "diff --cc ");
    out.extend_from_slice(&quoted_name(&delta.path));
    out.push(b'\n');
    push_str(out, "index ");
    push_str(out, &ours.to_hex_with_len(7).to_string());
    push_str(out, ",");
    push_str(out, &theirs.to_hex_with_len(7).to_string());
    push_str(out, "..");
    // The result lives only in the worktree, so it has no object id.
    push_str(out, &repo.object_hash().null().to_hex_with_len(7).to_string());
    out.push(b'\n');
    emit_file_line(out, b"--- ", &quote_one("a/", &delta.path));
    emit_file_line(out, b"+++ ", &quote_one("b/", &delta.path));

    dump_sline(out, &sline, cnt, ctx);
    Ok(())
}

fn blob_bytes(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    Ok(repo.find_object(id)?.detach().data)
}

/// `coalesce_lines()`: LCS-merge `fresh` (the lines parent `parent` lost) into the
/// already-merged `base`, so a line lost by several parents is shown once.
fn coalesce_lines(base: &mut Vec<LostLine>, fresh: Vec<Vec<u8>>, parent: u32) {
    if fresh.is_empty() {
        return;
    }
    if base.is_empty() {
        *base = fresh
            .into_iter()
            .map(|line| LostLine {
                line,
                parent_map: 1 << parent,
            })
            .collect();
        return;
    }
    let (n, m) = (base.len(), fresh.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    // 0 = BASE, 1 = NEW, 2 = MATCH — the same encoding `combine-diff.c` uses.
    let mut dir = vec![vec![0u8; m + 1]; n + 1];
    for d in dir.iter_mut() {
        d[0] = 0;
    }
    for j in 1..=m {
        dir[0][j] = 1;
    }
    for i in 1..=n {
        for j in 1..=m {
            if base[i - 1].line == fresh[j - 1] {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = 2;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = 1;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = 0;
            }
        }
    }
    let mut merged: Vec<LostLine> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i != 0 || j != 0 {
        match dir[i][j] {
            2 => {
                let mut ll = std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                );
                ll.parent_map |= 1 << parent;
                merged.push(ll);
                i -= 1;
                j -= 1;
            }
            1 => {
                merged.push(LostLine {
                    line: fresh[j - 1].clone(),
                    parent_map: 1 << parent,
                });
                j -= 1;
            }
            _ => {
                merged.push(std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                ));
                i -= 1;
            }
        }
    }
    merged.reverse();
    *base = merged;
}

const ALL_MASK: u32 = (1 << NUM_PARENT) - 1;
const MARK: u32 = 1 << NUM_PARENT;
const NO_PRE_DELETE: u32 = 2 << NUM_PARENT;

fn interesting(sl: &SLine) -> bool {
    sl.flag & ALL_MASK != 0 || !sl.lost.is_empty()
}

/// `adjust_hunk_tail()`.
fn adjust_hunk_tail(sline: &[SLine], hunk_begin: usize, i: usize) -> usize {
    if hunk_begin + 1 <= i && sline[i - 1].flag & ALL_MASK == 0 {
        i - 1
    } else {
        i
    }
}

/// `find_next()`.
fn find_next(sline: &[SLine], i: usize, cnt: usize, look_for_uninteresting: bool) -> usize {
    let mut i = i;
    while i <= cnt {
        let marked = sline[i].flag & MARK != 0;
        if look_for_uninteresting != marked {
            return i;
        }
        i += 1;
    }
    i
}

/// `give_context()`.
fn give_context(sline: &mut [SLine], cnt: usize, context: usize) {
    let mut i = find_next(sline, 0, cnt, false);
    if cnt < i {
        return;
    }
    while i <= cnt {
        let mut j = i.saturating_sub(context);
        while j < i {
            if sline[j].flag & MARK == 0 {
                sline[j].flag |= NO_PRE_DELETE;
            }
            sline[j].flag |= MARK;
            j += 1;
        }
        loop {
            let mut j = find_next(sline, i, cnt, true);
            if cnt < j {
                return;
            }
            let k = find_next(sline, j, cnt, false);
            j = adjust_hunk_tail(sline, i, j);
            if k < j + context {
                while j < k {
                    sline[j].flag |= MARK;
                    j += 1;
                }
                i = k;
                continue;
            }
            i = k;
            let mut j2 = j;
            let end = (j + context).min(cnt + 1);
            while j2 < end {
                sline[j2].flag |= MARK;
                j2 += 1;
            }
            break;
        }
    }
}

/// `make_hunks()` with `dense` set, which is what `--cc` uses.
fn make_hunks(sline: &mut [SLine], cnt: usize, context: u32) {
    let context = context as usize;
    for sl in sline.iter_mut().take(cnt + 1) {
        if interesting(sl) {
            sl.flag |= MARK;
        } else {
            sl.flag &= !MARK;
        }
    }

    // Drop hunks whose every line differs from the same single set of parents:
    // those are changes only one side made, which `--cc` elides.
    let mut i = 0usize;
    while i <= cnt {
        while i <= cnt && sline[i].flag & MARK == 0 {
            i += 1;
        }
        if cnt < i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j <= cnt {
            if sline[j].flag & MARK == 0 {
                // Look past the gap: another marked line within `context` continues it.
                let mut la = adjust_hunk_tail(sline, hunk_begin, j);
                la = (la + context).min(cnt + 1);
                let mut contin = false;
                while la > 0 && j <= la - 1 {
                    la -= 1;
                    if sline[la].flag & MARK != 0 {
                        contin = true;
                        break;
                    }
                }
                if !contin {
                    break;
                }
                j = la;
            }
            j += 1;
        }
        let hunk_end = j;

        let mut same_diff: u32 = 0;
        let mut has_interesting = false;
        for sl in sline.iter().take(hunk_end).skip(i) {
            if has_interesting {
                break;
            }
            let this_diff = sl.flag & ALL_MASK;
            if this_diff != 0 {
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                    break;
                }
            }
            for ll in &sl.lost {
                if has_interesting {
                    break;
                }
                if same_diff == 0 {
                    same_diff = ll.parent_map;
                } else if same_diff != ll.parent_map {
                    has_interesting = true;
                }
            }
        }

        if !has_interesting && same_diff != ALL_MASK {
            for sl in sline.iter_mut().take(hunk_end).skip(hunk_begin) {
                sl.flag &= !MARK;
            }
        }
        i = hunk_end;
    }

    give_context(sline, cnt, context);
}

/// `dump_sline()`.
fn dump_sline(out: &mut Vec<u8>, sline: &[SLine], cnt: usize, context: u32) {
    let mut lno = 0usize;
    loop {
        while lno <= cnt && sline[lno].flag & MARK == 0 {
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt && sline[hunk_end].flag & MARK != 0 {
            hunk_end += 1;
        }
        let mut rlines = hunk_end - lno;
        if cnt < hunk_end {
            rlines -= 1; // pointing at the last delete hunk
        }
        let mut null_context = 0usize;
        if context == 0 {
            for sl in sline.iter().take(hunk_end).skip(lno) {
                if sl.flag & (MARK - 1) == 0 {
                    null_context += 1;
                }
            }
            rlines = rlines.saturating_sub(null_context);
        }

        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        for n in 0..NUM_PARENT {
            let l0 = sline[lno].p_lno[n];
            let l1 = sline[hunk_end].p_lno[n];
            push_str(
                out,
                &format!(" -{l0},{}", l1 as i64 - l0 as i64 - null_context as i64),
            );
        }
        push_str(out, &format!(" +{},{rlines} ", lno + 1));
        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        out.push(b'\n');

        while lno < hunk_end {
            let sl = &sline[lno];
            lno += 1;
            if sl.flag & NO_PRE_DELETE == 0 {
                for ll in &sl.lost {
                    for n in 0..NUM_PARENT {
                        out.push(if ll.parent_map & (1 << n) != 0 { b'-' } else { b' ' });
                    }
                    out.extend_from_slice(&ll.line);
                    out.push(b'\n');
                }
            }
            if cnt < lno {
                break;
            }
            if sl.flag & (MARK - 1) == 0 && context == 0 {
                // Only there to hang lost lines in front of; not shown at -U0.
                continue;
            }
            for n in 0..NUM_PARENT {
                out.push(if sl.flag & (1 << n) != 0 { b'+' } else { b' ' });
            }
            out.extend_from_slice(&sl.bol);
            out.push(b'\n');
        }
    }
}

// ---------------------------------------------------------------------------
// unified-diff hunk sink
// ---------------------------------------------------------------------------

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length when
/// it is 1 and using the pre-hunk line number when it is 0, exactly like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// A [`ConsumeHunk`] sink that renders unified-diff hunks into a byte buffer.
///
/// The tokens the differ compares may be whitespace-normalized (`-w` and friends),
/// so line *content* is taken from the original line tables instead, tracked by the
/// cursors the hunk header establishes.
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.buf.extend_from_slice(b"@@ -");
        self.buf.extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" +");
        self.buf.extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" @@\n");

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                DiffLineKind::Context => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            self.buf.push(marker);
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last line
            // of a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}
