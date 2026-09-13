//! ## About `debug_assert!()
//!
//! The idea is to have code that won't panic in production. Thus, if in production that assertion would fail,
//! we will rather let the code run and hope it will either be correct enough or fail in more graceful ways later.
//!
//! Once such a case becomes a bug and is reproduced in testing, the debug-assertion will kick in and hopefully
//! contribute to finding a fix faster.
use std::collections::HashMap;

use bstr::{BStr, BString, ByteSlice, ByteVec};
use gix_diff::tree_with_rewrites::{Change, ChangeRef};
use gix_hash::ObjectId;
use gix_object::{
    tree,
    tree::{EntryKind, EntryMode},
};

use crate::{
    blob::{ResourceKind, builtin_driver::binary::Pick},
    tree::{
        Conflict, ConflictIndexEntry, ConflictIndexEntryPathHint, ConflictMapping, Error, Options, Resolution,
        ResolutionFailure,
    },
};

/// Produce a unique path within the directory that contains the file at `file_path` like `a/b`, using `editor`
/// and `tree` to assure unique names, to obtain the tree at `a/` and `side_name` to more clearly signal
/// where the file is coming from.
pub fn unique_path_in_tree(
    file_path: &BStr,
    editor: &tree::Editor<'_>,
    tree: &TreeNodes,
    side_name: &BStr,
) -> Result<BString, Error> {
    let mut buf = file_path.to_owned();
    buf.push(b'~');
    buf.extend(
        side_name
            .as_bytes()
            .iter()
            .copied()
            .map(|b| if b == b'/' { b'_' } else { b }),
    );

    // We could use a cursor here, but clashes are so unlikely that this wouldn't be meaningful for performance.
    let base_len = buf.len();
    let mut suffix = 0;
    // `unique_path()` (merge-ort.c:930-935) only skips names that exist *exactly*:
    // `strmap_contains(existing_paths, newpath.buf)`. A lookup that merely passes a
    // changed directory on the way is not an occupied name, and counting it as one
    // never terminates once that directory is the candidate's parent.
    let occupied = |path: &BStr| {
        matches!(
            tree.check_conflict(path),
            Some(PossibleConflict::Match { .. } | PossibleConflict::TreeToNonTree { .. })
        )
    };
    while editor.get(to_components_bstring_ref(&buf)).is_some() || occupied(buf.as_bstr()) {
        buf.truncate(base_len);
        buf.push_str(format!("_{suffix}"));
        suffix += 1;
    }
    Ok(buf)
}

