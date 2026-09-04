//! Writing a *split* index — git's `write_locked_index()` (read-cache.c:3309-3392),
//! `write_shared_index()` (:3241-3276) and `prepare_to_write_split_index()`
//! (split-index.c:235-393).
//!
//! A split index is two files. `$GIT_DIR/sharedindex.<id>` holds the entries and
//! nothing else — `do_write_index(si->base, ...)` runs on a bare `index_state`
//! that only ever received a copy of `cache[]`, so it carries no tree-cache and
//! no other extension. `$GIT_DIR/index` holds whatever entries are *not* in the
//! shared half plus the `link` extension naming it, and keeps the tree-cache and
//! everything else the index had.
//!
//! # Which of the two shapes a write takes
//!
//! `write_locked_index()` decides, and the decision is one condition:
//!
//! ```c
//! if ((!si && !test_split_index_env) ||
//!     alternate_index_output ||
//!     (istate->cache_changed & ~EXTMASK)) {
//!         ret = do_write_locked_index(istate, lock, flags,
//!                                     ~WRITE_SPLIT_INDEX_EXTENSION);
//!         goto out;
//! }
//! ```
//!
//! `~WRITE_SPLIT_INDEX_EXTENSION` is "every extension but `link`", so that branch
//! writes **one whole index and drops the link**. `EXTMASK` covers `CE_ENTRY_ADDED`,
//! `CE_ENTRY_REMOVED` and `CE_ENTRY_CHANGED` (read-cache.c:79-81), so ordinary
//! staging, removal and refreshing of entries do *not* take it — a split index
//! stays split across `add`, `reset`, `read-tree`, `checkout` and every
//! `update-index` that only edits entries.
//!
//! What is left of `cache_changed & ~EXTMASK` is `SOMETHING_CHANGED` alone, and all
//! of git sets that in exactly three places:
//!
//!  * `remove_split_index()` (split-index.c:491) — `core.splitIndex=false` at read
//!    time, or `update-index --no-split-index`;
//!  * `builtin/update-index.c:749` — after `--refresh`/`--really-refresh`, when
//!    `has_racy_timestamp()` says an entry's recorded mtime is not older than the
//!    index's own;
//!  * `builtin/update-index.c:1192` — `--index-version <n>` naming a version the
//!    index does not already have.
//!
//! Which is why `update-index --split-index` on its own splits an index while
//! `update-index --split-index --index-version 4` does not: the second one changed
//! the version, and that is `SOMETHING_CHANGED`. [`Request`] is that decision, and
//! the caller — which is the only place that knows whether any of the three
//! happened — names it.
//!
//! # What the split half holds
//!
//! `prepare_to_write_split_index()` swaps `istate->cache[]` out for a much shorter
//! list and puts it back afterwards. That list is, in order:
//!
//!  1. one *stand-in* per base entry being replaced — same stat data, same id, same
//!     mode, with the name stripped (`CE_STRIP_NAME`, so the path is empty and the
//!     name length zero) — walked in **base order**, each setting its bit in the
//!     replace bitmap;
//!  2. every entry that is not in the base at all, in index order.
//!
//! and the delete bitmap gets a bit for every base entry that is no longer in the
//! index. A base entry is replaced when it already had a stand-in
//! (`CE_UPDATE_IN_BASE`, which is why a replacement stays one), when it is racily
//! clean, or when its content no longer matches what the base holds.

use std::path::{Path, PathBuf};

use crate::{Entry, File, SplitIndex, entry, extension, split_index::BaseEntry, write};

/// The error produced by [`File::write_locked()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Hash(#[from] gix_hash::io::Error),
    #[error(transparent)]
    Write(#[from] crate::file::write::Error),
}

/// The part of git's `cache_changed` that [`File::write_locked()`] reads, named by
/// the caller because it is the caller that performed the change.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Nothing asked for a change of shape. A split index is written split and a
    /// whole one whole, which is `write_locked_index()` with no bit outside
    /// `EXTMASK` set.
    #[default]
    Keep,
    /// git's `SPLIT_INDEX_ORDERED`, set by `add_split_index()` for
    /// `update-index --split-index` and by `too_many_not_shared_entries()`: write a
    /// fresh shared index holding every entry, and a split half naming it.
    NewShared,
    /// git's `SOMETHING_CHANGED`: write one whole index and drop the `link`
    /// extension, whatever shape the index had.
    Whole,
}

