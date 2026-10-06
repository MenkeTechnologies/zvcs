//! A sparse index held expanded in memory while git would hold it collapsed.
//!
//! git keeps a sparse index *collapsed* — each wholly-excluded directory one `040000` entry —
//! for the whole run of a command that set `command_requires_full_index = 0`
//! (`repo_read_index()`, repository.c:459-460, and `ensure_correct_sparsity()`,
//! sparse-index.c:475-484), and writes it back as it stands: `convert_to_sparse()` returns at
//! once on an `INDEX_COLLAPSED` index (sparse-index.c:207), so neither the entries nor the
//! cache-tree are rebuilt on the way out. A command that reaches a path inside such a
//! directory expands the whole index first (`ensure_full_index()`) and the write then
//! collapses it again from scratch.
//!
//! Nothing in this port can work over a sparse-directory entry, so every index is expanded as
//! it is read. To still write what git writes, an index that git would be holding collapsed
//! remembers the directories it was expanded from — a [`VirtualSparseDir`] each — and stays
//! marked sparse (`istate->sparse_index == INDEX_COLLAPSED`). The write puts each of them back
//! as the single entry git never stopped holding, and turns the cache-tree back into the shape
//! git's has over the collapsed entries, provided the command left every one of them exactly as
//! it was expanded. If it did not, git would have expanded the index for real, and so does
//! [`State::forget_virtual_sparse_dirs()`].

use bstr::{BStr, BString, ByteSlice};

use crate::{
    State,
    entry::{Flags, Mode, Stage, Stat},
    extension::Tree,
};

/// One sparse-directory entry that was expanded on read only because this port needs a full
/// index, together with what it expanded to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualSparseDir {
    /// The directory's index name, with git's trailing `/`.
    pub path: BString,
    /// The tree the sparse-directory entry named.
    pub id: gix_hash::ObjectId,
    /// The entries `add_path_to_index()` (sparse-index.c:275-328) made of it, in index order:
    /// full path, blob id and mode. Each one was pushed `CE_SKIP_WORKTREE` with no stat data.
    pub entries: Vec<(BString, gix_hash::ObjectId, Mode)>,
}

/// The flags `construct_sparse_dir_entry()` (sparse-index.c:44-55) and `add_path_to_index()`
/// give their entries: `CE_SKIP_WORKTREE`, which only an extended entry can carry.
fn sparse_flags() -> Flags {
    Flags::SKIP_WORKTREE | Flags::EXTENDED
}

/// The components of `dir` (`a/b/`) as cache-tree node names.
fn components(dir: &BStr) -> Vec<&[u8]> {
    dir.split_str("/").filter(|c| !c.is_empty()).collect()
}

/// Apply `f` to every node from the root down to (and including) the node named by
/// `components`, as far as the cache-tree has them; `f` learns whether the node is the last
/// one on the path.
fn walk_path(tree: &mut Tree, components: &[&[u8]], f: &mut dyn FnMut(&mut Tree, bool)) {
    f(tree, components.is_empty());
    let Some((first, rest)) = components.split_first() else { return };
    if let Some(child) = tree.children.iter_mut().find(|c| c.name.as_slice() == *first) {
        walk_path(child, rest, f);
    }
}

impl State {
    /// The sparse directories this state is holding expanded although git would hold them
    /// collapsed. Empty unless [`is_sparse()`](State::is_sparse()).
    pub fn virtual_sparse_dirs(&self) -> &[VirtualSparseDir] {
        &self.virtual_sparse_dirs
    }

    /// Whether any entry is a sparse-directory entry (`S_ISSPARSEDIR`). Unlike
    /// [`is_sparse()`](State::is_sparse()), which is git's `istate->sparse_index`, this is
    /// false for an index whose sparse directories were expanded on read.
    pub fn has_sparse_dir_entries(&self) -> bool {
        self.entries.iter().any(|e| e.mode == Mode::DIR)
    }

    /// Mark this state `INDEX_COLLAPSED`: git's `istate->sparse_index = 1`, which writes the
    /// `sdir` extension and keeps `convert_to_sparse()` from touching the index on write.
    pub fn set_collapsed(&mut self) {
        self.is_sparse = true;
    }

    /// Take over `src`'s sparsity — collapsed or not, and the directories it was read with —
    /// for a state that replaces it while git would have gone on holding `src` itself, as
    /// `read_from_tree()` stages into the index it read rather than into a new one.
    pub fn inherit_sparse_index(&mut self, src: &State) {
        self.is_sparse = src.is_sparse;
        self.virtual_sparse_dirs = src.virtual_sparse_dirs.clone();
    }

    /// Replace the remembered sparse directories — for a command that moved the index onto
    /// another tree, where git's `unpack_trees()` carries each sparse-directory entry across
    /// as the new tree's directory.
    pub fn set_virtual_sparse_dirs(&mut self, dirs: Vec<VirtualSparseDir>) {
        self.virtual_sparse_dirs = dirs;
    }

    /// The remembered sparse directory `path` lies strictly inside, if any — the
    /// sparse-directory entry git's `index_name_pos()` finds just before it
    /// (read-cache.c:543-560).
    pub fn virtual_sparse_dir_containing(&self, path: &BStr) -> Option<&VirtualSparseDir> {
        self.virtual_sparse_dirs
            .iter()
            .find(|d| path.len() > d.path.len() && path.starts_with(d.path.as_slice()))
    }

    /// git's `ensure_full_index()` for an index whose sparse directories this port had already
    /// expanded: it is a full index from here on (`INDEX_EXPANDED`), and the directories it was
    /// read with are forgotten. Returns whether the state was collapsed before.
    pub fn forget_virtual_sparse_dirs(&mut self) -> bool {
        let was = self.is_sparse;
        self.virtual_sparse_dirs.clear();
        self.is_sparse = false;
        was
    }

