//! Building a tree-cache straight from a tree object — the port of
//! `cache-tree.c`'s `prime_cache_tree()` / `prime_cache_tree_rec()`.
//!
//! This is the shortcut `git read-tree` takes when it read exactly one tree and no
//! `--prefix`: "the index must match exactly what came from the tree", so every
//! node's id and entry count can be read off the tree objects instead of being
//! recomputed from the entries (builtin/read-tree.c:281-290). No object is written
//! and no entry is hashed; the tree is already in the odb by definition.
//!
//! The precondition is the dangerous part. Priming asserts that the index equals
//! the tree, and nothing in the resulting cache-tree records which entries it was
//! derived from — so a caller that primes after building an index that does *not*
//! match hands the next `write-tree` a confidently wrong answer. Only call it where
//! git does.

use bstr::ByteSlice;
use gix_hash::ObjectId;
use gix_object::{FindExt, TreeRefIter};

use crate::{State, extension::Tree};

/// The error returned by [`State::prime_cache_tree()`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A tree object named by the tree being walked could not be read.
    #[error(transparent)]
    Find(#[from] gix_object::find::existing_iter::Error),
    /// A tree object was present but malformed.
    #[error("could not decode tree object {id}")]
    Decode {
        /// The tree that would not parse.
        id: ObjectId,
    },
}

impl State {
    /// Replace the tree-cache with one derived from the tree object `root`, which the
    /// caller guarantees this index is an exact expansion of.
    ///
    /// Port of `prime_cache_tree()` (cache-tree.c:897-911). Every node comes out valid,
    /// with `entry_count` set to the number of non-tree entries reachable below it
    /// (gitlinks included — `S_ISDIR` is false for mode `160000`, so
    /// `prime_cache_tree_rec()` counts them like blobs, cache-tree.c:856-857).
    ///
    /// A **sparse** index is refused: there the cache-tree has "leaf" nodes standing in
    /// for whole directories that are not expanded in the index at all, which git
    /// detects with an `index_entry_exists()` probe per directory
    /// (cache-tree.c:878-889). Rather than model that, this drops the extension and
    /// returns `Ok(())` — the caller loses the shortcut and nothing else.
    pub fn prime_cache_tree(&mut self, objects: &impl gix_object::Find, root: &gix_hash::oid) -> Result<(), Error> {
        let _span = gix_features::trace::coarse!("gix_index::State::prime_cache_tree()");
        // `cache_tree_free(&istate->cache_tree)` (cache-tree.c:904) — whatever was
        // there is replaced wholesale, never merged into.
        self.tree = None;
        if self.is_sparse() {
            return Ok(());
        }

        let mut tree = Tree {
            name: Default::default(),
            id: root.to_owned(),
            num_entries: None,
            children: Vec::new(),
        };
        prime_one(&mut tree, root, objects)?;
        self.tree = Some(tree);
        Ok(())
    }
}

/// Port of `prime_cache_tree_rec()` (cache-tree.c:841-895), minus the sparse-index
/// branch its caller has already ruled out.
fn prime_one(it: &mut Tree, id: &gix_hash::oid, objects: &impl gix_object::Find) -> Result<u32, Error> {
    it.id = id.to_owned();
    let mut buf = Vec::new();
    let entries: TreeRefIter<'_> = objects.find_tree_iter(id, &mut buf)?;

    let mut count = 0_u32;
    let mut children: Vec<Tree> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| Error::Decode { id: id.to_owned() })?;
        if !entry.mode.is_tree() {
            // Blobs, symlinks and gitlinks all count as one entry each.
            count += 1;
            continue;
        }
        let mut child = Tree {
            name: entry.filename.as_bytes().into(),
            id: entry.oid.to_owned(),
            num_entries: None,
            children: Vec::new(),
        };
        count += prime_one(&mut child, entry.oid, objects)?;
        children.push(child);
    }

    // Tree entries are already in git's tree order, which is not the order the
    // extension stores subtrees in (`subtree_name_cmp` sorts by length first,
    // cache-tree.c:49-57).
    children.sort_by(|a, b| super::subtree_name_cmp(&a.name, &b.name));
    it.children = children;
    it.num_entries = Some(count);
    Ok(count)
}