/// `default_max_percent_split_change` (read-cache.c:3279).
const DEFAULT_MAX_PERCENT_SPLIT_CHANGE: u32 = 20;

impl File {
    /// git's `write_locked_index()` (read-cache.c:3309): write this index in whichever
    /// of the two shapes `request` and its own split state call for, resolving the
    /// shared half against `git_dir`.
    ///
    /// Returns the id of the shared index when one was written.
    pub fn write_locked(
        &mut self,
        git_dir: &Path,
        request: Request,
        max_percent_split_change: Option<u32>,
        options: write::Options,
    ) -> Result<Option<gix_hash::ObjectId>, Error> {
        if request == Request::Whole {
            // `~WRITE_SPLIT_INDEX_EXTENSION`: the shared half stays on disk — git never
            // unlinks it here — but nothing points at it any more.
            self.state.remove_split_index();
            self.write(options)?;
            return Ok(None);
        }
        if self.state.split_index().is_none() && request != Request::NewShared {
            self.write(options)?;
            return Ok(None);
        }

        // `if (too_many_not_shared_entries(istate)) istate->cache_changed |= SPLIT_INDEX_ORDERED;`
        // (read-cache.c:3348): too much of the index living outside the shared half is
        // the point at which git folds it all back in and starts a new one.
        let new_shared = request == Request::NewShared
            || too_many_not_shared_entries(self, max_percent_split_change);

        let shared_id = if new_shared {
            let id = write_shared(self, git_dir)?;
            // `move_cache_to_base_index()` (split-index.c:80-121).
            self.state.set_split_index(Some(SplitIndex::from_written_shared_index(
                id,
                self.state.entries(),
                self.state.path_backing(),
            )));
            // ```c
            // for (i = 0; i < si->base->cache_nr; i++)
            //         si->base->cache[i]->ce_flags &= ~CE_UPDATE_IN_BASE;
            // ```
            //
            // (split-index.c:119-120.) The base and the index share their entries there,
            // so clearing the flag on the base clears it on the index too: every entry is
            // in the shared half now, and none of them is a replacement of it until
            // something changes one. Without this the split half would carry a stand-in
            // for every entry it just wrote into the base.
            for entry in self.state.entries_mut() {
                entry.flags.remove(entry::Flags::UPDATE_IN_BASE);
            }
            Some(id)
        } else {
            None
        };

        let saved = prepare_to_write_split_index(self);
        let result = self.write(options);
        finish_writing_split_index(self, saved);
        result?;
        Ok(shared_id)
    }

    /// Split this index in two under `git_dir` and write both halves —
    /// `update-index --split-index` on an index that is not one yet.
    ///
    /// A thin name for [`write_locked()`](File::write_locked()) with
    /// [`Request::NewShared`], kept because that is what the option means.
    pub fn write_split(&mut self, git_dir: &Path, options: write::Options) -> Result<gix_hash::ObjectId, Error> {
        Ok(self
            .write_locked(git_dir, Request::NewShared, None, options)?
            .expect("a new shared index is always written for `NewShared`"))
    }
}

/// `too_many_not_shared_entries()` (read-cache.c:3281-3306), with
/// `splitIndex.maxPercentChange` already resolved by the caller.
fn too_many_not_shared_entries(file: &File, max_percent_split_change: Option<u32>) -> bool {
    let max_split = match max_percent_split_change {
        // "0% means always write a new shared index"
        Some(0) => return true,
        // "100% means never write a new shared index"
        Some(100) => return false,
        Some(n) => n,
        None => DEFAULT_MAX_PERCENT_SPLIT_CHANGE,
    };
    let Some(si) = file.state.split_index() else {
        return false;
    };
    let backing = file.state.path_backing();
    // `if (!ce->index) not_shared++` — an entry the shared half does not hold.
    let not_shared = file
        .state
        .entries()
        .iter()
        .filter(|e| si.position_of(e.path_in(backing), e.flags.stage()).is_none())
        .count();
    let cache_nr = file.state.entries().len();
    (cache_nr as u64) * u64::from(max_split) < (not_shared as u64) * 100
}

/// What `prepare_to_write_split_index()` parks in `si->saved_cache` so
/// `finish_writing_split_index()` can put it back.
struct Saved {
    entries: Vec<Entry>,
    link: Option<extension::Link>,
}

