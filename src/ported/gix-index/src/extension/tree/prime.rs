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

use bstr::{BString, ByteSlice, ByteVec};
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
    /// A **sparse** index primes too. There the cache-tree has "leaf" nodes standing in
    /// for whole directories that the index never expands, and git tells the two apart
    /// with one `index_entry_exists()` probe per directory: a directory that appears in
    /// the index *as an entry* (a sparse directory, stored with its trailing `/`) becomes
    /// a leaf, and any other directory is walked as usual (cache-tree.c:872-891). The
    /// walk therefore needs the path it has reached, which is why git builds `tree_path`
    /// as it descends — and only bothers to when the index is sparse
    /// (cache-tree.c:866-871).
    pub fn prime_cache_tree(&mut self, objects: &impl gix_object::Find, root: &gix_hash::oid) -> Result<(), Error> {
        let _span = gix_features::trace::coarse!("gix_index::State::prime_cache_tree()");
        // `cache_tree_free(&istate->cache_tree)` (cache-tree.c:904) — whatever was
        // there is replaced wholesale, never merged into.
        self.tree = None;

        let mut tree = Tree {
            name: Default::default(),
            id: root.to_owned(),
            num_entries: None,
            children: Vec::new(),
        };
        // `struct strbuf tree_path = STRBUF_INIT;` (cache-tree.c:901) — the root's path
        // is empty, and it stays unused entirely unless this index is sparse.
        let mut tree_path = BString::default();
        let sparse = self.is_sparse().then_some(&*self);
        prime_one(&mut tree, root, objects, sparse, &mut tree_path)?;
        self.tree = Some(tree);
        Ok(())
    }
}