    /// Replace the sparse-directory entry at `dir.path` by `dir.entries` and remember `dir`, so
    /// the write can put the entry back.
    ///
    /// `subtree` is the cache-tree of the expanded directory, rooted at the directory itself:
    /// it takes the place of the directory's node, which over the collapsed entry is a leaf
    /// covering one entry, and every valid node above it now covers `n - 1` more entries.
    pub fn expand_sparse_dir_virtually(&mut self, dir: VirtualSparseDir, subtree: Option<Tree>) {
        let path = dir.path.clone();
        self.remove_entries(|_, p, e| e.mode == Mode::DIR && p == path.as_bstr());
        for (p, id, mode) in &dir.entries {
            self.dangerously_push_entry(Stat::default(), *id, sparse_flags(), *mode, p.as_bstr());
        }
        self.sort_entries();

        let grown = dir.entries.len() as i64 - 1;
        if let Some(tree) = self.tree.as_mut() {
            let comps = components(path.as_bstr());
            walk_path(tree, &comps, &mut |node, last| {
                if last {
                    if let Some(sub) = subtree.as_ref() {
                        node.children = sub.children.clone();
                        node.num_entries = node.num_entries.and(sub.num_entries);
                    }
                } else if let Some(n) = node.num_entries.as_mut() {
                    *n = (*n as i64 + grown) as u32;
                }
            });
        }
        self.virtual_sparse_dirs.push(dir);
    }

    /// Put every remembered sparse directory back as the single entry it was read as, and the
    /// cache-tree back into the shape it has over the collapsed entries.
    ///
    /// All or nothing: if any directory no longer holds exactly what it was expanded to — an
    /// entry changed, lost `CE_SKIP_WORKTREE`, gained a stage, or a path was added or removed
    /// below it — the state is left untouched and `false` is returned, because git could not
    /// have changed it without expanding the whole index first.
    pub fn collapse_virtual_sparse_dirs(&mut self) -> bool {
        if self.virtual_sparse_dirs.is_empty() {
            return true;
        }
        let mut ranges = Vec::with_capacity(self.virtual_sparse_dirs.len());
        for dir in &self.virtual_sparse_dirs {
            let start = self
                .entries
                .partition_point(|e| e.path(self).as_bytes() < dir.path.as_slice());
            let len = self.entries[start..]
                .iter()
                .take_while(|e| e.path(self).starts_with(dir.path.as_slice()))
                .count();
            if len != dir.entries.len() {
                return false;
            }
            let same = self.entries[start..start + len]
                .iter()
                .zip(&dir.entries)
                .all(|(e, (p, id, mode))| {
                    e.path(self) == p.as_bstr()
                        && e.id == *id
                        && e.mode == *mode
                        && e.stage() == Stage::Unconflicted
                        && e.flags.contains(Flags::SKIP_WORKTREE)
                });
            if !same {
                return false;
            }
            ranges.push(start..start + len);
        }

        let dirs = std::mem::take(&mut self.virtual_sparse_dirs);
        let mut idx = 0;
        let mut ri = 0;
        self.entries.retain(|_| {
            while ri < ranges.len() && idx >= ranges[ri].end {
                ri += 1;
            }
            let keep = !(ri < ranges.len() && ranges[ri].contains(&idx));
            idx += 1;
            keep
        });
        for dir in &dirs {
            self.dangerously_push_entry(Stat::default(), dir.id, sparse_flags(), Mode::DIR, dir.path.as_bstr());
            let shrunk = dir.entries.len() as u32 - 1;
            if let Some(tree) = self.tree.as_mut() {
                let comps = components(dir.path.as_bstr());
                walk_path(tree, &comps, &mut |node, last| {
                    if last {
                        node.children.clear();
                        node.num_entries = node.num_entries.map(|_| 1);
                    } else if let Some(n) = node.num_entries.as_mut() {
                        *n -= shrunk;
                    }
                });
            }
        }
        self.sort_entries();
        // Kept, so the expansion can be redone after the write — git's in-memory index never
        // changed, and the caller may go on using it.
        self.virtual_sparse_dirs = dirs;
        true
    }

    /// The entry-list half of `convert_to_sparse_rec()` (sparse-index.c:60-130): replace every
    /// entry below each of `dirs` — `(name with trailing '/', tree id)` — by one
    /// sparse-directory entry, `construct_sparse_dir_entry()`'s (sparse-index.c:44-55), and mark
    /// the state `INDEX_COLLAPSED`. Deciding which directories may collapse, and rebuilding the
    /// cache-tree over the result, is the caller's.
    pub fn collapse_into_sparse_dirs(&mut self, dirs: &[(BString, gix_hash::ObjectId)]) {
        if !dirs.is_empty() {
            self.remove_entries(|_, p, _| dirs.iter().any(|(d, _)| p.starts_with(d.as_slice())));
            for (d, id) in dirs {
                self.dangerously_push_entry(Stat::default(), *id, sparse_flags(), Mode::DIR, d.as_bstr());
            }
            self.sort_entries();
        }
        self.virtual_sparse_dirs.clear();
        self.is_sparse = true;
    }

    /// Undo [`collapse_virtual_sparse_dirs()`](State::collapse_virtual_sparse_dirs()) after a
    /// write: the remembered directories are expanded again with the subtrees `subtree_of`
    /// supplies.
    pub fn reexpand_virtual_sparse_dirs(&mut self, subtree_of: &mut dyn FnMut(&VirtualSparseDir) -> Option<Tree>) {
        let dirs = std::mem::take(&mut self.virtual_sparse_dirs);
        for dir in dirs {
            let sub = subtree_of(&dir);
            self.expand_sparse_dir_virtually(dir, sub);
        }
    }
}
