//! `sparse-index.c`: when an index is sparse, when it is full, and the conversions between the
//! two that every read and every write of the index run.
//!
//! git decides at the end of every read (`do_read_index()`, read-cache.c:2335-2341):
//!
//! ```c
//! if (istate->repo->settings.command_requires_full_index)
//!         ensure_full_index(istate);
//! else
//!         ensure_correct_sparsity(istate);
//! ```
//!
//! and again at the start of every write (`do_write_locked_index()`, read-cache.c:3141-3157):
//! `convert_to_sparse()` collapses a full index whenever the repository allows a sparse one, and
//! leaves an index that is already collapsed exactly as it is — entries *and* cache-tree.
//!
//! This port has no command that can work over a sparse-directory entry, so every index is
//! expanded as it is read. One git would keep collapsed is expanded only *virtually*
//! ([`gix::index::State::virtual_sparse_dirs()`]): it stays marked `INDEX_COLLAPSED`, and the
//! write puts its directories back as they were read instead of rebuilding the index from
//! scratch, which is what makes the cache-tree a sparse-aware command leaves behind match
//! git's. [`ensure_full_index`] is git's function of that name for the places a command expands
//! for real.

use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::{BString, ByteSlice};
use gix::index::extension::tree::update as cache_tree;

use crate::porcelain::write_tree::RepoOdb;

/// `the_repository->settings.command_requires_full_index`, which `prepare_repo_settings()`
/// starts at 1 (repo-settings.c:89) and each sparse-aware builtin clears before reading the
/// index.
static COMMAND_REQUIRES_FULL_INDEX: AtomicBool = AtomicBool::new(true);

/// `give_advice_on_expansion` (sparse-index.c:29): the advice is given once per process, and
/// not at all once the process has collapsed an index on purpose.
static GIVE_ADVICE_ON_EXPANSION: AtomicBool = AtomicBool::new(true);

/// git's `ADVICE_MSG` (sparse-index.c:30-37).
const ADVICE_MSG: &str = "\
The sparse index is expanding to a full index, a slow operation.
Your working directory likely has contents that are outside of
your sparse-checkout patterns. Use 'git sparse-checkout list' to
see your sparse-checkout definition and compare it to your working
directory contents. Cleaning up any merge conflicts or staged
changes before running 'git sparse-checkout clean' or 'git
sparse-checkout reapply' may assist in this cleanup.";

/// The builtins that set `command_requires_full_index = 0` before they read the index
/// (`grep -l 'command_requires_full_index = 0' builtin/*.c`). `checkout.c` does it in
/// `checkout_main()`, which `switch` and `restore` share; `commit.c` in both `cmd_status()`
/// and `cmd_commit()`; `revert.c` in `run_sequencer()`, shared by `cherry-pick`; `log.c` only
/// in `cmd_show()`. `describe` is the one conditional: only its `--dirty` branch reads the
/// index, and it clears the setting right before (builtin/describe.c:763).
const SPARSE_AWARE: &[&str] = &[
    "add",
    "apply",
    "blame",
    "cat-file",
    "check-attr",
    "checkout",
    "checkout-index",
    "cherry-pick",
    "clean",
    "commit",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "fetch",
    "grep",
    "ls-files",
    "merge",
    "merge-ours",
    "pull",
    "read-tree",
    "rebase",
    "reset",
    "restore",
    "rev-parse",
    "revert",
    "rm",
    "show",
    "sparse-checkout",
    "stash",
    "status",
    "switch",
    "update-index",
    "worktree",
    "write-tree",
];

/// The three settings every sparse-index decision reads: `cfg->apply_sparse_checkout`,
/// `cfg->core_sparse_checkout_cone` and `r->settings.sparse_index`.
#[derive(Clone, Copy)]
pub struct Settings {
    /// `core.sparseCheckout`.
    pub apply_sparse_checkout: bool,
    /// `core.sparseCheckoutCone`.
    pub cone: bool,
    /// `index.sparse`.
    pub sparse_index: bool,
}

