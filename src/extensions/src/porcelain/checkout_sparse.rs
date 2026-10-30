//! `checkout_paths()` (builtin/checkout.c:517-719) over a sparse index.
//!
//! `checkout_main()` clears `command_requires_full_index` (builtin/checkout.c:1904), so the
//! index `checkout <tree-ish> -- <paths>` and `restore` work on is the collapsed one: each
//! directory the cone leaves out is a single `040000` sparse-directory entry flagged
//! `CE_SKIP_WORKTREE`, and nothing inside it is an entry of its own. `checkout.c` never calls
//! `pathspec_needs_expanded_index()` and never expands on purpose. What reaches inside a
//! sparse directory is exactly what C does with that one entry:
//!
//! * `read_tree_some()` walks the source tree, and `update_some()` hands each tree it reaches
//!   to `try_update_sparse_directory()` (builtin/checkout.c:149-190). The same tree is not
//!   descended into; in no-overlay mode a different one replaces the entry's object id
//!   without descending (`oidcpy()` + `CE_UPDATE`, no cache-tree invalidation); in overlay
//!   mode a different one is descended into, and each blob below it goes through
//!   `add_index_entry()`. That call appends a name sorting after the last entry without
//!   searching (read-cache.c:1262-1265), which leaves the blob *next to* the sparse
//!   directory, and searches every other name with `EXPAND_SPARSE`, which expands the whole
//!   index the first time the entry before the insertion point is a sparse directory holding
//!   the name (read-cache.c:543-560).
//! * `mark_ce_for_checkout_*()` (builtin/checkout.c:387-438) sees the sparse directory as one
//!   skip-worktree entry. Only `--ignore-skip-worktree-bits` lets the pathspec reach it, and
//!   then `ce_path_match()` matches its name as a directory: `out` and `.` select `out/`,
//!   `out/b` does not.
//! * `checkout_worktree()` hands a matched sparse directory to `checkout_entry()`, which
//!   creates its leading directories and fails in `write_entry()` with
//!   `unknown file mode for out/ in index` (entry.c:408-409); one the no-overlay marker gave
//!   `CE_WT_REMOVE` goes to `unlink_entry()` and leaves the index.
//!
//! This port reads every index expanded ([`crate::sparse_index`]). [`SparseDirs::prepare`]
//! turns the index a path checkout just read back into the collapsed one git holds, works out
//! what the steps above do to each sparse directory, and — when `add_index_entry()` would have
//! expanded the index — expands it for real instead, so the caller's full-index code then
//! runs over what git runs over.

use std::collections::HashSet;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::ObjectId;

use crate::pathspec::{MAGIC_EXCLUDE, MAGIC_GLOB, MAGIC_ICASE, MAGIC_LITERAL, MAGIC_TOP};

/// One sparse-directory entry of the collapsed index.
struct Dir {
    /// The entry's name, with git's trailing `/`.
    path: BString,
    /// Its object id — the source tree's when `try_update_sparse_directory()` replaced it.
    id: ObjectId,
    /// `CE_UPDATE`: no-overlay `try_update_sparse_directory()` moved it to the source tree.
    update: bool,
    /// `CE_MATCHED`.
    matched: bool,
    /// `CE_REMOVE | CE_WT_REMOVE` from `mark_ce_for_checkout_no_overlay()`.
    remove: bool,
}

/// One `pathspec_item`, for the magic this port matches against a sparse directory itself.
struct Item {
    /// `item->match`: the element relative to the top of the work tree.
    mat: BString,
    /// `item->prefix`: how much of `mat` is the command's prefix, always compared exactly.
    prefix: usize,
    /// `item->nowildcard_len`.
    nowildcard_len: usize,
    /// `PATHSPEC_EXCLUDE`.
    exclude: bool,
    /// `PATHSPEC_ICASE`.
    icase: bool,
    /// `PATHSPEC_GLOB`.
    glob: bool,
    /// The element of the command line this item came from; `None` for the empty item
    /// `parse_pathspec()` adds when every element is an exclusion (pathspec.c:649-656).
    element: Option<usize>,
}

