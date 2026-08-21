//! Recomputing the `TREE` (cache-tree) extension from the index entries — the port of
//! `cache-tree.c`'s `cache_tree_update()` / `update_one()` / `verify_cache()`.
//!
//! ### Why this exists
//!
//! The `TREE` extension caches, per directory, the object id of the tree that directory's
//! index entries hash to, plus the number of index entries it covers. `git write-tree` and
//! `git commit` consult it so an unchanged subdirectory is never re-serialised
//! (`update_one()` returns early on a node whose `entry_count` is non-negative and whose
//! object is present, cache-tree.c:336-339), and `git status` uses the same node ids to
//! skip whole directories when diffing the index against `HEAD`.
//!
//! Upstream `gix-index` only ever *decoded* and *re-emitted* the extension verbatim, so the
//! only safe thing a mutating caller could do was throw it away
//! ([`State::remove_tree()`](crate::State::remove_tree)). That is correct but it makes every
//! index this crate writes strictly worse than the one git would have written: git has to
//! rebuild the whole tree on its next `write-tree`, and in a repository shared with stock git
//! the extension flip-flops in and out of existence.
//!
//! This module supplies the missing half. Together with
//! [`State::invalidate_path_in_tree()`](crate::State::invalidate_path_in_tree) — the port of
//! `do_invalidate_path()` — it makes the round trip
//! *decode → mutate entries → invalidate the touched paths → recompute → write* behave exactly
//! like git's, which is what lets stock git read back what we wrote and agree with it.
//!
//! ### The safety property that matters
//!
//! A node is marked valid (`num_entries = Some(_)`) **only after** the tree object it names has
//! been handed to [`Odb::write_tree()`](update::Odb::write_tree). A node that could not be proven correct stays
//! `None` ("invalid", git's `entry_count = -1`), which costs a recomputation and nothing else.
//! Being over-eager about invalidation is merely slow; being under-eager writes a cache-tree
//! that makes a later `write-tree` emit the *wrong* tree, so every decision here errs towards
//! invalidating.

use bstr::{BString, ByteSlice};
use gix_hash::ObjectId;

use crate::{Entry, PathStorageRef, State, entry, extension::Tree};

/// The object database `cache_tree_update()` writes the recomputed trees into, and asks about
/// the existence of the objects an index entry names.
///
/// `cache-tree.c` reaches into `the_repository->objects` directly; this crate has no repository,
/// so the two operations it actually performs are named here and supplied by the caller.
pub trait Odb {
    /// Return `true` if `id` is present in the object database.
    ///
    /// Mirrors `odb_has_object()` as called from `update_one()` (cache-tree.c:337, :445). A
    /// storage error must be reported as "absent": the only consequence is a recomputation,
    /// whereas claiming presence for an object that is missing would mint a cache-tree entry
    /// pointing at nothing.
    fn has_object(&self, id: &gix_hash::oid) -> bool;

    /// Write `tree` — the raw, already-serialised body of a tree object — and return its id.
    ///
    /// Mirrors `odb_write_object_ext(..., OBJ_TREE, ...)` (cache-tree.c:501).
    fn write_tree(&self, tree: &[u8]) -> Result<ObjectId, Box<dyn std::error::Error + Send + Sync + 'static>>;
}

/// The subset of git's `WRITE_TREE_*` flags (cache-tree.h:39-43) that affects tree building.
///
/// `WRITE_TREE_IGNORE_CACHE_TREE` is not represented because it is handled one level up, by
/// dropping the extension before calling ([`State::remove_tree()`](crate::State::remove_tree)),
/// exactly as `write_index_as_tree_internal()` does (cache-tree.c:751-754).
/// `WRITE_TREE_SILENT` only governs which of git's diagnostics reach stderr, and this crate
/// prints nothing at all — the caller renders [`Error`] however its verb needs to.
#[derive(Debug, Default, Copy, Clone)]
pub struct Options {
    /// `WRITE_TREE_MISSING_OK`: do not require the object an index entry names to be present
    /// in the odb (cache-tree.c:308, :441-446). This is `git write-tree --missing-ok`.
    pub missing_ok: bool,
    /// `WRITE_TREE_REPAIR`: never write a tree object. Hash what this level would serialise
    /// and accept the id only if that object is *already* in the odb, marking the node invalid
    /// otherwise (cache-tree.c:490-497).
    ///
    /// This is the mode `unpack_trees()` uses after a merge: it refreshes the cache-tree for
    /// free wherever the resulting directory happens to be one the repository already has,
    /// and admits ignorance everywhere else. A node it validates is exactly as trustworthy as
    /// one written normally — the id was derived from the entries and then proven present.
    pub repair: bool,
}

