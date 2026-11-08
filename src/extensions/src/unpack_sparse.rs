//! The sparse-checkout half of `unpack_trees()` (unpack-trees.c:1928-2075), shared by every
//! writer that moves the worktree from one tree to another: `checkout`, `switch`, `reset`,
//! `read-tree -u`, and the merge-shaped verbs that reach the worktree through
//! `merge_apply`.
//!
//! git runs it inside `unpack_trees()`, over the whole result index:
//!
//! ```c
//! if (!o->skip_sparse_checkout)
//!         mark_new_skip_worktree(o->internal.pl, o->src_index, 0,
//!                                CE_NEW_SKIP_WORKTREE, o->verbose_update);
//! [... traverse_trees(): the merge function builds o->internal.result ...]
//! if (!o->skip_sparse_checkout) {
//!         mark_new_skip_worktree(o->internal.pl, &o->internal.result,
//!                                CE_ADDED, CE_SKIP_WORKTREE | CE_NEW_SKIP_WORKTREE,
//!                                o->verbose_update);
//!         for (i = 0; i < o->internal.result.cache_nr; i++) {
//!                 struct cache_entry *ce = o->internal.result.cache[i];
//!                 if (ce->ce_flags & CE_ADDED &&
//!                     verify_absent(ce, WARNING_SPARSE_ORPHANED_NOT_OVERWRITTEN, o))
//!                         ret = 1;
//!                 if (apply_sparse_checkout(&o->internal.result, ce, o))
//!                         ret = 1;
//!         }
//!         if (ret == 1) {
//!                 if (o->internal.show_all_errors)
//!                         display_warning_msgs(o);
//!                 ret = 0;
//!         }
//! }
//! ret = check_updates(o, &o->internal.result) ? (-2) : 0;
//! ```
//!
//! (unpack-trees.c:1974-1976, :2035-2075.) The writers here do not share git's per-entry
//! `ce_flags`, so each hands over what its merge function decided — the result index, the
//! paths it gave `CE_UPDATE` and the paths it gave `CE_WT_REMOVE` — and [`SparseCheckout::apply`]
//! recovers the rest from the source index: an entry's `CE_SKIP_WORKTREE` going in is the one
//! `keep_entry()` keeps and `merged_entry()` migrates from the source entry at its path
//! (:2616-2617), `CE_NEW_SKIP_WORKTREE` is loop #1's verdict on that source entry, and an
//! entry no source entry stands behind is `CE_ADDED`, whose two bits loop #2 sets together.
//! What comes back is the result's new `CE_SKIP_WORKTREE` bits and the two sets as
//! `apply_sparse_checkout()` left them, for the writer's own `check_updates()`.

use std::collections::{BTreeSet, HashMap, HashSet};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Flags, Mode};

use crate::porcelain::sparse_checkout::UnpackPatterns;

/// The `struct unpack_trees_options` fields the sparse loops read.
#[derive(Clone, Copy, Default)]
pub(crate) struct Opts {
    /// `o->internal.show_all_errors`, which only `setup_unpack_trees_porcelain()` sets
    /// (unpack-trees.c:204): `checkout`'s two-way switch, `merge` and `reset_head()`. Without
    /// it every warning is an `error()` with the plumbing wording, printed as it is met.
    pub(crate) porcelain: bool,
    /// `o->quiet`: `add_rejected_path()` reports nothing (unpack-trees.c:246-247).
    pub(crate) quiet: bool,
    /// `o->reset == UNPACK_RESET_OVERWRITE_UNTRACKED`: `verify_absent_1()` lets anything in
    /// the way be overwritten (unpack-trees.c:2488-2496).
    pub(crate) overwrite_untracked: bool,
    /// `o->reset` at all, `UNPACK_RESET_PROTECT_UNTRACKED` included: `verify_uptodate_1()` lets an
    /// entry without `CE_VALID` or `CE_SKIP_WORKTREE` through unchecked (unpack-trees.c:2233-2239).
    /// [`Self::overwrite_untracked`] implies it.
    pub(crate) reset: bool,
    /// `o->preserve_ignored`: without it `o->internal.dir` carries the standard excludes, and
    /// an ignored file in the way may be overwritten (unpack-trees.c:1922-1926, :2437-2443).
    pub(crate) preserve_ignored: bool,
    /// `o->verbose_update`: the `Updating index flags` meter of each loop.
    pub(crate) show_progress: bool,
}