impl Item {
    /// `ps_strncmp()` (pathspec.h): whether the first `n` bytes of `a` and `b` are equal,
    /// ignoring case under `:(icase)`. A string shorter than `n` ends in git's NUL.
    fn eq_n(&self, a: &[u8], b: &[u8], n: usize) -> bool {
        let (a, b) = (&a[..n.min(a.len())], &b[..n.min(b.len())]);
        if self.icase { a.eq_ignore_ascii_case(b) } else { a == b }
    }

    /// `basecmp()` (tree-walk.c:530-544): the command's prefix compares exactly even under
    /// `:(icase)`.
    fn base_eq(&self, base: &[u8], m: &[u8], len: usize) -> bool {
        if self.icase {
            let n = len.min(self.prefix);
            if base[..n.min(base.len())] != m[..n.min(m.len())] {
                return false;
            }
            return self.eq_n(&base[n.min(base.len())..], &m[n.min(m.len())..], len - n);
        }
        self.eq_n(base, m, len)
    }

    /// `git_fnmatch()` (dir.c:113-143): the first `prefix` bytes compare as `ps_strncmp()`
    /// does, the rest goes to `wildmatch()`, with `WM_PATHNAME` under `:(glob)` and
    /// `WM_CASEFOLD` under `:(icase)`.
    fn fnmatch(&self, pattern: &[u8], string: &[u8], prefix: usize) -> bool {
        let mut prefix = prefix;
        if prefix > 0 {
            if !self.eq_n(pattern, string, prefix) || string.len() < prefix || pattern.len() < prefix {
                return false;
            }
        } else {
            prefix = 0;
        }
        let mut mode = gix::glob::wildmatch::Mode::empty();
        if self.glob {
            mode |= gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL;
        }
        if self.icase {
            mode |= gix::glob::wildmatch::Mode::IGNORE_CASE;
        }
        gix::glob::wildmatch(pattern[prefix..].as_bstr(), string[prefix..].as_bstr(), mode)
    }
}

/// `enum interesting` (tree-walk.h).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Interesting {
    AllEntriesNot = -1,
    EntryNot = 0,
    Entry = 1,
    AllEntries = 2,
}

/// What `checkout_paths()` does to the sparse-directory entries of the index it read.
#[derive(Default)]
pub(super) struct SparseDirs {
    dirs: Vec<Dir>,
    /// Blobs below a sparse directory that `add_index_entry()` appended after it.
    appended: HashSet<BString>,
    /// The blob whose `add_index_entry()` expanded the index: looked up before the expansion,
    /// it is always a fresh `create_ce_flags(0)` entry, even where the tree agrees with the
    /// entry the expansion made.
    fresh: Option<BString>,
    /// `ps_matched` slots a sparse directory filled, by command-line element.
    hits: Vec<bool>,
}