/// `prepare_to_write_split_index()` (split-index.c:235-393): replace the state's entry
/// list with the one the split half stores, and build the two bitmaps that say how it
/// relates to the base.
///
/// Entries are matched to the base by path, which is what git falls back to whenever
/// `ce->index` does not identify the same `cache_entry` any more (split-index.c:315-318)
/// — and after an `unpack_trees()` rebuild that is every entry.
fn prepare_to_write_split_index(file: &mut File) -> Saved {
    let si = file
        .state
        .split_index()
        .expect("callers check for a shared half before preparing a split write");
    let base_len = si.base.len();
    let mut matched = vec![false; base_len];
    let mut replace = vec![false; base_len];
    // The stand-in for each replaced base entry, parked at its base position so the
    // written order is the base's, which is the order the replace bitmap is read in.
    let mut stand_in: Vec<Option<Entry>> = vec![None; base_len];
    let mut appended: Vec<Entry> = Vec::new();

    let timestamp = file.state.timestamp();
    let backing = file.state.path_backing();
    for entry in file.state.entries() {
        // "the writer drops `CE_REMOVE` entries" (read-cache.c:2915-2916), and
        // `prepare_to_write_split_index()` skips them for both lists.
        if entry.flags.contains(entry::Flags::REMOVE) {
            continue;
        }
        let Some(pos) = si.position_of(entry.path_in(backing), entry.flags.stage()) else {
            // `if (!ce->index) … continue;` then `entries[nr_entries++] = ce;` — an entry
            // the shared half does not hold is written whole into the split half.
            appended.push(entry.clone());
            continue;
        };
        matched[pos] = true;
        let base = &si.base[pos];
        // `if (ce->ce_flags & CE_UPDATE_IN_BASE)` — already replaced, so still replaced;
        // `else if (!ce_uptodate(ce) && is_racy_timestamp(istate, ce))` — racily clean, so
        // the split half has to carry it for `do_write_index()` to smudge; else
        // `compare_ce_content()`.
        let replaced = base.replaced
            || entry.flags.contains(entry::Flags::UPDATE_IN_BASE)
            || is_racy(entry, timestamp)
            || content_differs(entry, base);
        if replaced {
            replace[pos] = true;
            let mut stripped = entry.clone();
            // `ce->ce_flags |= CE_STRIP_NAME` (split-index.c:367): the name lives in the
            // base entry this stands in for, and the entry writer derives the stored name
            // length from the path, so an empty path is a zero name length.
            stripped.flags |= entry::Flags::STRIP_NAME;
            stripped.path = 0..0;
            stand_in[pos] = Some(stripped);
        }
    }

    // `if ((ce->ce_flags & CE_REMOVE) || !(ce->ce_flags & CE_MATCHED)) ewah_set(si->delete_bitmap, i);`
    let delete: Vec<bool> = (0..base_len)
        .map(|i| si.base[i].removed || !matched[i])
        .collect();

    let mut entries: Vec<Entry> = stand_in.into_iter().flatten().collect();
    entries.extend(appended);

    let link = extension::Link {
        shared_index_checksum: si.base_checksum,
        bitmaps: Some(extension::link::Bitmaps {
            delete: ewah(&delete),
            replace: ewah(&replace),
        }),
    };

    let saved = Saved {
        entries: std::mem::replace(&mut file.state.entries, entries),
        link: file.state.link.take(),
    };
    file.state.link = Some(link);
    saved
}

/// `finish_writing_split_index()` (split-index.c:395-407): put the full entry list back,
/// so the state still describes the repository once the file is on disk.
fn finish_writing_split_index(file: &mut File, saved: Saved) {
    file.state.entries = saved.entries;
    file.state.link = saved.link;
}

/// One serialised ewah bitmap, sized the way `ewah_set()` sizes it: `bit_size` grows to
/// the highest set bit and no further, so an empty bitmap declares zero bits and one
/// whose last set bit is at 1 declares two.
fn ewah(bits: &[bool]) -> gix_bitmap::ewah::Vec {
    let end = bits.iter().rposition(|b| *b).map_or(0, |i| i + 1);
    gix_bitmap::ewah::Vec::from_bits(&bits[..end]).expect("far fewer than 4 billion entries")
}