/// The values `sparse-checkout` gave the settings in-process, which outrank the configuration
/// the repository was opened with: git's `set_sparse_index_config()` sets
/// `repo->settings.sparse_index` as it writes `index.sparse` (sparse-index.c:140-148),
/// `disable` clears it outright (builtin/sparse-checkout.c:1083-1084), and the mode switches
/// assign `cfg->core_sparse_checkout_cone` directly.
static SETTINGS_OVERRIDE: std::sync::Mutex<Option<Settings>> = std::sync::Mutex::new(None);

/// Record the settings a `sparse-checkout` subcommand now runs under.
pub fn override_settings(settings: Settings) {
    *SETTINGS_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings);
}

/// The settings in force: an in-process override, else the repository's configuration.
fn settings(repo: &gix::Repository) -> Settings {
    if let Some(s) = *SETTINGS_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) {
        return s;
    }
    let snapshot = repo.config_snapshot();
    Settings {
        apply_sparse_checkout: snapshot.boolean("core.sparseCheckout").unwrap_or(false),
        cone: snapshot.boolean("core.sparseCheckoutCone").unwrap_or(false),
        sparse_index: snapshot.boolean("index.sparse").unwrap_or(false),
    }
}

/// Settle `command_requires_full_index` for the builtin `sub` is about to run.
pub fn note_command(sub: &str, args: &[String]) {
    let aware = SPARSE_AWARE.contains(&sub)
        || (sub == "describe" && args.iter().any(|a| a == "--dirty" || a.starts_with("--dirty=")));
    COMMAND_REQUIRES_FULL_INDEX.store(!aware, Ordering::Relaxed);
    gix::repository::set_post_read_index_hook(post_read_index);
}

/// `give_advice_on_expansion = 0`, as `sparse-checkout disable` sets it
/// (builtin/sparse-checkout.c:1071) before expanding on purpose.
pub fn no_advice_on_expansion() {
    GIVE_ADVICE_ON_EXPANSION.store(false, Ordering::Relaxed);
}

/// The tail of `do_read_index()` (read-cache.c:2335-2341), or for an index file that did not
/// exist, `set_new_index_sparsity()` (read-cache.c:2198-2208) — registered with `gix` so it
/// runs on every read.
fn post_read_index(repo: &gix::Repository, index: &mut gix::index::File, fresh: bool) {
    let requires_full = COMMAND_REQUIRES_FULL_INDEX.load(Ordering::Relaxed);
    if fresh {
        if !requires_full && is_sparse_index_allowed(repo, index) {
            index.set_collapsed();
        }
        return;
    }
    if requires_full {
        ensure_full_index(repo, index);
        return;
    }
    // `ensure_correct_sparsity()` (sparse-index.c:475-484).
    if is_sparse_index_allowed(repo, index) {
        convert_to_sparse(repo, index);
    } else {
        ensure_full_index(repo, index);
    }
    expand_virtually(repo, index);
}

/// Replace each sparse-directory entry by the entries of its tree while keeping the index
/// `INDEX_COLLAPSED`, remembering every directory so the write can put it back.
fn expand_virtually(repo: &gix::Repository, index: &mut gix::index::State) {
    let dirs: Vec<(BString, gix::ObjectId)> = index
        .entries()
        .iter()
        .filter(|e| e.mode.is_sparse())
        .map(|e| (e.path(index).to_owned(), e.id))
        .collect();
    for (path, id) in dirs {
        let Some((dir, subtree)) = expansion_of(repo, path, id) else {
            continue;
        };
        index.expand_sparse_dir_virtually(dir, subtree);
    }
}

