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
    write_split(repo, index, options, gix::index::file::split::Request::Keep)
}

/// [`write_with`] for a caller that also knows what git's `cache_changed` would say about
/// the *shape* of the index — see [`gix::index::file::split::Request`].
///
/// This is git's `write_locked_index()` (read-cache.c:3309) rather than
/// `do_write_locked_index()`: an index that was read as a split index is written back as
/// one, with only the entries the shared half does not already hold, unless `request`
/// says otherwise. Every writer in this port goes through here for the same reason they
/// all go through the smudge — git has one such function, and a writer that skipped it
/// would dissolve a repository's split index the first time it touched it.
pub fn write_split(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    smudge_racily_clean(repo, index);
    // `alternate_index_output` (read-cache.c:3332): `read-tree --index-output=<file>` and
    // friends write somewhere that is not the repository's index, and git writes a whole
    // index there whatever shape the real one has.
    if index.path() != repo.index_path() {
        return index.write(options);
    }
    write_locked(repo, index, options, request)
}

/// git's `write_locked_index()` proper, without the smudge its `do_write_index()` does —
/// for the one caller, `update-index`, that already resolved every entry it touched.
pub fn write_locked(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    let request = tweak_split_index(repo, request);
    let git_dir = repo.git_dir().to_owned();
    let max_percent = crate::config::split_index_max_percent_change(repo);
    match index.write_locked(&git_dir, request, max_percent, options) {
        Ok(_) => Ok(()),
        Err(gix::index::file::split::Error::Write(err)) => Err(err),
        Err(err) => Err(gix::index::file::write::Error::Io(std::io::Error::other(err).into())),
    }
}

/// `tweak_split_index()` (read-cache.c:1932-1946), which git runs on every index it reads:
/// `core.splitIndex=false` calls `remove_split_index()` and so sets `SOMETHING_CHANGED`,
/// and `core.splitIndex=true` calls `add_split_index()`, which sets `SPLIT_INDEX_ORDERED`
/// only when the index is not split already.
///
/// A request the caller made itself outranks both — it describes a change that has already
/// happened, and git's own order puts `cache_changed & ~EXTMASK` first.
///
/// ### Only the `false` half is here
///
/// git runs this from `post_read_index_from()`, so it applies to an index that was *read*
/// and to no other. Applied at the write instead, the `true` half would split indexes git
/// leaves whole: a plain `read-tree <tree>` never reads the old index at all
/// (builtin/read-tree.c:201 reads it only `if (opts.reset || opts.merge || opts.prefix)`),
/// so `add_split_index()` never runs for it and stock writes one whole file even under
/// `core.splitIndex=true`. Moving the whole tweak to where it belongs needs the read side
/// to carry `SPLIT_INDEX_ORDERED` from `add_split_index()` through to the write, which
/// this port has no room for on `State` yet; the `false` half needs no such carrier,
/// because dropping the shared half is a decision the write can make on its own.
fn tweak_split_index(
    repo: &gix::Repository,
    request: gix::index::file::split::Request,
) -> gix::index::file::split::Request {
    if request != gix::index::file::split::Request::Keep {
        return request;
    }
    match crate::config::split_index(repo) {
        Some(false) => gix::index::file::split::Request::Whole,
        _ => request,
    }
}

/// The bytes of a path as the index spells it, for diagnostics.
#[allow(dead_code)]
pub(crate) fn display(path: &gix::bstr::BStr) -> String {
    path.to_str_lossy().into_owned()
}