/// `is_racy_timestamp()` (read-cache.c:370-375): the entry's *recorded* mtime is not
/// older than the index's own, so a change made in that same second could still be
/// invisible to a stat comparison.
///
/// A gitlink is exempt (`!S_ISGITLINK(ce->ce_mode)`) because it is answered by the nested
/// repository rather than by a stat, and an index with no timestamp — one built from
/// scratch — is exempt because there is nothing to be racy against.
fn is_racy(entry: &Entry, timestamp: filetime::FileTime) -> bool {
    if entry.mode == entry::Mode::COMMIT || entry.flags.contains(entry::Flags::UPTODATE) {
        return false;
    }
    let seconds = timestamp.unix_seconds();
    seconds != 0 && seconds <= i64::from(entry.stat.mtime.secs)
}

/// `xstrfmt("%s/sharedindex.%s", gitdir, oid_to_hex(id))` (read-cache.c:3268).
pub fn shared_index_path(git_dir: &Path, id: gix_hash::ObjectId) -> PathBuf {
    git_dir.join(format!("sharedindex.{id}"))
}

/// git's `write_shared_index()` (read-cache.c:3241-3276): serialize every entry to a
/// temporary file in `git_dir`, then rename it to the `sharedindex.<checksum>` its own
/// trailer names.
///
/// `do_write_index(si->base, *temp, WRITE_NO_EXTENSION, flags)` runs against an
/// `index_state` that holds nothing but `cache[]`, so no optional extension is written —
/// which is why [`write::Extensions::None`] is not a simplification here but the port.
///
/// ### The shared half is never written with `index.skipHash`
///
/// `link` refers to the shared index *by its checksum* and the reader verifies it
/// (`expected_checksum`), so a shared index with a zeroed trailer could not be found, let
/// alone verified. `skip_hash` therefore applies only to the split half, which is the one
/// git's own `write_shared_index()` hashes unconditionally to get a name for.
///
/// ### Older shared indexes are left alone
///
/// `clean_shared_index_files()` (read-cache.c:3212) unlinks the other `sharedindex.*`
/// files, but only those `should_delete_shared_index()` finds older than
/// `splitIndex.sharedIndexExpire`, which defaults to two weeks. A shared index this port
/// has just superseded is by definition newer than that, so the loop deletes nothing and
/// is not run.
fn write_shared(file: &File, git_dir: &Path) -> Result<gix_hash::ObjectId, Error> {
    let mut buf = Vec::new();
    let (_version, id) = file.write_to(
        &mut buf,
        write::Options {
            extensions: write::Extensions::None,
            // `do_write_index(si->base, …)` writes the shared half at whatever version the
            // base state carries, which is the version the caller already settled for the
            // index as a whole.
            version: None,
            skip_hash: false,
        },
    )?;

    std::fs::create_dir_all(git_dir)?;
    let final_path = shared_index_path(git_dir, id);
    // `mks_tempfile_sm(repo_git_path(the_repository, "sharedindex_XXXXXX"), 0, 0666)`
    // (read-cache.c:3358) followed by `rename_tempfile()` (read-cache.c:3269): the file
    // only ever appears under its final name complete, so a concurrent reader resolving
    // `link` either finds nothing or finds all of it.
    let temp = git_dir.join(format!("sharedindex_{id}.tmp"));
    std::fs::write(&temp, &buf)?;
    if let Err(err) = std::fs::rename(&temp, &final_path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(id)
}

/// The flags `compare_ce_content()` (split-index.c:200-220) lets through: "only on-disk
/// flags matter", which is `CE_STAGEMASK | CE_VALID | CE_EXTENDED_FLAGS`.
fn on_disk_flags(flags: entry::Flags) -> entry::Flags {
    flags
        & (entry::Flags::STAGE_MASK
            | entry::Flags::ASSUME_VALID
            | entry::Flags::INTENT_TO_ADD
            | entry::Flags::SKIP_WORKTREE)
}

/// `compare_ce_content()` (split-index.c:200-220): everything but the hashmap entry and
/// the name.
fn content_differs(entry: &Entry, base: &BaseEntry) -> bool {
    entry.stat != base.entry.stat
        || entry.mode != base.entry.mode
        || entry.id != base.entry.id
        || on_disk_flags(entry.flags) != on_disk_flags(base.entry.flags)
}