/// What `add_path_to_index()` (sparse-index.c:275-328) makes of the sparse directory `path`
/// naming tree `id`, and the cache-tree of that directory over it.
fn expansion_of(
    repo: &gix::Repository,
    path: BString,
    id: gix::ObjectId,
) -> Option<(gix::index::VirtualSparseDir, Option<gix::index::extension::Tree>)> {
    let mut sub: gix::index::State = repo.index_from_tree(&id).ok()?.into();
    let entries = sub
        .entries()
        .iter()
        .map(|e| {
            let mut full = path.clone();
            full.extend_from_slice(e.path(&sub));
            (full, e.id, e.mode)
        })
        .collect();
    // Every tree named here is in the repository already — it is the one the sparse
    // directory names, and its subtrees — so `WRITE_TREE_REPAIR` validates every node without
    // writing an object.
    let odb = RepoOdb { repo };
    let subtree = sub
        .cache_tree_update(&odb, cache_tree::Options { missing_ok: false, repair: true })
        .ok()
        .and_then(|_| sub.remove_tree());
    Some((gix::index::VirtualSparseDir { path, id, entries }, subtree))
}

/// Move the remembered sparse directories of `index` onto `tree`, the tree a two-way or one-way
/// `unpack_trees()` just moved the index to.
///
/// `unpack_trees()` traverses a sparse-directory entry like any other entry
/// (unpack-trees.c:1164-1220): a directory whose tree moved is carried across as the new tree's
/// sparse directory, and one the new tree drops is deleted — the index stays collapsed and
/// nothing is expanded. A directory stays remembered when the index holds exactly that tree's
/// content below it, every entry merged and skip-worktree, and is forgotten when neither the
/// tree nor the index has anything below it any more. Anything else is left for the write,
/// which can then not put the directory back and expands the index as git would have.
pub fn retarget_virtual_sparse_dirs(repo: &gix::Repository, index: &mut gix::index::State, tree: gix::ObjectId) {
    if !index.is_sparse() || index.virtual_sparse_dirs().is_empty() {
        return;
    }
    let Ok(root) = repo.find_tree(tree) else { return };
    let mut dirs = Vec::with_capacity(index.virtual_sparse_dirs().len());
    for dir in index.virtual_sparse_dirs().to_vec() {
        let below_in_index = index
            .entries()
            .iter()
            .any(|e| e.path(index).starts_with(dir.path.as_slice()));
        let name = dir.path.strip_suffix(b"/").unwrap_or(&dir.path);
        let entry = root
            .lookup_entry(name.split(|b| *b == b'/'))
            .ok()
            .flatten()
            .filter(|e| e.mode().is_tree());
        let Some(entry) = entry else {
            if below_in_index {
                dirs.push(dir);
            }
            continue;
        };
        let id = entry.object_id();
        if id == dir.id {
            dirs.push(dir);
            continue;
        }
        match expansion_of(repo, dir.path.clone(), id) {
            Some((moved, _)) => dirs.push(moved),
            None => dirs.push(dir),
        }
    }
    index.set_virtual_sparse_dirs(dirs);
}

/// `ensure_full_index()` — `expand_index(istate, NULL)` (sparse-index.c:330-474): an index
/// that is not `INDEX_EXPANDED` gives the advice (once), has every sparse directory replaced
/// by its entries, is marked `INDEX_EXPANDED`, and gets its cache-tree recomputed from scratch
/// (`cache_tree_free()` + `cache_tree_update(istate, 0)`, sparse-index.c:469-470).
///
/// Returns whether the index was sparse before.
pub fn ensure_full_index(repo: &gix::Repository, index: &mut gix::index::State) -> bool {
    if !index.is_sparse() {
        return false;
    }
    if GIVE_ADVICE_ON_EXPANSION.swap(false, Ordering::Relaxed) {
        crate::advice::Advice::SparseIndexExpanded.advise_in(repo, ADVICE_MSG);
    }
    // Directories still collapsed (an index read before the read hook existed, or one a
    // conversion just collapsed) are expanded from their trees; ones already expanded on
    // read are simply forgotten.
    let _ = repo.ensure_full_index(index);
    index.forget_virtual_sparse_dirs();
    let odb = RepoOdb { repo };
    index.remove_tree();
    let _ = index.cache_tree_update(&odb, cache_tree::Options::default());
    true
}

