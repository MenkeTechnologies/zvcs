//! What a *split* index remembers about its shared half — git's `struct split_index`
//! (split-index.h) as it stands after `merge_base_index()` (split-index.c:158-200).
//!
//! Reading a split index dissolves the `link` extension into one flat entry list, and
//! that list on its own cannot be written back out: the file's shape is a function of
//! *which* entries the shared half already holds, at which position, and which of them
//! the split half is overriding. git keeps that by sharing `cache_entry` pointers
//! between `istate->cache[]` and `si->base->cache[]` and marking each shared entry with
//! its base position (`ce->index`) and `CE_UPDATE_IN_BASE`.
//!
//! This crate cannot share entries between two states, so the same three facts are kept
//! here instead, in base order:
//!
//!  * the entry as the base holds it *after* the split half's replacements were applied
//!    — which is what `replace_entry()` leaves in `si->base->cache[pos]`, since `dst`
//!    there **is** the base entry (split-index.c:130-155);
//!  * [`BaseEntry::replaced`], git's `CE_UPDATE_IN_BASE`: the base entry already had a
//!    stand-in in the split half, so it keeps one — git only ever clears that flag when
//!    it writes a *new* shared index;
//!  * [`BaseEntry::removed`], git's `CE_REMOVE` from the delete bitmap: the base entry
//!    is gone and its delete bit has to be written again.
//!
//! Entries are matched back to the base by path rather than by a stored position. git's
//! `ce->index` survives only where a code path reuses the very same `cache_entry`, and
//! `prepare_to_write_split_index()` falls back to exactly this comparison whenever it
//! does not (`ce->ce_namelen != base->ce_namelen || strcmp(...)`, split-index.c:315-318,
//! and `compare_ce_content()` at :354): a base entry whose path is in the index is the
//! base entry that path stands on.

use std::ops::Range;

use crate::{Entry, PathStorage, PathStorageRef};

/// One entry of the shared index, as the state that links to it sees it.
#[derive(Clone, PartialEq, Eq)]
pub struct BaseEntry {
    /// The entry's content, `si->base->cache[i]` after the replacements were merged in.
    pub entry: Entry,
    /// git's `CE_UPDATE_IN_BASE`: the split half already carries a stand-in for this
    /// base entry, so writing the split half again carries one too.
    pub replaced: bool,
    /// git's `CE_REMOVE`, set by the delete bitmap: this base entry is not part of the
    /// index any more and its delete bit is written again.
    pub removed: bool,
}

/// The shared half of a split index, as `istate->split_index` describes it.
#[derive(Clone, PartialEq, Eq)]
pub struct SplitIndex {
    /// git's `si->base_oid`: the shared index's checksum, which is also its file name.
    pub base_checksum: gix_hash::ObjectId,
    /// git's `si->base->cache[]`, in base order — the order the delete and replace
    /// bitmaps index into.
    pub base: Vec<BaseEntry>,
    /// The path storage `base`'s entries point into; the shared index's own, kept
    /// separate from the state's so neither has to be rewritten to hold the other.
    pub base_path_backing: PathStorage,
}

impl SplitIndex {
    /// The path of the base entry at `index`, as the shared index spells it.
    pub fn path_at(&self, index: usize) -> &bstr::BStr {
        self.base[index].entry.path_in(&self.base_path_backing)
    }

    /// The position of the entry for `path` at `stage` in the base, if it holds one —
    /// git's fallback to a name comparison when `ce->index` cannot be trusted
    /// (split-index.c:315).
    ///
    /// A binary search: the shared index is written by `do_write_index()`, which
    /// serialises `cache[]` in the order git keeps it, and that order is sorted by path
    /// and then by stage. The stage is part of the key because an unmerged path has three
    /// entries under one name and each stands on a base entry of its own.
    pub fn position_of(&self, path: &bstr::BStr, stage: crate::entry::Stage) -> Option<usize> {
        self.base
            .binary_search_by(|probe| {
                probe
                    .entry
                    .path_in(&self.base_path_backing)
                    .cmp(path)
                    .then_with(|| probe.entry.flags.stage().cmp(&stage))
            })
            .ok()
    }

    /// Build the base from `entries` and `backing` — git's `move_cache_to_base_index()`
    /// (split-index.c:80-121), which hands the whole cache to a freshly allocated base
    /// and clears `CE_UPDATE_IN_BASE` on every entry it moved.
    pub fn from_written_shared_index(
        base_checksum: gix_hash::ObjectId,
        entries: &[Entry],
        backing: &PathStorageRef,
    ) -> Self {
        let mut base_path_backing = PathStorage::with_capacity(backing.len());
        let base = entries
            .iter()
            .map(|entry| {
                let start = base_path_backing.len();
                base_path_backing.extend_from_slice(entry.path_in(backing));
                let mut entry = entry.clone();
                entry.path = Range {
                    start,
                    end: base_path_backing.len(),
                };
                BaseEntry {
                    entry,
                    replaced: false,
                    removed: false,
                }
            })
            .collect();
        SplitIndex {
            base_checksum,
            base,
            base_path_backing,
        }
    }
}