/// `enum unpack_trees_error_types`' three warnings (unpack-trees.h).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Warning {
    /// `WARNING_SPARSE_NOT_UPTODATE_FILE`
    NotUptodate,
    /// `WARNING_SPARSE_ORPHANED_NOT_OVERWRITTEN`
    Orphaned,
}

/// The `enum unpack_trees_error_types` errors `verify_clean_subdirectory()` and `verify_absent_1()`
/// report from the sparse loop (unpack-trees.h).
#[derive(Clone, Copy)]
enum Error {
    /// `ERROR_NOT_UPTODATE_FILE`
    NotUptodateFile,
    /// `ERROR_NOT_UPTODATE_DIR`
    NotUptodateDir,
    /// `ERROR_CWD_IN_THE_WAY`
    CwdInTheWay,
}

impl Error {
    /// `unpack_plumbing_errors[]` (unpack-trees.c:42-53).
    fn plumbing(self, path: &BStr) -> String {
        match self {
            Error::NotUptodateFile => format!("Entry '{path}' not uptodate. Cannot merge."),
            Error::NotUptodateDir => format!("Updating '{path}' would lose untracked files in it"),
            Error::CwdInTheWay => format!("Refusing to remove '{path}' since it is the current working directory."),
        }
    }
}

impl Warning {
    /// `unpack_plumbing_errors[]` (unpack-trees.c:69-76).
    fn plumbing(self, path: &BStr) -> String {
        match self {
            Warning::NotUptodate => format!("Path '{path}' not uptodate; will not remove from working tree."),
            Warning::Orphaned => format!("Path '{path}' already present; will not overwrite with sparse update."),
        }
    }

    /// `setup_unpack_trees_porcelain()`'s wording (unpack-trees.c:197-202).
    fn porcelain(self) -> &'static str {
        match self {
            Warning::NotUptodate => "The following paths are not up to date and were left despite sparse patterns:",
            Warning::Orphaned => {
                "The following paths were already present and thus not updated despite sparse patterns:"
            }
        }
    }
}