/// `index_name_pos()` looking `path` up (read-cache.c:543-560): when the index is collapsed and
/// a sparse directory holds `path`, git expands the whole index before it searches again.
/// `index` is the index as git holds it at that moment. Returns whether it was expanded.
pub fn expand_on_lookup(repo: &gix::Repository, index: &mut gix::index::State, path: &gix::bstr::BStr) -> bool {
    if !index.is_sparse() || index.virtual_sparse_dir_containing(path).is_none() {
        return false;
    }
    ensure_full_index(repo, index)
}

/// `path_in_sparse_checkout()` (dir.c:1576-1622) against the repository's own patterns: a
/// repository that is not sparse, or whose pattern file cannot be read
/// (`init_sparse_checkout_patterns()` failing), takes every path in.
pub fn path_in_sparse_checkout(repo: &gix::Repository, path: &[u8]) -> bool {
    let settings = settings(repo);
    if !settings.apply_sparse_checkout {
        return true;
    }
    let cone = settings.cone;
    match crate::porcelain::sparse_checkout::UnpackPatterns::load(repo, cone) {
        Some(patterns) => patterns.path_in_sparse_checkout(path),
        None => true,
    }
}

/// `path_in_cone_mode_sparse_checkout()` (dir.c:1631-1635): [`path_in_sparse_checkout`], except
/// that patterns which are not cone-shaped take every path in.
pub fn path_in_cone_mode_sparse_checkout(repo: &gix::Repository, path: &[u8]) -> bool {
    let settings = settings(repo);
    if !settings.apply_sparse_checkout || !settings.cone {
        return true;
    }
    match crate::porcelain::sparse_checkout::UnpackPatterns::load(repo, true) {
        Some(patterns) => patterns.path_in_sparse_checkout(path),
        None => true,
    }
}

/// `pathspec_needs_expanded_index()` (pathspec.c:805-894): would matching `args` against the
/// collapsed `index` have to look inside a sparse directory? `args` are the command-line
/// pathspecs, relative to the current directory.
///
/// A pathspec with magic always does. A wildcard one does when its literal prefix reaches
/// into a sparse directory, or when it could match a sparse directory without being only
/// trailing `*`s after an in-cone path. A literal one does when it lies outside the cone and
/// names no skip-worktree entry of the collapsed index (`matches_skip_worktree()`).
pub fn pathspec_needs_expanded_index(repo: &gix::Repository, index: &gix::index::State, args: &[String]) -> bool {
    if !index.is_sparse() {
        return false;
    }
    let prefix: Vec<u8> = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| {
            let mut p = gix::path::into_bstr(p).into_owned();
            if !p.is_empty() {
                p.push(b'/');
            }
            p.into()
        })
        .unwrap_or_default();
    let dirs = index.virtual_sparse_dirs();
    for arg in args {
        if arg.starts_with(':') {
            return true;
        }
        let mut item = prefix.clone();
        item.extend_from_slice(arg.strip_prefix("./").unwrap_or(arg).as_bytes());
        let nowildcard_len = item
            .iter()
            .position(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'))
            .unwrap_or(item.len());
        if nowildcard_len < item.len() {
            if item[nowildcard_len..].iter().all(|b| *b == b'*')
                && path_in_cone_mode_sparse_checkout(repo, &item)
            {
                continue;
            }
            let hit = dirs.iter().any(|d| {
                let name = d.path.as_slice();
                (nowildcard_len > name.len() && item.starts_with(name))
                    || (name.starts_with(&item[..nowildcard_len])
                        && gix::glob::wildmatch(item.as_bstr(), name.as_bstr(), gix::glob::wildmatch::Mode::empty()))
            });
            if hit {
                return true;
            }
        } else if !path_in_cone_mode_sparse_checkout(repo, &item) && !matches_skip_worktree(index, &item) {
            return true;
        }
    }
    false
}

/// `matches_skip_worktree()` (pathspec.c:778-803) for a literal pathspec: does it name a
/// skip-worktree entry of the collapsed index — a sparse directory, or a skip-worktree file
/// outside one — exactly or as a leading directory?
fn matches_skip_worktree(index: &gix::index::State, item: &[u8]) -> bool {
    let names = |name: &[u8]| {
        let name = name.strip_suffix(b"/").unwrap_or(name);
        name == item || (name.len() > item.len() && name.starts_with(item) && name[item.len()] == b'/')
    };
    index.virtual_sparse_dirs().iter().any(|d| names(&d.path))
        || index.entries().iter().any(|e| {
            let path = e.path(index);
            e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE)
                && index.virtual_sparse_dir_containing(path).is_none()
                && names(path)
        })
}