/// Port of `prime_cache_tree_rec()` (cache-tree.c:841-895).
///
/// `sparse` is `Some(state)` exactly when `r->index->sparse_index` is true in git, and is what
/// the `index_entry_exists()` probe runs against; `tree_path` is the directory path reached so
/// far, with a trailing `/`, maintained only in that case.
fn prime_one(
    it: &mut Tree,
    id: &gix_hash::oid,
    objects: &impl gix_object::Find,
    sparse: Option<&State>,
    tree_path: &mut BString,
) -> Result<u32, Error> {
    it.id = id.to_owned();
    let mut buf = Vec::new();
    let entries: TreeRefIter<'_> = objects.find_tree_iter(id, &mut buf)?;

    let base_path_len = tree_path.len();
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
        // "Recursively-constructed subtree path is only needed when working in a sparse
        // index (where it's used to determine whether the subtree is a sparse directory
        // in the index)." (cache-tree.c:866-871) — `strbuf_setlen()` back to this level
        // before each sibling, then `<name>/`.
        if sparse.is_some() {
            tree_path.truncate(base_path_len);
            tree_path.push_str(entry.filename);
            tree_path.push(b'/');
        }
        // "If a sparse index is in use, the directory being processed may be sparse. To
        // confirm that, we can check whether an entry with that exact name exists in the
        // index. If it does, the created subtree should be sparse." (cache-tree.c:873-889).
        //
        // git parses the subtree object before this probe and dies if it is missing
        // (cache-tree.c:861-864); a leaf here never looks inside the tree, so this reads
        // it only when it descends. The node's id comes from the parent tree entry either
        // way, so the only difference is which of the two reports a repository that has
        // lost that object.
        let is_sparse_dir = sparse.is_some_and(|state| state.entry_index_by_path(tree_path.as_bstr()).is_ok());
        if is_sparse_dir {
            // `prime_cache_tree_sparse_dir()` (cache-tree.c:830-839): the whole directory
            // is one index entry, so the node is valid, covers exactly one entry, and has
            // no children of its own.
            child.num_entries = Some(1);
            count += 1;
        } else {
            count += prime_one(&mut child, entry.oid, objects, sparse, tree_path)?;
        }
        children.push(child);
    }
    if sparse.is_some() {
        tree_path.truncate(base_path_len);
    }

    // Tree entries are already in git's tree order, which is not the order the
    // extension stores subtrees in (`subtree_name_cmp` sorts by length first,
    // cache-tree.c:49-57).
    children.sort_by(|a, b| super::subtree_name_cmp(&a.name, &b.name));
    it.children = children;
    it.num_entries = Some(count);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bstr::BStr;
    use gix_hash::ObjectId;

    use crate::{State, entry};

    /// An object database holding exactly the tree bodies a test puts in it, keyed by whatever
    /// id the test chose — priming never hashes anything, so the ids need not be real.
    struct Trees(HashMap<ObjectId, Vec<u8>>);

    impl gix_object::Find for Trees {
        fn try_find<'a>(
            &self,
            id: &gix_hash::oid,
            buffer: &'a mut Vec<u8>,
        ) -> Result<Option<gix_object::Data<'a>>, gix_object::find::Error> {
            match self.0.get(id) {
                Some(body) => {
                    buffer.clear();
                    buffer.extend_from_slice(body);
                    Ok(Some(gix_object::Data::new(
                        buffer,
                        gix_object::Kind::Tree,
                        gix_hash::Kind::Sha1,
                    )))
                }
                None => Ok(None),
            }
        }
    }

    fn oid(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    /// `<mode> <name>\0<20 raw hash bytes>`, the canonical tree entry.
    fn tree_entry(out: &mut Vec<u8>, mode: &str, name: &str, id: ObjectId) {
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(id.as_bytes());
    }

    /// A root tree with two subdirectories, each holding one blob.
    const ROOT: u8 = 1;
    const DENSE: u8 = 2;
    const SPARSE: u8 = 3;

    fn odb() -> Trees {
        let mut root = Vec::new();
        tree_entry(&mut root, "40000", "dense", oid(DENSE));
        tree_entry(&mut root, "40000", "sparse", oid(SPARSE));
        let mut dense = Vec::new();
        tree_entry(&mut dense, "100644", "f", oid(10));
        let mut sparse = Vec::new();
        tree_entry(&mut sparse, "100644", "g", oid(11));
        Trees(HashMap::from([
            (oid(ROOT), root),
            (oid(DENSE), dense),
            (oid(SPARSE), sparse),
        ]))
    }

    /// An index over `dense/f`, plus `sparse/` either expanded to `sparse/g` or collapsed into
    /// one sparse-directory entry.
    fn index(collapsed: bool) -> State {
        let mut state = State::new(gix_hash::Kind::Sha1);
        state.dangerously_push_entry(
            entry::Stat::default(),
            oid(10),
            entry::Flags::empty(),
            entry::Mode::FILE,
            BStr::new("dense/f"),
        );
        if collapsed {
            state.dangerously_push_entry(
                entry::Stat::default(),
                oid(SPARSE),
                entry::Flags::empty(),
                entry::Mode::DIR,
                BStr::new("sparse/"),
            );
        } else {
            state.dangerously_push_entry(
                entry::Stat::default(),
                oid(11),
                entry::Flags::empty(),
                entry::Mode::FILE,
                BStr::new("sparse/g"),
            );
        }
        state.sort_entries();
        state
    }

    /// The probe only fires on a sparse index, and only for the directory that is *itself* an
    /// index entry: `sparse/` becomes a leaf naming its tree and covering its one entry
    /// (`prime_cache_tree_sparse_dir()`, cache-tree.c:830-839), while `dense/` is walked as
    /// usual even though it sits in the same index.
    #[test]
    fn a_sparse_directory_becomes_a_leaf_node() {
        let mut state = index(true);
        state.is_sparse = true;
        state.prime_cache_tree(&odb(), &oid(ROOT)).expect("all trees present");

        let tree = state.tree().expect("primed");
        assert_eq!(tree.num_entries, Some(2), "one entry below `dense/`, one *for* `sparse/`");
        let dense = &tree.children[0];
        assert_eq!(dense.name.as_slice(), b"dense");
        assert_eq!(dense.num_entries, Some(1));
        assert_eq!(dense.id, oid(DENSE));
        let sparse = &tree.children[1];
        assert_eq!(sparse.name.as_slice(), b"sparse");
        assert_eq!(
            sparse.num_entries,
            Some(1),
            "a sparse directory is one index entry no matter how much is under it"
        );
        assert_eq!(sparse.id, oid(SPARSE));
        assert!(
            sparse.children.is_empty(),
            "a leaf node has no children — nothing below it is in the index to describe"
        );
    }

    /// The same trees over a *non*-sparse index: `tree_path` is never built, no probe runs, and
    /// both directories are expanded from their tree objects.
    #[test]
    fn a_dense_index_expands_every_directory() {
        let mut state = index(false);
        assert!(!state.is_sparse(), "no directory entry, so nothing marks this sparse");
        state.prime_cache_tree(&odb(), &oid(ROOT)).expect("all trees present");

        let tree = state.tree().expect("primed");
        assert_eq!(tree.num_entries, Some(2), "one blob below each directory");
        for child in &tree.children {
            assert_eq!(child.num_entries, Some(1));
            assert!(child.children.is_empty(), "neither subdirectory has one of its own");
        }
    }

    /// A directory that merely *starts* with the probed path must not be mistaken for a sparse
    /// directory: the probe is an exact match on `<dir>/`, so `sparse-other/x` — which sorts
    /// between `sparse-` and `sparse/` — leaves `sparse/` expanded.
    #[test]
    fn the_probe_is_an_exact_match_not_a_prefix() {
        let mut state = index(false);
        state.dangerously_push_entry(
            entry::Stat::default(),
            oid(12),
            entry::Flags::empty(),
            entry::Mode::FILE,
            BStr::new("sparse-other"),
        );
        state.sort_entries();
        state.is_sparse = true;
        state.prime_cache_tree(&odb(), &oid(ROOT)).expect("all trees present");

        let tree = state.tree().expect("primed");
        let sparse = tree.children.iter().find(|c| c.name.as_slice() == b"sparse").expect("node for sparse");
        assert_eq!(sparse.num_entries, Some(1));
        assert!(
            sparse.children.is_empty(),
            "`sparse/` holds one blob, so expanding it yields a count but no children"
        );
        assert_eq!(
            tree.num_entries,
            Some(2),
            "priming counts what the *tree* holds; `sparse-other` is not in it"
        );
    }
}