/// Perform a merge between two blobs and return the result of its object id.
#[expect(clippy::too_many_arguments)]
pub fn perform_blob_merge<E>(
    mut labels: crate::blob::builtin_driver::text::Labels<'_>,
    objects: &impl gix_object::FindObjectOrHeader,
    blob_merge: &mut crate::blob::Platform,
    buf: &mut Vec<u8>,
    write_blob_to_odb: &mut impl FnMut(&[u8]) -> Result<ObjectId, E>,
    (our_location, our_id, our_mode): (&BString, ObjectId, EntryMode),
    (their_location, their_id, their_mode): (&BString, ObjectId, EntryMode),
    (previous_location, previous_id, previous_mode): (&BString, ObjectId, EntryMode),
    (extra_markers, outer_side): (u8, ConflictMapping),
    options: &Options,
) -> Result<(ObjectId, crate::blob::Resolution), Error>
where
    E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    if our_id == their_id {
        // Both sides arrived at the same content, so there is nothing to merge and the
        // content itself is the result. The modes may still differ — but they need not:
        // the caller dispatches on the *kind* of change each side made, not on what it
        // produced, so a rename on one side and a modification on the other reach here
        // whenever the two happen to land on the same blob. That is what a cherry-pick
        // of a commit whose change the branch already carries looks like.
        return Ok((their_id, crate::blob::Resolution::Complete));
    }
    if matches!(our_mode.kind(), EntryKind::Link) && matches!(their_mode.kind(), EntryKind::Link) {
        let (pick, resolution) = crate::blob::builtin_driver::binary(options.symlink_conflicts);
        let (our_id, their_id) = match outer_side {
            ConflictMapping::Original => (our_id, their_id),
            ConflictMapping::Swapped => (their_id, our_id),
        };
        let id = match pick {
            Pick::Ancestor => previous_id,
            Pick::Ours => our_id,
            Pick::Theirs => their_id,
        };
        return Ok((id, resolution));
    }
    let (our_kind, their_kind) = match outer_side {
        ConflictMapping::Original => (ResourceKind::CurrentOrOurs, ResourceKind::OtherOrTheirs),
        ConflictMapping::Swapped => (ResourceKind::OtherOrTheirs, ResourceKind::CurrentOrOurs),
    };
    blob_merge.set_resource(our_id, our_mode.kind(), our_location.as_bstr(), our_kind, objects)?;
    blob_merge.set_resource(
        their_id,
        their_mode.kind(),
        their_location.as_bstr(),
        their_kind,
        objects,
    )?;
    blob_merge.set_resource(
        previous_id,
        previous_mode.kind(),
        previous_location.as_bstr(),
        ResourceKind::CommonAncestorOrBase,
        objects,
    )?;

    fn combined(side: &BStr, location: &BString) -> BString {
        let mut buf = side.to_owned();
        buf.push_byte(b':');
        buf.push_str(location);
        buf
    }

    // The labels follow the ids, which are resourced by `outer_side` above, and so
    // do the paths paired with them. The paths only differ when a rename or
    // directory rename is involved: t6423 11e with `B A` wrote `B:y/c` against
    // `A:z/c` where merge-ort writes `B:z/c` against `A:y/c` (merge-ort.c:2137-2144).
    let (our_location, their_location) = if outer_side.is_swapped() {
        (labels.current, labels.other) = (labels.other, labels.current);
        (their_location, our_location)
    } else {
        (our_location, their_location)
    };

    let (ancestor, current, other);
    let labels = if our_location == their_location {
        labels
    } else {
        ancestor = labels.ancestor.map(|side| combined(side, previous_location));
        current = labels.current.map(|side| combined(side, our_location));
        other = labels.other.map(|side| combined(side, their_location));
        crate::blob::builtin_driver::text::Labels {
            ancestor: ancestor.as_ref().map(|n| n.as_bstr()),
            current: current.as_ref().map(|n| n.as_bstr()),
            other: other.as_ref().map(|n| n.as_bstr()),
        }
    };
    let mut prep = blob_merge.prepare_merge(objects, options.blob_merge)?;
    add_extra_marker_size(
        &mut prep,
        extra_markers.saturating_add(options.marker_size_multiplier.saturating_mul(2)),
    );
    let (pick, resolution) = prep.merge(buf, labels, &options.blob_merge_command_ctx)?;

    let merged_blob_id = prep
        .id_by_pick(pick, buf, write_blob_to_odb)
        .map_err(|err| Error::WriteBlobToOdb(err.into()))?
        .ok_or(Error::MergeResourceNotFound)?;
    Ok((merged_blob_id, resolution))
}

/// `ll_merge()`'s last move before it dispatches to the driver
/// (merge-ll.c:445-447, git v2.55.0):
///
/// ```c
/// if (opts->extra_marker_size) {
///     marker_size += opts->extra_marker_size;
/// }
/// ```
///
/// It lands *after* the `conflict-marker-size` attribute has replaced the
/// default (merge-ll.c:431-438) rather than before it, and that order is the
/// whole point: a path that asks for thirteen markers is written with fifteen
/// of them one recursion level down. Widening the default before
/// [`Platform::prepare_merge()`](crate::blob::Platform::prepare_merge) instead
/// let the attribute overwrite the widened value, so a virtual merge base under
/// `conflict-marker-size=13` came out with thirteen-character markers where
/// stock git writes fifteen.
fn add_extra_marker_size(prep: &mut crate::blob::PlatformRef<'_>, extra: u8) {
    if let crate::blob::builtin_driver::text::Conflict::Keep { marker_size, .. } = &mut prep.options.text.conflict {
        *marker_size = marker_size.saturating_add(extra);
    }
}