/// `istate->sparse_checkout_patterns` of the last `o->internal.result` a
/// [`SparseCheckout`] stood for, while still unloaded: the warnings its own
/// `init_sparse_checkout_patterns()` (dir.c:1557-1574) prints when the first attribute
/// lookup of `check_updates()` reaches it. `check_updates()` sets `GIT_ATTR_CHECKOUT`
/// (unpack-trees.c:456), so `read_attr()` asks `read_attr_from_index()` first (attr.c:852-855),
/// and that asks `path_in_cone_mode_sparse_checkout()` of the index (attr.c:821).
/// [`take_result_index_pattern_warnings`] consumes it, as the load is cached on the
/// `index_state` and happens once.
static RESULT_INDEX_PATTERNS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The warnings the result index's pattern load prints, the first time `checkout_entry_ca()`
/// writes a regular file and so runs `convert_attrs()` on it (entry.c:584-587); `None` once
/// loaded, or when no sparse `unpack_trees()` stands behind the checkout.
pub(crate) fn take_result_index_pattern_warnings() -> Option<String> {
    RESULT_INDEX_PATTERNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// `unpack_trees()` with `o->skip_sparse_checkout` clear: the pattern list it read and the
/// options its warnings depend on.
pub(crate) struct SparseCheckout {
    patterns: UnpackPatterns,
    opts: Opts,
}

impl SparseCheckout {
    /// ```c
    /// if (!cfg->apply_sparse_checkout || !o->update)
    ///         o->skip_sparse_checkout = 1;
    /// if (!o->skip_sparse_checkout) {
    ///         [...]
    ///         populate_from_existing_patterns(o, &pl);
    /// }
    /// ```
    ///
    /// (unpack-trees.c:1931-1936.) `None` is `o->skip_sparse_checkout` set — no sparse checkout,
    /// a run that does not update the worktree, or no pattern file to read (:1829-1836).
    pub(crate) fn begin(repo: &gix::Repository, update: bool, opts: Opts) -> Option<Self> {
        if !update {
            return None;
        }
        Self::with_patterns(crate::merge_guard::sparse_patterns(repo), opts)
    }

    /// [`Self::begin`] for a caller that has already read the patterns, `None` standing for
    /// `o->skip_sparse_checkout`.
    pub(crate) fn with_patterns(patterns: Option<UnpackPatterns>, opts: Opts) -> Option<Self> {
        let patterns = patterns?;
        // `populate_from_existing_patterns()` loads the file into this run's own `pl`
        // (unpack-trees.c:1832-1840), so `add_pattern_to_hashsets()` warns here, once per
        // `unpack_trees()`. `o->internal.result` is a fresh `index_state` whose
        // `sparse_checkout_patterns` nothing has loaded yet (dir.c:1557-1574).
        eprint!("{}", patterns.warnings);
        *RESULT_INDEX_PATTERNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(patterns.warnings.clone());
        Some(SparseCheckout { patterns, opts })
    }

    /// The two sparse loops and `apply_sparse_checkout()` over `result`, the index the merge
    /// function built from `src`.
    ///
    /// `update` holds the paths of `result` entries carrying `CE_UPDATE`, `remove` the paths of
    /// the source entries the merge dropped (`deleted_entry()`'s `CE_REMOVE`, which
    /// `do_add_entry()` makes `CE_WT_REMOVE`, :222-223). On return each stage-0 entry of
    /// `result` carries the `CE_SKIP_WORKTREE` the loops gave it, `update` names what
    /// `check_updates()` writes and `remove` what it unlinks — which now includes an entry the
    /// patterns took out of the worktree and that stays in the index.
    pub(crate) fn apply(
        &self,
        repo: &gix::Repository,
        src: &gix::index::File,
        result: &mut gix::index::State,
        update: &mut BTreeSet<BString>,
        remove: &mut BTreeSet<BString>,
    ) -> anyhow::Result<()> {
        let die = |e: crate::progress::DelayError| crate::fatal::die(e.to_string());

        // Sparse loop #1: `CE_NEW_SKIP_WORKTREE` on every entry of the source index.
        let src_new = self.patterns.new_skip_worktree(src, &|_| true, self.opts.show_progress).map_err(die)?;
        let mut src_at: HashMap<BString, usize> = HashMap::new();
        let mut src_conflicted: HashSet<BString> = HashSet::new();
        {
            let backing = src.path_backing();
            for (i, e) in src.entries().iter().enumerate() {
                let path = e.path_in(backing).to_owned();
                if e.stage_raw() == 0 {
                    src_at.insert(path, i);
                } else {
                    src_conflicted.insert(path);
                }
            }
        }

        // `o->internal.result` holds the `CE_REMOVE` entries too, each where its path sorts, and
        // loop #2 walks them with the rest.
        let result_at: HashMap<(BString, u32), usize> = {
            let backing = result.path_backing();
            result
                .entries()
                .iter()
                .enumerate()
                .map(|(i, e)| ((e.path_in(backing).to_owned(), e.stage_raw()), i))
                .collect()
        };
        let in_result: HashSet<&BString> = result_at.keys().map(|(p, _)| p).collect();
        let removed: Vec<BString> = remove
            .iter()
            .filter(|p| !in_result.contains(p) && src_at.contains_key(*p))
            .cloned()
            .collect();
        drop(in_result);
        let mut full = result.clone();
        for p in &removed {
            let e = &src.entries()[src_at[p]];
            full.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, p.as_bstr());
        }
        full.sort_entries();
        enum At {
            Result(usize),
            Removed,
        }
        let full_paths: Vec<BString> = {
            let backing = full.path_backing();
            full.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        let at: Vec<At> = full
            .entries()
            .iter()
            .zip(&full_paths)
            .map(|(e, p)| match result_at.get(&(p.clone(), e.stage_raw())) {
                Some(&i) => At::Result(i),
                None => At::Removed,
            })
            .collect();

        // `merged_entry()` sets `CE_ADDED` where no source entry stands at the path (:2569-2581).
        let added: Vec<bool> = full
            .entries()
            .iter()
            .zip(&full_paths)
            .zip(&at)
            .map(|((e, p), at)| {
                matches!(at, At::Result(_))
                    && e.stage_raw() == 0
                    && !src_at.contains_key(p)
                    && !src_conflicted.contains(p)
            })
            .collect();

        // Sparse loop #2: `CE_SKIP_WORKTREE | CE_NEW_SKIP_WORKTREE` on the `CE_ADDED` entries,
        // then cleared again on every entry the walk takes in — which in cone mode reaches past
        // the `CE_ADDED` ones (`clear_ce_flags()`).
        let cleared = self
            .patterns
            .clear_ce_flags(&full, &|i| added[i], self.opts.show_progress)
            .map_err(die)?;

        let stat = crate::porcelain::read_tree::StatCtx::new(repo, src)?;
        let mut wt = Worktree {
            repo,
            src,
            src_new: &src_new,
            stat: &stat,
            excludes: None,
            removed: removed.iter().cloned().collect(),
        };
        let mut warnings = Warnings::new(self.opts);
        let mut skip: Vec<Option<bool>> = vec![None; result.entries().len()];

        for (f, path) in full_paths.iter().enumerate() {
            let e = &full.entries()[f];
            // The bits each entry carries into `apply_sparse_checkout()`: `CE_ADDED` ones from
            // loop #2; the rest from the source entry, which `keep_entry()`, `merged_entry()`
            // (:2616-2617) and `deleted_entry()` carry over with its loop #1 verdict. An unmerged
            // entry, or `merged_entry()` over a `CE_CONFLICTED` one, has neither.
            let (was, new) = match at[f] {
                _ if e.stage_raw() != 0 || src_conflicted.contains(path) => (false, false),
                At::Result(_) if added[f] => (true, true),
                At::Result(_) | At::Removed => {
                    let s = src_at[path];
                    (src.entries()[s].flags.contains(Flags::SKIP_WORKTREE), src_new[s])
                }
            };
            let (was, new) = (was && !cleared[f], new && !cleared[f]);
            // `verify_absent()` returns at once for a `CE_NEW_SKIP_WORKTREE` entry
            // (unpack-trees.c:2541-2542). What it refuses only makes the loop's `ret` 1: the
            // entry keeps its `CE_UPDATE` and is written all the same.
            if added[f] && !new {
                let _ = self.verify_absent(&mut wt, &mut warnings, e.mode, path, remove);
            }
            let left = self.apply_sparse_checkout(&mut wt, &mut warnings, e, path, (was, new), update, remove);
            if let At::Result(i) = at[f] {
                skip[i] = Some(left);
            }
        }

        for (e, skip) in result.entries_mut().iter_mut().zip(skip) {
            match skip {
                Some(true) => e.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED),
                Some(false) => {
                    e.flags.remove(Flags::SKIP_WORKTREE);
                    if !e.flags.contains(Flags::INTENT_TO_ADD) {
                        e.flags.remove(Flags::EXTENDED);
                    }
                }
                None => {}
            }
        }
        warnings.display();
        Ok(())
    }

    /// `apply_sparse_checkout()` (unpack-trees.c:523-585) for the entry `e` at `path`, whose
    /// `CE_SKIP_WORKTREE` and `CE_NEW_SKIP_WORKTREE` going in are `was` and `new`. Returns the
    /// `CE_SKIP_WORKTREE` it leaves.
    #[allow(clippy::too_many_arguments)]
    fn apply_sparse_checkout(
        &self,
        wt: &mut Worktree<'_, '_>,
        warnings: &mut Warnings,
        e: &gix::index::Entry,
        path: &BString,
        (was, new): (bool, bool),
        update: &mut BTreeSet<BString>,
        remove: &mut BTreeSet<BString>,
    ) -> bool {
        let is_removed = wt.removed.contains(path);
        // "Make sure they don't modify worktree if they are already outside checkout area":
        // no `CE_UPDATE`, and a `CE_REMOVE` keeps its index removal but not its unlink.
        if was && new {
            if is_removed {
                remove.remove(path);
            } else {
                update.remove(path);
            }
        }
        if !was && new {
            // "If CE_UPDATE is set, verify_uptodate() must be called already".
            if !update.contains(path) && self.uptodate_rejects(wt.repo, wt.stat, e, path) {
                warnings.reject(Warning::NotUptodate, path);
                return false;
            }
            update.remove(path);
            remove.insert(path.clone());
        }
        if was && !new {
            if self.verify_absent(wt, warnings, e.mode, path, remove) {
                return false;
            }
            if !is_removed {
                update.insert(path.clone());
            }
        }
        new
    }

    /// `verify_uptodate_sparse()` (unpack-trees.c:2282-2286): `verify_uptodate_1()` with the
    /// entry's `CE_SKIP_WORKTREE` set, so its file is `lstat()`ed whatever `o->reset` says
    /// (:2233-2239). A missing file is `ENOENT`, never a rejection; a gitlink never is
    /// (:2253-2262).
    fn uptodate_rejects(
        &self,
        repo: &gix::Repository,
        stat: &crate::porcelain::read_tree::StatCtx,
        e: &gix::index::Entry,
        path: &BString,
    ) -> bool {
        stat.probe(repo, e, path.as_bstr()) == crate::porcelain::read_tree::Probe::Changed
    }

    /// `verify_absent_1(ce, WARNING_SPARSE_ORPHANED_NOT_OVERWRITTEN, COMPLETELY_ABSENT, o)`
    /// (unpack-trees.c:2479-2535) and the `check_ok_to_remove()` it ends in (:2419-2473), for the
    /// entry of `mode` at `path` that is to be written: whether something already in the
    /// worktree stands in its way. Each refusal is reported as `add_rejected_path()` reports it.
    fn verify_absent(
        &self,
        wt: &mut Worktree<'_, '_>,
        warnings: &mut Warnings,
        mode: Mode,
        path: &BString,
        remove: &mut BTreeSet<BString>,
    ) -> bool {
        let repo = wt.repo;
        if self.opts.overwrite_untracked {
            // "Avoid nuking startup_info->original_cwd..."
            if is_original_cwd(repo, path.as_bstr()) {
                warnings.error(Error::CwdInTheWay, path);
                return true;
            }
            // "...but nuke anything else."
            return false;
        }
        let Some(workdir) = repo.workdir() else {
            return false;
        };
        // `check_leading_path()`: the first leading component that is not a directory is
        // what stands in the way; a missing one or a symlink leaves nothing to check.
        let mut name: &[u8] = path.as_slice();
        let mut leading = false;
        let mut at = 0;
        while let Some(slash) = path[at..].find_byte(b'/') {
            let lead = &path[..at + slash];
            match std::fs::symlink_metadata(workdir.join(gix::path::from_bstr(lead.as_bstr()).as_ref())) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(meta) if !meta.file_type().is_symlink() => {
                    name = lead;
                    leading = true;
                    break;
                }
                _ => return false,
            }
            at += slash + 1;
        }
        let full = workdir.join(gix::path::from_bstr(name.as_bstr()).as_ref());
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            return false;
        };
        let name = name.as_bstr();
        if repo.config_snapshot().boolean("core.ignorecase").unwrap_or(false)
            && crate::porcelain::reset::icase_exists(repo, wt.src, name, &full, &meta)
        {
            return false;
        }
        // `is_excluded(o->internal.dir, o->src_index, name, &dtype)`: the leading component is
        // asked as `DT_UNKNOWN`, which `lstat()` resolves; the entry itself as
        // `ce_to_dtype(ce)`, whatever the worktree holds there.
        let dtype = match leading {
            true if meta.is_dir() => Mode::DIR,
            true => Mode::FILE,
            false if mode == Mode::COMMIT => Mode::DIR,
            false => Mode::FILE,
        };
        if self.excluded(wt, name, dtype) {
            return false;
        }
        // "We are checking out path "foo" and found "foo/." in the working tree."
        if meta.is_dir() {
            return !self.verify_clean_subdirectory(wt, warnings, mode, name, remove);
        }
        // "The previous round may already have decided to delete this path".
        if wt.removed.contains(name) {
            return false;
        }
        warnings.reject(Warning::Orphaned, &name.to_owned());
        true
    }

    /// Whether `o->internal.dir`, set up with the standard excludes unless `o->preserve_ignored`
    /// (unpack-trees.c:1922-1926), excludes `name` as a `dtype`.
    fn excluded(&self, wt: &mut Worktree<'_, '_>, name: &BStr, dtype: Mode) -> bool {
        if self.opts.preserve_ignored {
            return false;
        }
        let (repo, src) = (wt.repo, wt.src);
        let stack = wt.excludes.get_or_insert_with(|| {
            repo.excludes(src, None, gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped)
                .ok()
        });
        stack
            .as_mut()
            .is_some_and(|s| s.at_entry(name, Some(dtype)).map(|p| p.is_excluded()).unwrap_or(false))
    }

    /// `verify_clean_subdirectory()` (unpack-trees.c:2323-2396) for the entry of `mode` at
    /// `name`, where the worktree holds a directory: "we would not want to lose anything in the
    /// existing directory there". Returns whether the directory may go.
    fn verify_clean_subdirectory(
        &self,
        wt: &mut Worktree<'_, '_>,
        warnings: &mut Warnings,
        mode: Mode,
        name: &BStr,
        remove: &mut BTreeSet<BString>,
    ) -> bool {
        // A gitlink whose submodule is not moving is left alone, and one that is goes to
        // `verify_clean_submodule()`, which returns 0 unless `submodule_from_ce()` finds the
        // submodule — only when the command recurses into submodules (`should_update_submodules()`),
        // which no sparse-loop caller here does.
        if mode == Mode::COMMIT {
            return true;
        }
        let mut prefix = name.to_owned();
        prefix.push(b'/');
        // "First let's make sure we do not have a local modification in that directory": each
        // stage-0 entry of `o->src_index` under it must pass `verify_uptodate()`, and goes into
        // the result as `CE_REMOVE`, which `do_add_entry()` makes `CE_WT_REMOVE` (:222-223).
        if let Some(range) = wt.src.prefixed_entries_range(prefix.as_bstr()) {
            for i in range {
                let e = &wt.src.entries()[i];
                if e.stage_raw() != 0 {
                    continue;
                }
                let path = e.path(wt.src).to_owned();
                if self.verify_uptodate(wt, e, wt.src_new[i], &path) {
                    warnings.error(Error::NotUptodateFile, &path);
                    return false;
                }
                wt.removed.insert(path.clone());
                remove.insert(path);
            }
        }
        // "Do not lose a locally present file that is not ignored."
        if self.untracked_under(wt, &prefix) {
            warnings.error(Error::NotUptodateDir, &name.to_owned());
            return false;
        }
        // "Do not lose startup_info->original_cwd"
        if is_original_cwd(wt.repo, name) {
            warnings.error(Error::CwdInTheWay, &name.to_owned());
            return false;
        }
        true
    }

    /// `verify_uptodate()` (unpack-trees.c:2224-2280) for the source entry `e`, whose loop #1
    /// verdict is `new_skip`: whether its file has changed from it.
    fn verify_uptodate(&self, wt: &Worktree<'_, '_>, e: &gix::index::Entry, new_skip: bool, path: &BString) -> bool {
        let cheats = e.flags.intersects(Flags::ASSUME_VALID | Flags::SKIP_WORKTREE);
        if e.flags.contains(Flags::SKIP_WORKTREE) && new_skip {
            return false;
        }
        // "CE_VALID and CE_SKIP_WORKTREE cheat, we better check again".
        if !cheats && (self.opts.reset || self.opts.overwrite_untracked || e.flags.contains(Flags::UPTODATE)) {
            return false;
        }
        e.mode != Mode::COMMIT && self.uptodate_rejects(wt.repo, wt.stat, e, path)
    }

    /// `read_directory(&d, o->src_index, "<name>/", ...)` with no `dir->flags` (unpack-trees.c:
    /// 2381-2386): whether anything under `prefix` is untracked — a regular file or symlink no
    /// source entry names, or a nested repository — and not excluded. An excluded directory is
    /// not entered, a directory a gitlink names is skipped, and `.git` is never looked at
    /// (`treat_path()`, `treat_directory()`, dir.c).
    fn untracked_under(&self, wt: &mut Worktree<'_, '_>, prefix: &BString) -> bool {
        let (repo, src) = (wt.repo, wt.src);
        untracked_under(repo, src, prefix, &mut |name, dtype| self.excluded(wt, name, dtype))
    }
}