impl SparseDirs {
    /// `read_tree_some()` and the marking pass of `checkout_paths()` as far as sparse
    /// directories are concerned, run on `index` right after it was read.
    ///
    /// `source` is `opts->source_tree`, `overlay` is `opts->overlay_mode`, and `selects` is the
    /// caller's pathspec match of a blob path. An index that is not collapsed is left alone
    /// and nothing is planned. Otherwise `index` becomes the collapsed index, or the expanded
    /// one if `add_index_entry()` expanded it.
    pub(super) fn prepare(
        repo: &gix::Repository,
        index: &mut gix::index::State,
        specs: &[&str],
        source: Option<ObjectId>,
        overlay: bool,
        ignore_skipworktree: bool,
        selects: &dyn Fn(&[u8]) -> bool,
    ) -> anyhow::Result<Self> {
        if !index.is_sparse() || index.virtual_sparse_dirs().is_empty() {
            return Ok(Self::default());
        }
        let Some(items) = items(repo, specs) else {
            return Ok(Self::default());
        };
        let mut dirs: Vec<Dir> = index
            .virtual_sparse_dirs()
            .iter()
            .map(|d| Dir { path: d.path.clone(), id: d.id, update: false, matched: false, remove: false })
            .collect();
        let mut plan = Self { hits: vec![false; specs.len()], ..Self::default() };

        if let Some(tree) = source {
            let root = repo.find_tree(tree)?;
            let source_dir = |path: &BString| {
                let name = path.strip_suffix(b"/").unwrap_or(path);
                root.lookup_entry(name.split(|b| *b == b'/'))
                    .ok()
                    .flatten()
                    .filter(|e| e.mode().is_tree())
                    .map(|e| e.object_id())
            };
            // `read_tree_at()` only calls `update_some()` on a tree every directory above it
            // led to; a different tree is either replaced in place or descended into.
            let changed: Vec<Option<ObjectId>> = dirs
                .iter()
                .map(|d| source_dir(&d.path).filter(|id| *id != d.id && reaches(&items, &d.path)))
                .collect();
            if !overlay {
                for (dir, id) in dirs.iter_mut().zip(&changed) {
                    if let Some(id) = id {
                        dir.id = *id;
                        dir.update = true;
                    }
                }
            } else if changed.iter().any(Option::is_some) {
                // `add_index_entry()` for each blob `read_tree()` reaches, in tree order.
                let mut last = collapsed_last(index);
                let blobs = repo.index_from_tree(&tree)?;
                for e in blobs.entries() {
                    let path = e.path(&blobs);
                    if !selects(path) {
                        continue;
                    }
                    match dirs.iter().position(|d| path.starts_with(d.path.as_slice())) {
                        Some(i) if changed[i].is_some() => {
                            if path > last.as_bstr() {
                                last = path.to_owned();
                                plan.appended.insert(path.to_owned());
                                continue;
                            }
                            // `index_name_stage_pos(…, EXPAND_SPARSE)` finds the sparse
                            // directory before the insertion point and `ensure_full_index()`s.
                            crate::sparse_index::ensure_full_index(repo, index);
                            plan.fresh = Some(path.to_owned());
                            plan.appended.clear();
                            return Ok(plan);
                        }
                        Some(_) => {}
                        None => {
                            // `update_some()` keeps an entry the tree agrees with
                            // (builtin/checkout.c:219-229); anything else is added, and only
                            // a name after the last entry moves the end of the index.
                            let kept = index.entry_index_by_path(path).ok().is_some_and(|at| {
                                let old = &index.entries()[at];
                                old.stage_raw() == 0
                                    && old.mode == e.mode
                                    && old.id == e.id
                                    && !old.flags.contains(gix::index::entry::Flags::INTENT_TO_ADD)
                            });
                            if !kept && path > last.as_bstr() {
                                last = path.to_owned();
                            }
                        }
                    }
                }
            }
        }

        // The index git holds: every sparse directory one entry again.
        if !index.collapse_virtual_sparse_dirs() {
            return Ok(Self::default());
        }
        index.set_virtual_sparse_dirs(Vec::new());

        // `mark_ce_for_checkout_overlay()` / `_no_overlay()` for each sparse directory.
        if ignore_skipworktree {
            for dir in &mut dirs {
                if overlay && source.is_some() && !dir.update {
                    continue;
                }
                if !ce_path_match(&items, &dir.path, &mut plan.hits) {
                    continue;
                }
                dir.matched = true;
                if !overlay && source.is_some() && !dir.update {
                    dir.remove = true;
                }
            }
        }
        plan.dirs = dirs;
        Ok(plan)
    }

    /// Whether `read_tree_some()` never reaches the source tree's blob `path` as an entry of
    /// its own: it lies inside a sparse directory and was not appended next to it.
    pub(super) fn hides_tree_path(&self, path: &[u8]) -> bool {
        self.dirs.iter().any(|d| path.starts_with(d.path.as_slice())) && !self.appended.contains(path.as_bstr())
    }

    /// Whether `update_some()` replaces the entry at `path` whatever it holds.
    pub(super) fn is_fresh(&self, path: &[u8]) -> bool {
        self.fresh.as_ref().is_some_and(|f| f.as_slice() == path)
    }

    /// Whether a sparse directory filled the `ps_matched` slot of element `si`.
    pub(super) fn hit(&self, si: usize) -> bool {
        self.hits.get(si).copied().unwrap_or(false)
    }

    /// The sparse-directory entries' share of the index `checkout_paths()` writes:
    /// `try_update_sparse_directory()`'s new object ids, which invalidate nothing, and
    /// `remove_marked_cache_entries(…, 1)` (read-cache.c:603-623) dropping the ones marked
    /// `CE_REMOVE`, each invalidating its path.
    pub(super) fn update_index(&self, index: &mut gix::index::State) {
        for dir in &self.dirs {
            let Ok(at) = index.entry_index_by_path(dir.path.as_bstr()) else {
                continue;
            };
            if dir.remove {
                index.remove_entries(|i, _, _| i == at);
                index.invalidate_path_in_tree(dir.path.as_bstr());
            } else if dir.update {
                index.entries_mut()[at].id = dir.id;
            }
        }
    }