/// A way to attach metadata to each change.
#[derive(Debug)]
pub struct TrackedChange {
    /// The actual change
    pub inner: Change,
    /// If `true`, this change counts as written to the tree using a [`tree::Editor`].
    pub was_written: bool,
    /// If `Some(ours_idx_to_ignore)`, this change must be placed into the tree before handling it.
    /// This makes sure that new changes aren't visible too early, which would mean the algorithm
    /// knows things too early which can be misleading.
    /// The `ours_idx_to_ignore` assures that the same rewrite won't be used as matching side, which
    /// would lead to strange effects. Only set if it's a rewrite though.
    pub needs_tree_insertion: Option<Option<usize>>,
    /// The location this change had before a directory rename moved it, if one did.
    /// merge-ort moves the path's `conflict_info` but leaves `pathnames[side]` alone
    /// (merge-ort.c:2826-2845), so content merges still label this side with it.
    pub location_before_directory_rename: Option<BString>,
    /// The base version a rename brought along to this change's path. merge-ort copies the
    /// old path's stage 1 into the destination's `conflict_info` (`process_renames()`,
    /// merge-ort.c:3192-3195), and that stage stays with the file wherever
    /// `process_entry()` moves it (merge-ort.c:4148-4157).
    pub carried_base: Option<ConflictIndexEntry>,
}

impl TrackedChange {
    /// The path merge-ort's `pathnames[side]` holds for this change: where its side
    /// put it, before any directory rename moved it.
    pub fn label_location(&self) -> &BString {
        self.location_before_directory_rename
            .as_ref()
            .unwrap_or_else(|| match &self.inner {
                Change::Addition { location, .. }
                | Change::Deletion { location, .. }
                | Change::Modification { location, .. }
                | Change::Rewrite { location, .. } => location,
            })
    }
}

pub type ChangeList = Vec<TrackedChange>;
pub type ChangeListRef = [TrackedChange];

/// Only keep leaf nodes, or trees that are the renamed, pushing `change` on `changes`.
/// Doing so makes it easy to track renamed or rewritten or copied directories, and properly
/// handle *their* changes that fall within them.
/// Note that it also rewrites `change` if it is a copy, turning it into an addition so copies don't have an effect
/// on the merge algorithm.
pub fn track(change: ChangeRef<'_>, changes: &mut ChangeList) {
    if change.entry_mode().is_tree() && matches!(change, ChangeRef::Modification { .. }) {
        return;
    }
    let is_tree = change.entry_mode().is_tree();
    changes.push(TrackedChange {
        inner: match change.into_owned() {
            Change::Rewrite {
                id,
                entry_mode,
                location,
                relation,
                copy,
                ..
            } if copy => Change::Addition {
                location,
                relation,
                entry_mode,
                id,
            },
            other => other,
        },
        was_written: is_tree,
        needs_tree_insertion: None,
        location_before_directory_rename: None,
        carried_base: None,
    });
}

/// The mode merge-ort holds for a path's stage and writes into the merged tree.
///
/// Every mode it reads from a tree has gone through `canon_mode()` (object.h:145-154,
/// applied by `decode_tree_entry()`, tree-walk.c:42), which a kind already is. The one
/// mode that never passed through it is the `0` a cleared side-1 stage holds
/// (`apply_directory_rename_modifications()`, merge-ort.c:2792-2797): it is recorded as
/// it is, and `write_tree()` writes it as `"%o"`, i.e. `0` (merge-ort.c:3857-3860).
pub fn canon_mode(mode: EntryMode) -> EntryMode {
    if mode.value() == 0 { mode } else { mode.kind().into() }
}

/// Unconditionally apply `change` to `editor`.
pub fn apply_change(
    editor: &mut tree::Editor<'_>,
    change: &Change,
    alternative_location: Option<&BString>,
) -> Result<(), tree::editor::Error> {
    use to_components_bstring_ref as to_components;
    if change.entry_mode().is_tree() {
        return Ok(());
    }

    let (location, mode, id) = match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        }
        | Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } => (location, entry_mode, id),
        Change::Deletion { location, .. } => {
            editor.remove(to_components(alternative_location.unwrap_or(location)))?;
            return Ok(());
        }
        Change::Rewrite {
            source_location,
            entry_mode,
            id,
            location,
            copy,
            ..
        } => {
            if !*copy {
                editor.remove(to_components(source_location))?;
            }
            (location, entry_mode, id)
        }
    };

    editor.upsert_mode(
        to_components(alternative_location.unwrap_or(location)),
        canon_mode(*mode),
        *id,
    )?;
    Ok(())
}