/// [`SparseCheckout::untracked_under`] against `src`, with `excluded` standing for
/// `o->internal.dir`: `verify_clean_subdirectory()`'s `read_directory()` for any
/// `unpack_trees()` caller, sparse or not.
pub(crate) fn untracked_under(
    repo: &gix::Repository,
    src: &gix::index::File,
    prefix: &BString,
    excluded: &mut dyn FnMut(&BStr, Mode) -> bool,
) -> bool {
    let Some(workdir) = repo.workdir().map(std::path::Path::to_path_buf) else {
        return false;
    };
    let icase = repo.config_snapshot().boolean("core.ignorecase").unwrap_or(false);
    let mut pending: Vec<BString> = vec![prefix.clone()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = std::fs::read_dir(workdir.join(gix::path::from_bstr(dir.as_bstr()).as_ref())) else {
            continue;
        };
        for entry in read.flatten() {
            let file_name = gix::path::os_str_into_bstr(&entry.file_name()).map(|n| n.to_owned());
            let Ok(file_name) = file_name else { continue };
            if file_name == ".git" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            let mut rel = dir.clone();
            rel.extend_from_slice(&file_name);
            if !file_type.is_dir() {
                // `index_file_exists(istate, path, len, ignore_case)`.
                let tracked = src.entry_by_path(rel.as_bstr()).is_some()
                    || (icase
                        && src
                            .entries()
                            .iter()
                            .any(|e| e.path(src).eq_ignore_ascii_case(rel.as_slice())));
                if tracked || !(file_type.is_file() || file_type.is_symlink()) {
                    continue;
                }
                if !excluded(rel.as_bstr(), Mode::FILE) {
                    return true;
                }
                continue;
            }
            if excluded(rel.as_bstr(), Mode::DIR) {
                continue;
            }
            // `directory_exists_in_index()`: a gitlink is skipped, a directory the index
            // has entries under is entered.
            if src
                .entry_by_path(rel.as_bstr())
                .is_some_and(|e| e.mode == Mode::COMMIT)
            {
                continue;
            }
            rel.push(b'/');
            let in_index = src.prefixed_entries_range(rel.as_bstr()).is_some();
            if !in_index && is_nonbare_repository_dir(&entry.path()) {
                return true;
            }
            pending.push(rel);
        }
    }
    false
}