    /// The matched sparse directory whose `checkout_entry()` aborts: the first, in index order,
    /// whose name `lstat()`s. `checkout_worktree()` walks the index in order
    /// (builtin/checkout.c:452-465), so nothing that sorts after it is written.
    fn aborts_at(&self, workdir: &std::path::Path) -> Option<&BString> {
        self.dirs
            .iter()
            .filter(|d| d.matched && !d.remove)
            .map(|d| &d.path)
            .find(|p| std::fs::symlink_metadata(workdir.join(gix::path::from_bstr(p.as_bstr()))).is_ok())
    }

    /// Drop from the files `checkout_worktree()` is about to write (`subset`) and unlink
    /// (`remove`) every path its walk never reaches because a sparse directory before it aborts.
    pub(super) fn truncate_at_abort(
        &self,
        repo: &gix::Repository,
        subset: &mut gix::index::File,
        remove: &mut Vec<BString>,
    ) {
        let Some(at) = repo.workdir().and_then(|w| self.aborts_at(w)).cloned() else {
            return;
        };
        subset.remove_entries(|_, path, _| path >= at.as_bstr());
        remove.retain(|path| path < &at);
    }

    /// `checkout_worktree()`'s `checkout_entry()` for each matched sparse directory. Returns
    /// whether one failed, which makes `checkout_paths()` return -1.
    pub(super) fn checkout_worktree(&self, repo: &gix::Repository) -> anyhow::Result<bool> {
        let Some(workdir) = repo.workdir() else { return Ok(false) };
        let mut errs = false;
        for dir in self.dirs.iter().filter(|d| d.matched) {
            if dir.remove {
                unlink_entry(workdir, &dir.path);
                continue;
            }
            // `checkout_entry_ca()` (entry.c:482-588): when the name does not `lstat()`, its
            // leading directories are created and `write_entry()` refuses the mode. When it
            // does, `ie_match_stat()` reaches `ce_match_stat_basic()`, whose switch has no
            // case for a directory: `BUG()` aborts with the index still locked by
            // `repo_hold_locked_index()` (builtin/checkout.c:627).
            let full = workdir.join(gix::path::from_bstr(dir.path.as_bstr()));
            if std::fs::symlink_metadata(&full).is_ok() {
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(repo.git_dir().join("index.lock"));
                eprintln!("BUG: read-cache.c:339: unsupported ce_mode: 40000");
                std::process::abort();
            }
            create_directories(workdir, &dir.path)?;
            eprintln!("error: unknown file mode for {} in index", dir.path);
            errs = true;
        }
        Ok(errs)
    }
}

/// The name of the last entry of the collapsed index `index` stands for.
fn collapsed_last(index: &gix::index::State) -> BString {
    let dirs = index.virtual_sparse_dirs();
    let file = index
        .entries()
        .iter()
        .map(|e| e.path(index))
        .filter(|p| index.virtual_sparse_dir_containing(p).is_none())
        .max();
    let dir = dirs.iter().map(|d| d.path.as_bstr()).max();
    file.max(dir).map(ToOwned::to_owned).unwrap_or_default()
}