/// A potential conflict that needs to be checked. It comes in several varieties and always happens
/// if paths overlap in some way between *theirs* and *ours*.
#[derive(Debug)]
pub enum PossibleConflict {
    /// *our* changes have a tree here, but *they* place a non-tree or edit an existing item (that we removed).
    TreeToNonTree {
        /// The possibly available change at this node.
        change_idx: Option<usize>,
    },
    /// A non-tree in *our* tree turned into a tree in *theirs* - this can be done with additions in *theirs*,
    /// or if we added a blob, while they added a directory.
    NonTreeToTree {
        /// The possibly available change at this node.
        change_idx: Option<usize>,
    },
    /// A perfect match, i.e. *our* change at `a/b/c` corresponds to *their* change at the same path.
    Match {
        /// The index to *our* change at *their* path.
        change_idx: usize,
    },
    /// *their* change at `a/b/c` passed `a/b` which is an index to *our* change indicating a directory that was rewritten,
    /// with all its contents being renamed. However, *theirs* has been added *into* that renamed directory.
    PassedRewrittenDirectory { change_idx: usize },
}

impl PossibleConflict {
    pub(super) fn change_idx(&self) -> Option<usize> {
        match self {
            PossibleConflict::TreeToNonTree { change_idx, .. } | PossibleConflict::NonTreeToTree { change_idx, .. } => {
                *change_idx
            }
            PossibleConflict::Match { change_idx, .. }
            | PossibleConflict::PassedRewrittenDirectory { change_idx, .. } => Some(*change_idx),
        }
    }
}

/// The flat list of all tree-nodes so we can avoid having a linked-tree using pointers
/// which is useful for traversal and initial setup as that can then trivially be non-recursive.
pub struct TreeNodes(Vec<TreeNode>);

/// Trees lead to other trees, or leafs (without children), and it can be represented by a renamed directory.
#[derive(Debug, Default, Clone)]
struct TreeNode {
    /// A mapping of path components to their children to quickly see if `theirs` in some way is potentially
    /// conflicting with `ours`.
    children: HashMap<BString, usize>,
    /// The index to a change, which is always set if this is a leaf node (with no children), and if there are children and this
    /// is a rewritten tree.
    change_idx: Option<usize>,
    /// Keep track of where the location of this node is derived from.
    location: ChangeLocation,
}

#[derive(Debug, Default, Clone, Copy)]
enum ChangeLocation {
    /// The change is at its current (and only) location, or in the source location of a rename.
    #[default]
    CurrentLocation,
    /// This is always the destination of a rename.
    RenamedLocation,
}

impl TreeNode {
    fn is_leaf_node(&self) -> bool {
        self.children.is_empty()
    }
}

impl TreeNodes {
    pub fn new() -> Self {
        TreeNodes(vec![TreeNode::default()])
    }