/// What `verify_absent_1()` and `verify_clean_subdirectory()` read: `o->src_index` with its loop
/// #1 verdicts, its stat data, `o->internal.dir`'s excludes once they are needed, and the
/// paths of the result's `CE_REMOVE` entries.
struct Worktree<'r, 'a> {
    repo: &'r gix::Repository,
    src: &'a gix::index::File,
    src_new: &'a [bool],
    stat: &'a crate::porcelain::read_tree::StatCtx,
    excludes: Option<Option<gix::AttributeStack<'r>>>,
    removed: BTreeSet<BString>,
}

/// `startup_info->original_cwd` is `name`: the directory the command was started in, which
/// `setup_original_cwd()` records relative to the top of the worktree (setup.c).
fn is_original_cwd(repo: &gix::Repository, name: &BStr) -> bool {
    let (Some(cwd), Some(workdir)) = (crate::worktree::original_cwd(), repo.workdir()) else {
        return false;
    };
    let full = workdir.join(gix::path::from_bstr(name).as_ref());
    gix::path::realpath(&full).is_ok_and(|real| real == cwd)
}

/// `is_nonbare_repository_dir()` (setup.c): `<dir>/.git` is a gitfile or a git directory.
fn is_nonbare_repository_dir(dir: &std::path::Path) -> bool {
    dir.join(".git").exists() && gix::open(dir).is_ok()
}