/// [`ensure_full_index`] without the advice — for the callers that print git's advice
/// themselves under their own conditions.
pub fn ensure_full_index_with_advice(repo: &gix::Repository, index: &mut gix::index::State, advise: bool) -> bool {
    if !advise {
        let give = GIVE_ADVICE_ON_EXPANSION.swap(false, Ordering::Relaxed);
        let was = ensure_full_index(repo, index);
        GIVE_ADVICE_ON_EXPANSION.store(give, Ordering::Relaxed);
        return was;
    }
    ensure_full_index(repo, index)
}

/// What [`before_write`] did to the index, so [`after_write`] can undo it.
pub enum Prepared {
    /// Nothing: the index is written as it is held.
    Untouched,
    /// A virtually expanded index was collapsed back to what was read.
    Recollapsed,
    /// A full index was converted to a sparse one (`was_full`).
    Converted,
}

/// `do_write_locked_index()`'s `convert_to_sparse()` (read-cache.c:3141-3148), for the index
/// this port holds: a collapsed one is put back as it was read, and if that is no longer
/// possible because the command changed something inside a sparse directory — which git
/// cannot do without expanding the index first — it is expanded for real and converted from
/// scratch like any full index.
pub fn before_write(repo: &gix::Repository, index: &mut gix::index::State) -> Prepared {
    if index.is_sparse() {
        if index.collapse_virtual_sparse_dirs() {
            return Prepared::Recollapsed;
        }
        ensure_full_index(repo, index);
    }
    if convert_to_sparse(repo, index) {
        Prepared::Converted
    } else {
        Prepared::Untouched
    }
}

/// The in-memory index after the write, as git leaves it: a converted one is expanded again
/// (`if (was_full) ensure_full_index(istate)`, read-cache.c:3156-3157), and a collapsed one —
/// which git never expanded — goes back to the virtually expanded form this port works with.
pub fn after_write(repo: &gix::Repository, index: &mut gix::index::State, prepared: Prepared) {
    match prepared {
        Prepared::Untouched => {}
        Prepared::Recollapsed => {
            index.reexpand_virtual_sparse_dirs(&mut |dir| {
                expansion_of(repo, dir.path.clone(), dir.id).and_then(|(_, subtree)| subtree)
            });
        }
        Prepared::Converted => {
            ensure_full_index(repo, index);
        }
    }
}

/// `convert_to_sparse()` (sparse-index.c:201-259) for an index that is not already
/// `INDEX_COLLAPSED`. Returns whether the index was collapsed.
pub fn convert_to_sparse(repo: &gix::Repository, index: &mut gix::index::State) -> bool {
    if index.is_sparse() || index.entries().is_empty() || !is_sparse_index_allowed(repo, index) {
        return false;
    }
    // "If we are purposefully collapsing a full index, then don't give advice when it is
    // expanded later."
    GIVE_ADVICE_ON_EXPANSION.store(false, Ordering::Relaxed);

    // `index_has_unmerged_entries()`: "If we have unmerged entries, then stay full."
    if index.entries().iter().any(|e| e.stage() != gix::index::entry::Stage::Unconflicted) {
        return false;
    }

    let odb = RepoOdb { repo };
    if !index.cache_tree_fully_valid(&odb) {
        // `cache_tree_free()` then `cache_tree_update(istate, WRITE_TREE_MISSING_OK)`; a
        // failure is "silently return" — this may need trees the repository lacks, and an
        // entry may still be in conflict.
        index.remove_tree();
        if index
            .cache_tree_update(&odb, cache_tree::Options { missing_ok: true, repair: false })
            .is_err()
        {
            return false;
        }
    }
    // `is_sparse_index_allowed()` above loaded the patterns; a file that vanished since
    // reads as no restriction, which collapses nothing.
    let Some(patterns) = crate::porcelain::sparse_checkout::UnpackPatterns::load(repo, true) else {
        return false;
    };

    // `remove_fsmonitor()`: the `FSMN` extension is not written by this port at all.
    let mut dirs = Vec::new();
    if let Some(tree) = index.tree() {
        let paths: Vec<&[u8]> = index.entries().iter().map(|e| e.path(index).as_bytes()).collect();
        let entries: Vec<&gix::index::Entry> = index.entries().iter().collect();
        convert_to_sparse_rec(&entries, &paths, 0, paths.len(), b"", tree, &patterns, &mut dirs);
    }
    index.collapse_into_sparse_dirs(&dirs);

    // "Clear and recompute the cache-tree".
    index.remove_tree();
    let _ = index.cache_tree_update(&odb, cache_tree::Options::default());
    true
}