    /// Insert our `change` at `change_idx`, into a linked-tree, assuring that each `change` is non-conflicting
    /// with this tree structure, i.e. each leaf path is only seen once.
    /// Note that directories can be added in between.
    pub fn track_change(&mut self, change: &Change, change_idx: usize) {
        for (path, location_hint) in [
            Some((change.source_location(), ChangeLocation::CurrentLocation)),
            match change {
                Change::Addition { .. } | Change::Deletion { .. } | Change::Modification { .. } => None,
                Change::Rewrite { location, .. } => Some((location.as_bstr(), ChangeLocation::RenamedLocation)),
            },
        ]
        .into_iter()
        .flatten()
        {
            let mut components = to_components(path).peekable();
            let mut next_index = self.0.len();
            let mut cursor = &mut self.0[0];
            while let Some(component) = components.next() {
                let is_last = components.peek().is_none();
                match cursor.children.get(component).copied() {
                    None => {
                        let new_node = TreeNode {
                            children: Default::default(),
                            change_idx: is_last.then_some(change_idx),
                            location: location_hint,
                        };
                        cursor.children.insert(component.to_owned(), next_index);
                        self.0.push(new_node);
                        cursor = &mut self.0[next_index];
                        next_index += 1;
                    }
                    Some(index) => {
                        cursor = &mut self.0[index];
                        if is_last && !cursor.is_leaf_node() {
                            // NOTE: we might encounter the same path multiple times in rare conditions.
                            //       At least we avoid overwriting existing intermediate changes, for good measure.
                            if cursor.change_idx.is_none() {
                                cursor.change_idx = Some(change_idx);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Search the tree with `our` changes for `theirs` by [`source_location()`](Change::source_location())).
    /// If there is an entry but both are the same, or if there is no entry, return `None`.
    pub fn check_conflict(&self, theirs_location: &BStr) -> Option<PossibleConflict> {
        if self.0.len() == 1 {
            return None;
        }
        let components = to_components(theirs_location);
        let mut cursor = &self.0[0];
        let mut cursor_idx = 0;
        let mut intermediate_change = None;
        for component in components {
            if cursor.change_idx.is_some() {
                intermediate_change = cursor.change_idx.map(|change_idx| (change_idx, cursor_idx));
            }
            match cursor.children.get(component).copied() {
                // *their* change is outside *our* tree
                None => {
                    let res = if cursor.is_leaf_node() {
                        Some(PossibleConflict::NonTreeToTree {
                            change_idx: cursor.change_idx,
                        })
                    } else {
                        // a change somewhere else, i.e. `a/c` and we know `a/b` only.
                        intermediate_change.and_then(|(change, cursor_idx)| {
                            let cursor = &self.0[cursor_idx];
                            // If this is a destination location of a rename, then the `their_location`
                            // is already at the right spot, and we can just ignore it.
                            if matches!(cursor.location, ChangeLocation::CurrentLocation) {
                                Some(PossibleConflict::PassedRewrittenDirectory { change_idx: change })
                            } else {
                                None
                            }
                        })
                    };
                    return res;
                }
                Some(child_idx) => {
                    cursor_idx = child_idx;
                    cursor = &self.0[cursor_idx];
                }
            }
        }

        if cursor.is_leaf_node() {
            // A directory whose every change was resolved away (a rename source that
            // `remove_leaf()` dropped) is left as a node without children or change.
            // Nothing of *ours* is at its path any more (t6423 12m with
            // `merge.directoryRenames=false`, B's symlink at the emptied `dir/subdir`).
            PossibleConflict::Match {
                change_idx: cursor.change_idx?,
            }
        } else {
            PossibleConflict::TreeToNonTree {
                change_idx: cursor.change_idx,
            }
        }
        .into()
    }

    /// Whether nothing of this side stays beneath the directory at `location` once
    /// the merge is done, so that it is no longer in the way of a non-directory
    /// placed at `location` itself.
    ///
    /// merge-ort decides a file/directory conflict in `process_entry()` only after
    /// every path below the directory was processed: when the directory merged to
    /// nothing, "directory no longer in the way, but we do have a file we need to
    /// place here" (merge-ort.c:4090-4110). The other side has a non-directory at
    /// `location`, so every base path below it is gone there, and what this side
    /// leaves below it is what survives. Deletions leave nothing; neither does the
    /// source of a rename, which `process_renames()` marks "resolved by removal"
    /// (merge-ort.c:3222-3226). Additions, modifications (a modify/delete keeps
    /// the file) and rename destinations stay.
    pub fn nothing_remains_beneath(&self, location: &BStr, changes: &ChangeListRef) -> bool {
        let mut cursor_idx = 0;
        for component in to_components(location) {
            match self.0[cursor_idx].children.get(component) {
                Some(&child_idx) => cursor_idx = child_idx,
                None => return true,
            }
        }
        let mut pending: Vec<usize> = self.0[cursor_idx].children.values().copied().collect();
        while let Some(node_idx) = pending.pop() {
            let node = &self.0[node_idx];
            pending.extend(node.children.values().copied());
            let Some(change_idx) = node.change_idx else {
                continue;
            };
            let change = &changes[change_idx].inner;
            if change.entry_mode().is_tree() {
                continue;
            }
            let removed = match node.location {
                ChangeLocation::RenamedLocation => false,
                ChangeLocation::CurrentLocation => {
                    matches!(change, Change::Deletion { .. } | Change::Rewrite { .. })
                }
            };
            if !removed {
                return false;
            }
        }
        true
    }

    /// Compare both changes and return `true` if they are *not* exactly the same.
    /// One two changes are the same, they will have the same effect.
    /// Since this is called after [`Self::check_conflict`], *our* change will not be applied,
    /// only theirs, which naturally avoids double-application
    /// (which shouldn't have side effects, but let's not risk it)
    pub fn is_not_same_change_in_possible_conflict(
        &self,
        theirs: &Change,
        conflict: &PossibleConflict,
        our_changes: &ChangeListRef,
    ) -> bool {
        conflict
            .change_idx()
            .is_none_or(|idx| !is_same_change_ignoring_relation(&our_changes[idx].inner, theirs))
    }

    pub fn remove_existing_leaf(&mut self, location: &BStr) {
        self.remove_leaf_inner(location, true);
    }

    pub fn remove_leaf(&mut self, location: &BStr) {
        self.remove_leaf_inner(location, false);
    }

    fn remove_leaf_inner(&mut self, location: &BStr, must_exist: bool) {
        let mut components = to_components(location).peekable();
        let mut cursor_idx = 0;
        while let Some(component) = components.next() {
            match self.0[cursor_idx].children.get(component).copied() {
                None => debug_assert!(!must_exist, "didn't find '{location}' for removal"),
                Some(existing_idx) => {
                    let is_last = components.peek().is_none();
                    if is_last {
                        // A path can be a leaf change and a directory at once: one side
                        // deleted the blob `x/d` and added `x/d/f` (t6423 7e). Handling the
                        // blob's change must not detach the directory below it, whose
                        // changes are still looked up through this node.
                        if self.0[existing_idx].is_leaf_node() {
                            self.0[cursor_idx].children.remove(component);
                        }
                        self.0[existing_idx].change_idx = None;
                    }
                    cursor_idx = existing_idx;
                }
            }
        }
    }

    /// Insert `new_change` which affects this tree into it and put it into `storage` to obtain the index.
    /// Panic if that change already exists as it must be made so that it definitely doesn't overlap with this tree.
    pub fn insert(&mut self, new_change: &Change, new_change_idx: usize) {
        let mut next_index = self.0.len();
        let mut cursor = &mut self.0[0];
        for component in to_components(new_change.location()) {
            match cursor.children.get(component).copied() {
                None => {
                    cursor.children.insert(component.to_owned(), next_index);
                    self.0.push(TreeNode::default());
                    cursor = &mut self.0[next_index];
                    next_index += 1;
                }
                Some(existing_idx) => {
                    cursor = &mut self.0[existing_idx];
                }
            }
        }

        debug_assert!(
            !matches!(new_change, Change::Rewrite { .. }),
            "BUG: we thought we wouldn't do that current.location is related?"
        );
        cursor.change_idx = Some(new_change_idx);
        cursor.location = ChangeLocation::CurrentLocation;
    }
}

/// Compare `ours` and `theirs` for having exactly the same effect, ignoring every
/// [`Relation`](gix_diff::tree::visit::Relation) id they carry.
///
/// The ids behind `Relation::Parent`/`ChildOfParent` come from a counter that
/// `gix_diff::tree` bumps once per added or deleted directory as it walks a *single* diff
/// — `ChangeId` is documented as "unique only within one diff operation". *Our* changes and
/// *their* changes come from two independent diffs against the merge base, so the same
/// change can, and routinely does, carry different ids on the two sides: one extra directory
/// sorting ahead of another shifts every following id by one.
///
/// Deriving equality from `PartialEq` therefore reports two byte-identical additions as
/// differing, which the caller turns into a conflict. Comparing location, mode and object id
/// — everything that actually lands in the merged tree — is what "same change" means here.
fn is_same_change_ignoring_relation(ours: &Change, theirs: &Change) -> bool {
    match (ours, theirs) {
        (
            Change::Addition {
                location: our_location,
                entry_mode: our_mode,
                id: our_id,
                relation: _,
            },
            Change::Addition {
                location: their_location,
                entry_mode: their_mode,
                id: their_id,
                relation: _,
            },
        )
        | (
            Change::Deletion {
                location: our_location,
                entry_mode: our_mode,
                id: our_id,
                relation: _,
            },
            Change::Deletion {
                location: their_location,
                entry_mode: their_mode,
                id: their_id,
                relation: _,
            },
        ) => our_location == their_location && our_mode == their_mode && our_id == their_id,
        (
            Change::Rewrite {
                source_location: our_source_location,
                source_entry_mode: our_source_mode,
                source_id: our_source_id,
                diff: our_diff,
                entry_mode: our_mode,
                id: our_id,
                location: our_location,
                copy: our_copy,
                source_relation: _,
                relation: _,
            },
            Change::Rewrite {
                source_location: their_source_location,
                source_entry_mode: their_source_mode,
                source_id: their_source_id,
                diff: their_diff,
                entry_mode: their_mode,
                id: their_id,
                location: their_location,
                copy: their_copy,
                source_relation: _,
                relation: _,
            },
        ) => {
            our_source_location == their_source_location
                && our_source_mode == their_source_mode
                && our_source_id == their_source_id
                && our_diff == their_diff
                && our_mode == their_mode
                && our_id == their_id
                && our_location == their_location
                && our_copy == their_copy
        }
        // `Modification` carries no relation, so its derived equality is already correct,
        // and any mismatched pair of variants is genuinely a different change.
        (ours, theirs) => ours == theirs,
    }
}

pub fn to_components_bstring_ref(rela_path: &BString) -> impl Iterator<Item = &BStr> {
    rela_path.split(|b| *b == b'/').map(Into::into)
}

pub fn to_components(rela_path: &BStr) -> impl Iterator<Item = &BStr> {
    rela_path.split(|b| *b == b'/').map(Into::into)
}

impl Conflict {
    pub(super) fn without_resolution(
        resolution: ResolutionFailure,
        changes: (&Change, &Change, ConflictMapping, ConflictMapping),
        entries: [Option<ConflictIndexEntry>; 3],
    ) -> Self {
        Conflict::maybe_resolved(Err(resolution), changes, entries)
    }

    pub(super) fn with_resolution(
        resolution: Resolution,
        changes: (&Change, &Change, ConflictMapping, ConflictMapping),
        entries: [Option<ConflictIndexEntry>; 3],
    ) -> Self {
        Conflict::maybe_resolved(Ok(resolution), changes, entries)
    }

    fn maybe_resolved(
        resolution: Result<Resolution, ResolutionFailure>,
        (ours, theirs, map, outer_map): (&Change, &Change, ConflictMapping, ConflictMapping),
        entries: [Option<ConflictIndexEntry>; 3],
    ) -> Self {
        Conflict {
            resolution,
            ours: ours.clone(),
            theirs: theirs.clone(),
            entries,
            map: map.to_global(outer_map),
        }
    }

    pub(super) fn unknown(changes: (&Change, &Change, ConflictMapping, ConflictMapping)) -> Self {
        let (source_mode, source_id) = changes.0.source_entry_mode_and_id();
        let (our_mode, our_id) = changes.0.entry_mode_and_id();
        let (their_mode, their_id) = changes.1.entry_mode_and_id();
        let entries = [
            Some(ConflictIndexEntry {
                mode: source_mode,
                id: source_id.into(),
                path_hint: Some(ConflictIndexEntryPathHint::Source),
            }),
            Some(ConflictIndexEntry {
                mode: our_mode,
                id: our_id.into(),
                path_hint: Some(ConflictIndexEntryPathHint::Current),
            }),
            Some(ConflictIndexEntry {
                mode: their_mode,
                id: their_id.into(),
                path_hint: Some(ConflictIndexEntryPathHint::RenamedOrTheirs),
            }),
        ];
        Conflict::maybe_resolved(Err(ResolutionFailure::Unknown), changes, entries)
    }
}