/// `o->internal.unpack_rejects[]` for the two warnings, and how they reach stderr.
struct Warnings {
    opts: Opts,
    not_uptodate: Vec<BString>,
    orphaned: Vec<BString>,
}

impl Warnings {
    fn new(opts: Opts) -> Self {
        Warnings {
            opts,
            not_uptodate: Vec::new(),
            orphaned: Vec::new(),
        }
    }

    /// `add_rejected_path()` for an error type `verify_absent()` reaches in the sparse loop. With
    /// `o->internal.show_all_errors` it only goes onto a list that `display_warning_msgs()`, the
    /// one display the loop has, does not print (unpack-trees.c:290-315, :2068-2075); without it
    /// it is an `error()` with the plumbing wording, printed as it is met.
    fn error(&mut self, kind: Error, path: &BString) {
        if self.opts.quiet || self.opts.porcelain {
            return;
        }
        eprintln!("error: {}", kind.plumbing(path.as_bstr()));
    }

    /// `add_rejected_path()` (unpack-trees.c:242-259).
    fn reject(&mut self, kind: Warning, path: &BString) {
        if self.opts.quiet {
            return;
        }
        if !self.opts.porcelain {
            eprintln!("error: {}", kind.plumbing(path.as_bstr()));
            return;
        }
        match kind {
            Warning::NotUptodate => self.not_uptodate.push(path.clone()),
            Warning::Orphaned => self.orphaned.push(path.clone()),
        }
    }