/// The error returned by [`State::cache_tree_update()`].
///
/// The variants carry the data git's `error()`/`fprintf()` calls interpolate rather than
/// pre-rendered text, so a caller can reproduce git's per-verb wording byte for byte.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `verify_cache()`'s first pass: the index still has conflicted entries
    /// (cache-tree.c:218-234). Every unmerged entry is reported, in index order; git prints
    /// at most ten of them followed by `...`, which is the caller's decision to make.
    #[error("the index has {} unmerged entries", .0.len())]
    Unmerged(Vec<(BString, ObjectId)>),
    /// `verify_cache()`'s second pass: the index holds both `path` and `path/...`, which cannot
    /// be represented as a tree (cache-tree.c:236-257).
    #[error("the index has {} directory/file conflicts", .0.len())]
    DirectoryFileConflict(Vec<(BString, BString)>),
    /// `update_one()`'s presence check failed: the entry's id is null, or the object is absent
    /// and `missing_ok` was not given (cache-tree.c:443-451). `mode` is the raw index mode, to
    /// be rendered as git does with `%06o`.
    #[error("invalid object {mode:06o} {id} for '{path}'")]
    InvalidObject {
        /// The index entry's mode, or `0o040000` when the offending entry is a subtree.
        mode: u32,
        /// The object id that could not be found.
        id: ObjectId,
        /// The full path of the entry, from the root of the index.
        path: BString,
    },
    /// A subtree that reported itself invalid (because it contains intent-to-add entries) also
    /// has no usable object id — `update_one()`'s silent `return -1` under `expected_missing`
    /// (cache-tree.c:448-449). git prints nothing for this and simply fails the update.
    #[error("a subtree containing intent-to-add entries has no tree object")]
    IntentToAddSubtree,
    /// `die("index cache-tree records empty sub-tree")` (cache-tree.c:387): a subtree consumed
    /// zero index entries, which would loop forever.
    #[error("index cache-tree records empty sub-tree")]
    EmptySubtree,
    /// The object database refused a tree object (cache-tree.c:501-504).
    #[error("could not write tree object")]
    WriteTree(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl State {
    /// Return `true` if the tree-cache is present and every node in it is valid *and* backed by
    /// an object that `odb` still has.
    ///
    /// Port of `cache_tree_fully_valid()` (cache-tree.c:278-292). This is the test
    /// `builtin/commit.c:487` and `write_index_as_tree()` (cache-tree.c:812-814) use to decide
    /// whether any work — and therefore any index write — is needed at all.
    pub fn cache_tree_fully_valid(&self, odb: &dyn Odb) -> bool {
        fn one(tree: &Tree, odb: &dyn Odb) -> bool {
            // `it->entry_count < 0 || !odb_has_object(...)` -> not valid (cache-tree.c:283-286).
            tree.num_entries.is_some()
                && odb.has_object(&tree.id)
                && tree.children.iter().all(|child| one(child, odb))
        }
        self.tree.as_ref().is_some_and(|tree| one(tree, odb))
    }

    /// Recompute every invalid node of the tree-cache from the current entries, writing each
    /// tree object into `odb`, and return the id of the root tree.
    ///
    /// Port of `cache_tree_update()` (cache-tree.c:517-548). As in git the tree-cache is created
    /// if absent, so this doubles as "build a tree from the index" — which is exactly how
    /// `git write-tree` and `git commit` obtain the tree they record.
    ///
    /// The caller is expected to write the index afterwards: git signals that by setting
    /// `CACHE_TREE_CHANGED` in `istate->cache_changed` (cache-tree.c:546), which is what makes
    /// `write_locked_index(..., SKIP_IF_UNCHANGED)` actually write (read-cache.c:3333).
    ///
    /// # Errors
    ///
    /// On any failure the tree-cache is **removed** rather than left half-built. git keeps the
    /// partially-updated structure around, which is safe there only because every caller either
    /// dies or refrains from writing the index; dropping it is the conservative choice that
    /// cannot leave a stale node on disk no matter what the caller does next.
    pub fn cache_tree_update(&mut self, odb: &dyn Odb, opts: Options) -> Result<ObjectId, Error> {
        let _span = gix_features::trace::coarse!("gix_index::State::cache_tree_update()");

        // `i = verify_cache(istate, flags); if (i) return i;` (cache-tree.c:523-526) — the
        // unmerged and D/F checks run before a single object is written.
        verify_cache(&self.entries, &self.path_backing)?;

        let object_hash = self.object_hash;
        // `if (!istate->cache_tree) istate->cache_tree = cache_tree();` (cache-tree.c:528-529),
        // where `cache_tree()` starts out invalid (`entry_count = -1`, cache-tree.c:28).
        let mut tree = self.tree.take().unwrap_or_else(|| Tree {
            name: Default::default(),
            id: ObjectId::null(object_hash),
            num_entries: None,
            children: Vec::new(),
        });

        let mut skip = 0;
        match update_one(
            &mut tree,
            &self.entries,
            &self.path_backing,
            b"",
            &mut skip,
            object_hash,
            opts,
            odb,
        ) {
            Ok(_) => {
                let id = tree.id;
                self.tree = Some(tree);
                Ok(id)
            }
            Err(err) => Err(err),
        }
    }
}

/// Port of `verify_cache()` (cache-tree.c:213-259): the index must be fully merged, and must not
/// contain `path` and `path/file` at the same time.
///
/// Both conditions are fatal for tree building rather than merely inconvenient: a conflicted
/// entry has no place in a tree at all, and a D/F pair would have `update_one()` emit a blob and
/// a subtree under the same name.
fn verify_cache(entries: &[Entry], backing: &PathStorageRef) -> Result<(), Error> {
    let unmerged: Vec<_> = entries
        .iter()
        .filter(|e| e.stage() != entry::Stage::Unconflicted)
        .map(|e| (e.path_in(backing).to_owned(), e.id))
        .collect();
    if !unmerged.is_empty() {
        return Err(Error::Unmerged(unmerged));
    }

    // "Also verify that the cache does not have path and path/file at the same time. At this
    // point we know the cache has only stage 0 entries." (cache-tree.c:236-239).
    //
    // git walks adjacent pairs and falls back to `index_name_pos_sparse(istate, "path/")` when
    // an unrelated entry sorts between them ("path-internal" sits between "path" and
    // "path/file" because '-' precedes '/', cache-tree.c:170-175). Since entries are sorted by
    // raw path bytes, probing for `path/` directly finds the same conflicts with none of the
    // adjacency special-casing.
    let mut conflicts = Vec::new();
    let mut probe: Vec<u8> = Vec::new();
    for entry in entries {
        let name = entry.path_in(backing);
        probe.clear();
        probe.extend_from_slice(name);
        probe.push(b'/');
        let pos = entries.partition_point(|other| other.path_in(backing).as_bytes() < probe.as_slice());
        if let Some(other) = entries.get(pos) {
            let other = other.path_in(backing);
            if other.starts_with(&probe) {
                conflicts.push((name.to_owned(), other.to_owned()));
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(Error::DirectoryFileConflict(conflicts));
    }
    Ok(())
}

/// Port of `update_one()` (cache-tree.c:299-515), returning the number of index entries this
/// level consumed — the value the caller adds to its cursor.
///
/// `cache` is the index tail starting at this level's first entry, and `base` is the full path
/// prefix every entry of this level shares, including its trailing `/` (git passes a pointer
/// into an entry's name plus its length, cache-tree.c:378-382).
#[allow(clippy::too_many_arguments)]
fn update_one(
    it: &mut Tree,
    cache: &[Entry],
    backing: &PathStorageRef,
    base: &[u8],
    skip_count: &mut usize,
    object_hash: gix_hash::Kind,
    opts: Options,
    odb: &dyn Odb,
) -> Result<usize, Error> {
    let baselen = base.len();
    *skip_count = 0;

    // "If the first entry of this region is a sparse directory entry corresponding exactly to
    // 'base', then this cache_tree struct is a 'leaf' [...] pointing to the tree OID specified
    // in the entry." (cache-tree.c:318-334).
    if let Some(ce) = cache.first() {
        let name = ce.path_in(backing);
        if ce.mode.is_sparse() && name.len() == baselen && name.as_bytes() == base {
            it.num_entries = Some(1);
            it.id = ce.id;
            return Ok(1);
        }
    }

    // The whole point of the extension: a node that is still valid and whose tree is really in
    // the odb is reused as-is, and its entire subtree is skipped (cache-tree.c:336-339).
    if let Some(count) = it.num_entries {
        if odb.has_object(&it.id) {
            return Ok(count as usize);
        }
    }

    // Pass one: find the subdirectories of this level and update them, in index order.
    //
    // git marks the existing `down[]` entries unused, re-finds (or creates) each one while
    // walking, and then drops the unmarked ones (`discard_unused_subtrees()`, cache-tree.c:394).
    // Moving the old children out and re-collecting only the ones the walk actually visits
    // achieves the same thing without the `used` flag.
    let mut previous = std::mem::take(&mut it.children);
    let mut children: Vec<(Tree, usize)> = Vec::new();
    let mut i = 0;
    while i < cache.len() {
        let path = cache[i].path_in(backing);
        // `if (pathlen <= baselen || memcmp(base, path, baselen)) break;` — end of this level.
        if path.len() <= baselen || path[..baselen] != *base {
            break;
        }
        let Some(sublen) = path[baselen..].find_byte(b'/') else {
            i += 1;
            continue;
        };
        let name: &[u8] = &path[baselen..baselen + sublen];
        let mut sub = match previous.iter().position(|c| c.name.as_slice() == name) {
            Some(pos) => previous.remove(pos),
            None => Tree {
                name: name.into(),
                id: ObjectId::null(object_hash),
                num_entries: None,
                children: Vec::new(),
            },
        };
        let mut sub_skip = 0;
        let consumed = update_one(
            &mut sub,
            &cache[i..],
            backing,
            &path[..baselen + sublen + 1],
            &mut sub_skip,
            object_hash,
            opts,
            odb,
        )?;
        // `if (!subcnt) die("index cache-tree records empty sub-tree");` (cache-tree.c:386-387).
        if consumed == 0 {
            return Err(Error::EmptySubtree);
        }
        i += consumed;
        *skip_count += sub_skip;
        children.push((sub, consumed));
    }

    // Pass two: serialise this level's tree object.
    //
    // Both passes walk the entries identically — non-directory entries advance by one, a
    // subdirectory by the count pass one recorded — so the n-th subtree met here is the n-th
    // subtree collected there, and `sub_idx` stands in for git's `find_subtree()` lookup.
    let mut buffer: Vec<u8> = Vec::new();
    let mut to_invalidate = false;
    let mut sub_idx = 0;
    i = 0;
    while i < cache.len() {
        let ce = &cache[i];
        let path = ce.path_in(backing);
        if path.len() <= baselen || path[..baselen] != *base {
            break;
        }

        let (id, mode, entlen, is_subtree, contains_ita) = match path[baselen..].find_byte(b'/') {
            Some(sublen) => {
                let (sub, consumed) = &children[sub_idx];
                sub_idx += 1;
                i += consumed;
                // "contains_ita = sub->cache_tree->entry_count < 0" — an intent-to-add entry
                // somewhere below poisons every node above it (cache-tree.c:428-432).
                let contains_ita = sub.num_entries.is_none();
                if contains_ita {
                    to_invalidate = true;
                }
                (sub.id, 0o040_000_u32, sublen, true, contains_ita)
            }
            None => {
                let mode = ce.mode.bits();
                i += 1;
                (ce.id, mode, path.len() - baselen, false, false)
            }
        };

        // `ce_missing_ok = mode == S_IFGITLINK || missing_ok || !must_check_existence(ce)`
        // (cache-tree.c:441-442). A gitlink's commit lives in the submodule's own odb, so its
        // absence here is expected. Promisor remotes — the third disjunct — are not modelled.
        let ce_missing_ok = mode == 0o160_000 || opts.missing_ok;
        if id.is_null() || (!ce_missing_ok && !odb.has_object(&id)) {
            // `if (expected_missing) return -1;` — the silent failure for a subtree that only
            // holds intent-to-add entries (cache-tree.c:448-449).
            return Err(if contains_ita {
                Error::IntentToAddSubtree
            } else {
                Error::InvalidObject {
                    mode,
                    id,
                    path: path[..baselen + entlen].as_bstr().to_owned(),
                }
            });
        }

        // "CE_REMOVE entries are removed before the index is written to disk. Skip them to
        // remain consistent with the future on-disk index." (cache-tree.c:454-462). Note that
        // git tests this on `cache[i]` *before* the cursor moved, i.e. on the first entry of a
        // subtree when this iteration handled one; that quirk is reproduced deliberately.
        if ce.flags.contains(entry::Flags::REMOVE) {
            *skip_count += 1;
            continue;
        }

        // "CE_INTENT_TO_ADD entries exist in on-disk index but they are not part of generated
        // trees. Invalidate up to root to force cache-tree users to read elsewhere."
        // (cache-tree.c:464-472).
        if !is_subtree && ce.flags.contains(entry::Flags::INTENT_TO_ADD) {
            to_invalidate = true;
            continue;
        }

        // "'sub' can be an empty tree if all subentries are i-t-a." (cache-tree.c:474-478).
        if contains_ita && id == ObjectId::empty_tree(object_hash) {
            continue;
        }

        // `strbuf_addf(&buffer, "%o %.*s%c", mode, entlen, path + baselen, '\0')` followed by
        // the raw hash (cache-tree.c:480-482) — the canonical tree object body. Index order is
        // already tree order: a directory's entries all start with `<name>/`, and `/` (0x2F)
        // sorts exactly where git's tree comparison puts the implied trailing slash.
        write_octal(&mut buffer, mode);
        buffer.push(b' ');
        buffer.extend_from_slice(&path[baselen..baselen + entlen]);
        buffer.push(0);
        buffer.extend_from_slice(id.as_bytes());
    }

    if opts.repair {
        // `hash_object_file(...); if (odb_has_object(&oid)) oidcpy(&it->oid, &oid); else
        // to_invalidate = 1;` (cache-tree.c:490-497). The node keeps whatever id it had when
        // the object is absent, which does not matter: `to_invalidate` about to make it
        // invalid, and an invalid node's id is never written out (cache-tree.c:575-577).
        let id = gix_object::compute_hash(object_hash, gix_object::Kind::Tree, &buffer)
            .map_err(|err| Error::WriteTree(Box::new(err)))?;
        if odb.has_object(&id) {
            it.id = id;
        } else {
            to_invalidate = true;
        }
    } else {
        it.id = odb.write_tree(&buffer).map_err(Error::WriteTree)?;
    }
    // `it->entry_count = to_invalidate ? -1 : i - *skip_count;` (cache-tree.c:508).
    it.num_entries = if to_invalidate {
        None
    } else {
        Some(u32::try_from(i - *skip_count).unwrap_or(u32::MAX))
    };
    // git keeps `down[]` sorted by `subtree_name_cmp` at all times (`find_subtree()` inserts at
    // the binary-searched position, cache-tree.c:92-104) and `write_one()` dies on an unsorted
    // array (cache-tree.c:580-584). The walk above collected them in index order instead, so
    // they are sorted once here.
    children.sort_by(|a, b| super::subtree_name_cmp(&a.0.name, &b.0.name));
    it.children = children.into_iter().map(|(tree, _)| tree).collect();
    Ok(i)
}

/// Append `mode` in octal, the way `strbuf_addf(..., "%o", mode)` renders it — no leading zero,
/// so `0o100644` becomes `100644` and a tree's `0o40000` becomes `40000`.
fn write_octal(out: &mut Vec<u8>, mode: u32) {
    let mut digits = [0u8; 12];
    let mut n = digits.len();
    let mut value = mode;
    loop {
        n -= 1;
        digits[n] = b'0' + u8::try_from(value & 0o7).expect("three bits");
        value >>= 3;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[n..]);
}

