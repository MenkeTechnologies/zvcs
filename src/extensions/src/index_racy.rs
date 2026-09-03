//! `ce_smudge_racily_clean_entry()` (read-cache.c:2560) — the write-time safety net that keeps a
//! change from becoming invisible.
//!
//! # The race
//!
//! An index entry records the file's `mtime` and size. A later `status` compares that stat against
//! the file and, when they agree, declares the entry clean **without reading the file**. That is
//! sound only while the stat can still tell the two apart, and it cannot when the file was written
//! in the *same second* as the index that recorded it: a second write within that second leaves
//! the recorded `mtime` (whole seconds on many filesystems, and equal nanoseconds when the writer
//! is fast) and — for a same-length rewrite — the recorded size untouched. git calls such an entry
//! **racily clean**.
//!
//! Reading is defended already: `is_racy_timestamp()` makes any entry whose `mtime` is not older
//! than the index's own timestamp re-hash rather than trust its stat, and `gix-status` does the
//! same. That defence expires the moment the index is written again for any other reason, because
//! the new index timestamp is later than the entry's `mtime` and the entry stops looking racy —
//! while its stat still matches the file it no longer describes. From then on the difference is
//! invisible: `status` prints nothing, `diff` is empty, `add` stages nothing,
//! `update-index --refresh` finds no work, and a commit made in that state silently leaves the
//! change out.
//!
//! # git's answer, ported here
//!
//! ```c
//! if (!ce_uptodate(ce) && is_racy_timestamp(istate, ce))
//!         ce_smudge_racily_clean_entry(istate, ce);
//! ```
//!
//! (read-cache.c:2902, inside `do_write_index()`.) Every index write re-checks the entries that
//! are racy *at that moment* and, for one whose content really has moved, zeroes the recorded size
//! — a size no file can match, so every later comparison re-hashes. The window is closed at the
//! only moment it can be closed: while the entry is still recognisably racy.
//!
//! This is why it belongs at the write, not at the read, and why it has to run on *every* write
//! rather than in the commands that happen to touch the worktree.

use gix::bstr::ByteSlice;

/// Smudge every racily-clean entry of `index`, as `do_write_index()` does before serialising.
///
/// A no-op for an index with no timestamp (never read from disk), for a bare repository, and for
/// entries whose `mtime` is older than the index's own — the overwhelming majority.
pub fn smudge_racily_clean(repo: &gix::Repository, index: &mut gix::index::File) {
    let Some(workdir) = repo.workdir().map(ToOwned::to_owned) else {
        return;
    };
    let timestamp = index.timestamp();
    if timestamp.unix_seconds() == 0 {
        return;
    }

    // `is_racy_stat()` (read-cache.c:355): the entry is racy when the index is not strictly newer
    // than the file it recorded. The nanosecond refinement there is behind `USE_NSEC`, which the
    // git this port targets is not built with — measured on the stock binary, which smudges an
    // entry whose recorded nanoseconds are *earlier* than the index's within the same second. So
    // the comparison is on whole seconds, exactly as the non-`USE_NSEC` branch does it.
    let racy = |stat: &gix::index::entry::Stat| -> bool {
        let isec = timestamp.unix_seconds() as u32;
        isec <= stat.mtime.secs
    };

    let object_hash = index.object_hash();
    let mut smudge: Vec<usize> = Vec::new();
    {
        let backing = index.path_backing();
        for (idx, entry) in index.entries().iter().enumerate() {
            // Gitlinks always consult the nested repository, so git never calls the smudge for
            // them (`is_racy_timestamp()` returns 0 for `S_ISGITLINK`).
            if entry.mode == gix::index::entry::Mode::COMMIT || !racy(&entry.stat) {
                continue;
            }
            let path = entry.path_in(backing);
            let Some(full) = repo.workdir_path(path) else { continue };
            let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
            let Ok(fs_meta) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
                continue;
            };
            let Ok(current) = gix::index::entry::Stat::from_fs(&fs_meta) else {
                continue;
            };
            let _ = meta;
            // `ce_match_stat_basic()`: a stat that already differs will be reported anyway, so
            // there is nothing to smudge.
            if current.size != entry.stat.size || current.mtime.secs != entry.stat.mtime.secs {
                continue;
            }
            // `ce_modified_check_fs()`: the stat agrees, so the content has to answer. Hashing the
            // file raw can disagree with a filtered blob (`core.autocrlf`, a clean filter), and
            // erring toward "smudge it" only costs the next `status` a re-read of one file, while
            // erring the other way is the invisible change this exists to prevent.
            let Ok(bytes) = std::fs::read(&full) else { continue };
            let disk = gix::objs::compute_hash(object_hash, gix::objs::Kind::Blob, &bytes);
            if disk.is_ok_and(|id| id == entry.id) {
                continue;
            }
            smudge.push(idx);
        }
    }

    for idx in smudge {
        // `ce->ce_stat_data.sd_size = 0` — the one field git touches, so nothing else about the
        // entry is disturbed.
        index.entries_mut()[idx].stat.size = 0;
    }
}

/// Write `index` the way every command in this port writes it: git's racy-clean smudge first, then
/// the serialisation with the repository's `index.*` options.
///
/// One function because git has one place too (`do_write_index()`); a smudge that some writers
/// perform and others skip is a race that reappears through whichever writer forgot.
pub fn write(repo: &gix::Repository, index: &mut gix::index::File) -> Result<(), gix::index::file::write::Error> {
    write_with(repo, index, crate::config::index_write_options(repo))
}

/// [`write`] for a caller that has resolved the write options itself — the one
/// thing a caller can need to decide is the index *version*, which git chooses
/// only for a state it built from scratch
/// ([`crate::config::index_write_options_fresh`]).
pub fn write_with(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
) -> Result<(), gix::index::file::write::Error> {
    smudge_racily_clean(repo, index);
    index.write(options)
}

/// The bytes of a path as the index spells it, for diagnostics.
#[allow(dead_code)]
pub(crate) fn display(path: &gix::bstr::BStr) -> String {
    path.to_str_lossy().into_owned()
}