    /// `display_warning_msgs()` (unpack-trees.c:290-315), in enum order.
    fn display(self) {
        let mut shown = false;
        for (kind, paths) in [(Warning::NotUptodate, &self.not_uptodate), (Warning::Orphaned, &self.orphaned)] {
            if paths.is_empty() {
                continue;
            }
            shown = true;
            let mut list = String::new();
            for p in paths {
                list.push_str(&format!("\t{}\n", p.to_str_lossy()));
            }
            eprintln!("warning: {}\n{list}", kind.porcelain());
        }
        if shown {
            eprintln!("After fixing the above paths, you may want to run `git sparse-checkout reapply`.");
        }
    }
}

/// Copy each entry's `CE_SKIP_WORKTREE` from the entry at the same path and stage of `from`,
/// the result index [`SparseCheckout::apply`] settled, onto the index a writer goes on to
/// write.
pub(crate) fn carry_skip_worktree_bits(from: &gix::index::State, to: &mut gix::index::State) {
    let bits: HashMap<(BString, u32), bool> = {
        let backing = from.path_backing();
        from.entries()
            .iter()
            .map(|e| ((e.path_in(backing).to_owned(), e.stage_raw()), e.flags.contains(Flags::SKIP_WORKTREE)))
            .collect()
    };
    let backing = to.path_backing().to_owned();
    for e in to.entries_mut() {
        match bits.get(&(e.path_in(&backing).to_owned(), e.stage_raw())) {
            Some(true) => e.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED),
            Some(false) => {
                e.flags.remove(Flags::SKIP_WORKTREE);
                if !e.flags.contains(Flags::INTENT_TO_ADD) {
                    e.flags.remove(Flags::EXTENDED);
                }
            }
            None => {}
        }
    }
}