/// `parse_pathspec()`'s items for `specs`, or `None` when an element carries `:(attr)` (or
/// `:(prefix)`), whose matching against a directory this port does not have.
fn items(repo: &gix::Repository, specs: &[&str]) -> Option<Vec<Item>> {
    let cwd: Vec<u8> = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| gix::path::into_bstr(p).into_owned().into())
        .filter(|p: &Vec<u8>| !p.is_empty())
        .map(|mut p| {
            p.push(b'/');
            p
        })
        .unwrap_or_default();
    let mut items = Vec::with_capacity(specs.len() + 1);
    for (si, spec) in specs.iter().enumerate() {
        let elt = BStr::new(spec.as_bytes());
        let element = crate::pathspec::parse_element_magic(elt).ok()?;
        let known = MAGIC_TOP | MAGIC_LITERAL | MAGIC_EXCLUDE | MAGIC_GLOB | MAGIC_ICASE;
        if element.magic & !known != 0 || element.prefix_magic {
            return None;
        }
        let path = element.path(elt);
        // `init_pathspec_item()` (pathspec.c:481-505): `:(top)` keeps `copyfrom` as it is,
        // anything else goes through `prefix_path_gently()`.
        let (mat, prefix) = if element.rooted() {
            (path.to_owned(), 0)
        } else {
            let mat = crate::pathspec::prefix_path(repo, path).ok()?;
            let prefix = if mat.starts_with(&cwd) { cwd.len() } else { 0 };
            (mat, prefix)
        };
        let nowildcard_len = if element.magic & MAGIC_LITERAL != 0 {
            mat.len()
        } else {
            mat.iter()
                .position(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'))
                .unwrap_or(mat.len())
                .max(prefix)
        };
        items.push(Item {
            mat,
            prefix,
            nowildcard_len,
            exclude: element.magic & MAGIC_EXCLUDE != 0,
            icase: element.magic & MAGIC_ICASE != 0,
            glob: element.magic & MAGIC_GLOB != 0,
            element: Some(si),
        });
    }
    if !items.is_empty() && items.iter().all(|i| i.exclude) {
        items.push(Item {
            mat: BString::default(),
            prefix: 0,
            nowildcard_len: 0,
            exclude: false,
            icase: false,
            glob: false,
            element: None,
        });
    }
    Some(items)
}

/// `match_pathspec_item()` (dir.c:349-460) for a sparse directory `name` (trailing `/`
/// included) under `DO_MATCH_DIRECTORY`.
fn match_pathspec_item(item: &Item, name: &[u8]) -> bool {
    let m = item.mat.as_slice();
    if item.prefix > 0 && item.icase && m[..item.prefix] != name[..item.prefix.min(name.len())] {
        return false;
    }
    if m.is_empty() {
        return true;
    }
    if m.len() <= name.len() && item.eq_n(m, name, m.len()) {
        if m.len() == name.len() || m[m.len() - 1] == b'/' || name[m.len()] == b'/' {
            return true;
        }
    } else if m[m.len() - 1] == b'/' && name.len() == m.len() - 1 && item.eq_n(m, name, name.len()) {
        return true;
    }
    item.nowildcard_len < m.len() && item.fnmatch(m, name, item.nowildcard_len)
}

/// `ce_path_match()` → `match_pathspec_with_flags()` (dir.c:540-560) for a sparse directory,
/// filling `seen` as `do_match_pathspec()` does — per positive item, before any exclusion.
fn ce_path_match(items: &[Item], name: &[u8], seen: &mut [bool]) -> bool {
    let mut positive = false;
    for item in items.iter().rev().filter(|i| !i.exclude) {
        if match_pathspec_item(item, name) {
            positive = true;
            if let Some(si) = item.element {
                seen[si] = true;
            }
        }
    }
    if !positive {
        return false;
    }
    !items.iter().filter(|i| i.exclude).any(|i| match_pathspec_item(i, name))
}

/// Does `read_tree_at()` reach the directory `path` (trailing `/`) and call `update_some()` on
/// it? Each directory on the way must be interesting (`tree_entry_interesting()`), and the
/// pathspec is recursive (`opts->pathspec.recursive = 1`, builtin/checkout.c:2075).
fn reaches(items: &[Item], path: &[u8]) -> bool {
    let mut base = 0;
    while let Some(slash) = path[base..].find_byte(b'/') {
        let end = base + slash;
        if tree_entry_interesting(items, &path[..base], &path[base..end]) <= Interesting::EntryNot {
            return false;
        }
        base = end + 1;
    }
    true
}

/// `tree_entry_interesting()` (tree-walk.c:785-858) for a directory `name` in `base`.
fn tree_entry_interesting(items: &[Item], base: &[u8], name: &[u8]) -> Interesting {
    let positive = do_match(items, base, name, false);
    if !items.iter().any(|i| i.exclude) || positive <= Interesting::EntryNot {
        return positive;
    }
    let negative = do_match(items, base, name, true);
    if positive == Interesting::AllEntries && negative == Interesting::EntryNot {
        return Interesting::Entry;
    }
    if negative <= Interesting::EntryNot {
        return positive;
    }
    if positive >= Interesting::Entry && negative == Interesting::Entry {
        return Interesting::Entry;
    }
    if (positive == Interesting::Entry && negative >= Interesting::Entry)
        || (positive == Interesting::AllEntries && negative == Interesting::Entry)
    {
        return Interesting::EntryNot;
    }
    Interesting::AllEntriesNot
}

