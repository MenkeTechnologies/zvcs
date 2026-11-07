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
        patterns.map(|patterns| SparseCheckout { patterns, opts })
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
        let mut excludes = None;
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
            // (unpack-trees.c:2540-2541).
            if added[f] && !new && self.absent_rejects(repo, src, &mut excludes, path, &removed) {
                warnings.reject(Warning::Orphaned, path);
            }
            let left = self.apply_sparse_checkout(
                repo,
                src,
                &stat,
                &mut excludes,
                &mut warnings,
                e,
                path,
                (was, new),
                update,
                remove,
                &removed,
            );
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
    /// `CE_SKIP_WORKTREE` it leaves. A `CE_REMOVE` entry is one of `removed`.
    #[allow(clippy::too_many_arguments)]
    fn apply_sparse_checkout<'r>(
        &self,
        repo: &'r gix::Repository,
        src: &gix::index::File,
        stat: &crate::porcelain::read_tree::StatCtx,
        excludes: &mut Option<Option<gix::AttributeStack<'r>>>,
        warnings: &mut Warnings,
        e: &gix::index::Entry,
        path: &BString,
        (was, new): (bool, bool),
        update: &mut BTreeSet<BString>,
        remove: &mut BTreeSet<BString>,
        removed: &[BString],
    ) -> bool {
        let is_removed = removed.binary_search(path).is_ok();
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
            if !update.contains(path) && self.uptodate_rejects(repo, stat, e, path) {
                warnings.reject(Warning::NotUptodate, path);
                return false;
            }
            update.remove(path);
            remove.insert(path.clone());
        }
        if was && !new {
            if self.absent_rejects(repo, src, excludes, path, removed) {
                warnings.reject(Warning::Orphaned, path);
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

    /// `verify_absent_1(ce, error_type, COMPLETELY_ABSENT, o)` (unpack-trees.c:2479-2535) and
    /// the `check_ok_to_remove()` it ends in (:2419-2473), for a path whose index entry is to
    /// be written: whether something already in the worktree stands in its way.
    fn absent_rejects<'r>(
        &self,
        repo: &'r gix::Repository,
        src: &gix::index::File,
        excludes: &mut Option<Option<gix::AttributeStack<'r>>>,
        path: &BString,
        removed: &[BString],
    ) -> bool {
        // `UNPACK_RESET_OVERWRITE_UNTRACKED`: "...but nuke anything else".
        if self.opts.overwrite_untracked {
            return false;
        }
        let Some(workdir) = repo.workdir() else {
            return false;
        };
        // `check_leading_path()`: the first leading component that is not a directory is
        // what stands in the way; a missing one or a symlink leaves nothing to check.
        let mut name: &[u8] = path.as_slice();
        let mut at = 0;
        while let Some(slash) = path[at..].find_byte(b'/') {
            let lead = &path[..at + slash];
            match std::fs::symlink_metadata(workdir.join(gix::path::from_bstr(lead.as_bstr()).as_ref())) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(meta) if !meta.file_type().is_symlink() => {
                    name = lead;
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
            && crate::porcelain::reset::icase_exists(repo, src, name, &full, &meta)
        {
            return false;
        }
        if !self.opts.preserve_ignored {
            let stack = excludes.get_or_insert_with(|| {
                repo.excludes(src, None, gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped)
                    .ok()
            });
            let mode = if meta.is_dir() { Mode::DIR } else { Mode::FILE };
            if stack
                .as_mut()
                .is_some_and(|s| s.at_entry(name, Some(mode)).map(|p| p.is_excluded()).unwrap_or(false))
            {
                return false;
            }
        }
        // A directory goes to `verify_clean_subdirectory()`, whose own refusals are errors
        // rather than this warning; a directory where a file is to be written is refused here.
        if meta.is_dir() {
            return true;
        }
        // "The previous round may already have decided to delete this path".
        if removed.binary_search_by(|p| p.as_slice().cmp(name.as_bytes())).is_ok() {
            return false;
        }
        true
    }
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
