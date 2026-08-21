//! Marking parts of the tree-cache out of date when an index entry changes — the port of
//! `cache-tree.c`'s `do_invalidate_path()` / `cache_tree_invalidate_path()`.
//!
//! This is the half of cache-tree maintenance that runs on *every* index mutation, and it is
//! the half that has to be right: [`super::update`] can only rebuild what it has been told is
//! stale, so a path that is changed without being invalidated here leaves a node claiming a
//! tree object that no longer matches the entries below it. The next `git write-tree` would
//! then hand back that stale id — a wrong tree, silently.
//!
//! git calls it from every entry-level mutation: `add_index_entry_with_check()`
//! (read-cache.c:1273-1274), `remove_file_from_index()` (:632), `remove_marked_cache_entries()`
//! (:610), `chmod_index_entry()` (:935) and the rename in `rename_index_entry_at()` (:169).

use bstr::{BStr, ByteSlice};

use crate::{State, extension::Tree};

impl State {
    /// Invalidate the tree-cache along `path`, so the next
    /// [`cache_tree_update()`](State::cache_tree_update) recomputes everything that could have
    /// been affected by a change to that path. Returns whether there was a tree-cache to touch.
    ///
    /// Port of `cache_tree_invalidate_path()` (cache-tree.c:159-163). `path` is the entry's full
    /// path from the root of the index; it need not exist (a deletion invalidates the same
    /// nodes an addition would), and passing a path whose directories are not in the cache is
    /// harmless.
    ///
    /// git additionally sets `CACHE_TREE_CHANGED` in `istate->cache_changed`, which is what makes
    /// `write_locked_index(..., SKIP_IF_UNCHANGED)` write the index (read-cache.c:3333); callers
    /// here decide that for themselves, which is what the return value is for.
    pub fn invalidate_path_in_tree(&mut self, path: &BStr) -> bool {
        match self.tree.as_mut() {
            Some(tree) => {
                do_invalidate_path(tree, path);
                true
            }
            // `if (!it) return 0;` (cache-tree.c:130-131) — no cache-tree, nothing to invalidate.
            None => false,
        }
    }
}

/// Port of `do_invalidate_path()` (cache-tree.c:113-157), with git's own comment:
///
/// ```text
/// a/b/c
/// ==> invalidate self
/// ==> find "a", have it invalidate "b/c"
/// a
/// ==> invalidate self
/// ==> if "a" exists as a subtree, remove it.
/// ```
///
/// Two things about this are load-bearing. First, *every* node on the way down is invalidated,
/// not just the leaf: a changed blob changes the tree of each directory above it. Second, the
/// final component is *removed* rather than invalidated, because when `a` names a file the
/// subtree `a` must not exist at all, and when it names a directory the whole directory is
/// being rewritten — either way keeping its children would be keeping stale nodes alive.
fn do_invalidate_path(it: &mut Tree, path: &BStr) {
    // `it->entry_count = -1;` — unconditional, before recursing (cache-tree.c:134).
    it.num_entries = None;
    match path.find_byte(b'/') {
        None => {
            if let Some(pos) = it.children.iter().position(|child| child.name.as_slice() == path.as_bytes()) {
                it.children.remove(pos);
            }
        }
        Some(namelen) => {
            let (name, rest) = (&path[..namelen], &path[namelen + 1..]);
            // `down = find_subtree(it, path, namelen, 0); if (down) ...` — a directory that is
            // not cached needs no invalidation, because there is no cached id to be wrong.
            if let Some(child) = it.children.iter_mut().find(|child| child.name.as_slice() == name) {
                do_invalidate_path(child, rest.as_bstr());
            }
        }
    }
}