/// `do_match()` (tree-walk.c:562-783) for a directory entry `name` in `base`, without
/// `max_depth` or attributes. Every wildcard item that gets as far as the fnmatch makes a
/// directory interesting, matched or not ("Match all directories").
fn do_match(items: &[Item], base: &[u8], name: &[u8], exclude: bool) -> Interesting {
    let has_wildcard = items.iter().any(|i| i.nowildcard_len < i.mat.len());
    let never = if has_wildcard { Interesting::EntryNot } else { Interesting::AllEntriesNot };
    for item in items.iter().rev().filter(|i| i.exclude == exclude) {
        let m = item.mat.as_slice();
        let wild = item.nowildcard_len < m.len();
        if base.len() >= m.len() {
            // `match_dir_prefix()`: below a directory the item names, everything is.
            if item.base_eq(base, m, m.len())
                && (m.is_empty() || base.get(m.len()).is_none_or(|b| *b == b'/') || m[m.len() - 1] == b'/')
            {
                return Interesting::AllEntries;
            }
        } else if base.is_empty() || item.base_eq(base, m, base.len()) {
            // `match_entry()`: the item names this directory or a path inside it.
            let rest = &m[base.len()..];
            if rest.len() >= name.len()
                && (rest.len() == name.len() || rest[name.len()] == b'/')
                && item.eq_n(rest, name, name.len())
            {
                return Interesting::Entry;
            }
            if wild {
                return Interesting::Entry;
            }
            continue;
        }
        if !wild {
            continue;
        }
        // `match_wildcard_base()`: the literal part of the item must agree with the base.
        if item.nowildcard_len > 0 && !base.is_empty() {
            let agrees = if base.len() >= item.nowildcard_len {
                item.base_eq(base, m, item.nowildcard_len)
            } else {
                item.base_eq(base, m, base.len())
            };
            if !agrees {
                continue;
            }
        }
        return Interesting::Entry;
    }
    never
}

/// `create_directories()` (entry.c:24-64) for the leading directories of `path` under
/// `workdir`, with `state->force` set as `checkout_worktree()` sets it.
fn create_directories(workdir: &std::path::Path, path: &[u8]) -> anyhow::Result<()> {
    let mut len = 0;
    while let Some(slash) = path[len..].find_byte(b'/').map(|s| len + s) {
        len = slash + 1;
        if slash == 0 {
            continue;
        }
        let rel = &path[..slash];
        let dir = workdir.join(gix::path::from_bstr(rel.as_bstr()));
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if let Err(err) = std::fs::create_dir(&dir) {
            let retried = err.kind() == std::io::ErrorKind::AlreadyExists
                && std::fs::remove_file(&dir).is_ok()
                && std::fs::create_dir(&dir).is_ok();
            if !retried {
                crate::git_fatal!("cannot create directory at '{}': {}", rel.as_bstr(), crate::external::strerror(&err));
            }
        }
    }
    Ok(())
}

/// `unlink_entry()` (entry.c:590-606) for a sparse directory: nothing unless every leading
/// directory is a real one (`check_leading_path()`), then `unlink_or_warn()`.
fn unlink_entry(workdir: &std::path::Path, path: &[u8]) {
    let name = path.strip_suffix(b"/").unwrap_or(path);
    let mut at = 0;
    loop {
        let end = name[at..].find_byte(b'/').map_or(name.len(), |s| at + s);
        let dir = workdir.join(gix::path::from_bstr(name[..end].as_bstr()));
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            return;
        }
        if end == name.len() {
            break;
        }
        at = end + 1;
    }
    let full = workdir.join(gix::path::from_bstr(path.as_bstr()));
    if let Err(err) = std::fs::remove_file(&full) {
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!("warning: unable to unlink '{}': {}", path.as_bstr(), crate::external::strerror(&err));
        }
    }
}