/// `convert_to_sparse_rec()` (sparse-index.c:60-130): the directories in `[start, end)` that
/// collapse, as `(name with trailing '/', tree id)`, appended to `out`.
#[allow(clippy::too_many_arguments)]
fn convert_to_sparse_rec(
    entries: &[&gix::index::Entry],
    paths: &[&[u8]],
    start: usize,
    end: usize,
    ct_path: &[u8],
    ct: &gix::index::extension::Tree,
    patterns: &crate::porcelain::sparse_checkout::UnpackPatterns,
    out: &mut Vec<(BString, gix::ObjectId)>,
) {
    // "Is the current path outside of the sparse cone? Then check if the region can be
    // replaced by a sparse directory entry (everything is sparse and merged)."
    let can_convert = !patterns.path_in_sparse_checkout(ct_path)
        && entries[start..end].iter().all(|ce| {
            ce.stage() == gix::index::entry::Stage::Unconflicted
                && ce.mode != gix::index::entry::Mode::COMMIT
                && ce.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE)
        });
    if can_convert {
        out.push((ct_path.into(), ct.id));
        return;
    }

    let mut i = start;
    while i < end {
        let base = &paths[i][ct_path.len()..];
        // "Detect if this is a normal entry outside of any subtree entry."
        let Some(slash) = base.find_byte(b'/') else {
            i += 1;
            continue;
        };
        let Some(child) = ct.children.iter().find(|c| c.name.as_slice() == &base[..slash]) else {
            i += 1;
            continue;
        };
        // "cache-tree entry is invalidated, cannot collapse."
        let Some(span) = child.num_entries else {
            i += 1;
            continue;
        };
        let span = span as usize;
        let child_path = &paths[i][..ct_path.len() + slash + 1];
        convert_to_sparse_rec(entries, paths, i, (i + span).min(end), child_path, child, patterns, out);
        i += span.max(1);
    }
}

/// `is_sparse_index_allowed()` (sparse-index.c:153-199) for a caller that writes the index —
/// git's `flags == 0`, never `SPARSE_INDEX_MEMORY_ONLY`.
pub fn is_sparse_index_allowed(repo: &gix::Repository, index: &gix::index::State) -> bool {
    let settings = settings(repo);
    // `cfg->apply_sparse_checkout`, `cfg->core_sparse_checkout_cone`: plain booleans that
    // default to off.
    if !settings.apply_sparse_checkout || !settings.cone {
        return false;
    }
    // "The sparse index is not (yet) integrated with a split index."
    if index.split_index().is_some() || index.had_link() {
        return false;
    }
    // `r->settings.sparse_index`: `index.sparse`, off by default (repo-settings.c:63).
    if !settings.sparse_index {
        return false;
    }
    // `init_sparse_checkout_patterns()` fails when the pattern file cannot be read
    // (dir.c:1557-1574). Whether its patterns are cone-shaped
    // (`pl->use_cone_patterns`, cleared by dir.c:927-930 on a hand-written non-cone line)
    // is not re-derived here; `core.sparseCheckoutCone` stands for it.
    crate::porcelain::sparse_checkout::UnpackPatterns::load(repo, true).is_some()
}
