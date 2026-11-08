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

/// `ce_modified_check_fs()` (read-cache.c:2513-2533) — has the worktree content at `full` moved
/// away from the blob `id` the index recorded?
///
/// ```c
/// switch (st->st_mode & S_IFMT) {
/// case S_IFREG: if (ce_compare_data(istate, ce, st)) return DATA_CHANGED; break;
/// case S_IFLNK: if (ce_compare_link(ce, xsize_t(st->st_size))) return DATA_CHANGED; break;
/// case S_IFDIR: if (S_ISGITLINK(ce->ce_mode)) return ce_compare_gitlink(ce) ? DATA_CHANGED : 0;
///         /* else fallthrough */
/// default: return TYPE_CHANGED;
/// }
/// ```
///
/// The switch is on the **filesystem** type, not on the mode the index recorded, and the symlink
/// arm is `ce_compare_link()` — `strbuf_readlink()` compared against the blob. It reads the *link*,
/// never what the link points at. Hashing `std::fs::read()` for every entry followed each symlink
/// to its target instead, so a clean `link-to-file -> README.md` hashed `README.md`'s bytes,
/// matched nothing, and was called modified. Both callers below did that, which is why
/// `git checkout` onto a branch of symlinks listed up to six paths where stock listed one, and why
/// *which* six varied run to run: only entries that happened to look racy at that moment were
/// asked.
///
/// `md` must come from `symlink_metadata` — this is `lstat`, and following the link is the bug.
///
/// Hashing raw can still disagree with a filtered blob (`core.autocrlf`, a clean filter); the
/// write-time smudge reads regular files through [`compare_data`] instead. Erring
/// toward "modified" costs a re-read; erring the other way makes a real change invisible, so an
/// unreadable file counts as modified, exactly as `ce_compare_data()`'s `match = -1` does.
pub fn modified_check_fs(
    object_hash: gix::hash::Kind,
    full: &std::path::Path,
    md: &std::fs::Metadata,
    id: &gix::ObjectId,
) -> bool {
    let ft = md.file_type();
    let hashed = if ft.is_symlink() {
        // `ce_compare_link()`.
        std::fs::read_link(full).ok().map(|t| gix::path::into_bstr(t).into_owned())
    } else if ft.is_file() {
        // `ce_compare_data()`.
        std::fs::read(full).ok().map(Into::into)
    } else {
        // `default: return TYPE_CHANGED`. A gitlink is never asked — `is_racy_timestamp()` says
        // no for `S_ISGITLINK` — so a directory here means the index recorded a blob.
        return true;
    };
    match hashed {
        Some(bytes) => gix::objs::compute_hash(object_hash, gix::objs::Kind::Blob, &bytes)
            .is_ok_and(|hash| hash != *id),
        None => true,
    }
}

/// `ce_match_stat_basic()` (read-cache.c:311-352): does a plain `lstat` comparison
/// already tell the entry apart from the file? Only the yes/no answer is needed here —
/// `ce_smudge_racily_clean_entry()` returns early on any bit.
///
/// The stat half is `match_stat_data()` (statinfo.c:64-104), which honours `core.trustctime`
/// and `core.checkStat` through `opts`: with the default `core.trustctime=true`, a rewrite
/// that moved `ctime` into a later second is a stat difference, so git leaves the entry
/// alone and every later reader reports it by stat. Comparing only size and mtime smudged
/// those entries too.
fn match_stat_basic_differs(
    entry: &gix::index::Entry,
    meta: &gix::index::fs::Metadata,
    current: &gix::index::entry::Stat,
    opts: gix::index::entry::stat::Options,
    trust_executable_bit: bool,
    has_symlinks: bool,
    empty_blob: &gix::ObjectId,
) -> bool {
    use gix::index::entry::Mode;
    // `if (ce->ce_flags & CE_REMOVE) return MODE_CHANGED | DATA_CHANGED | TYPE_CHANGED;`
    if entry.flags.contains(gix::index::entry::Flags::REMOVE) {
        return true;
    }
    let type_or_mode = if entry.mode == Mode::SYMLINK {
        // `if (!S_ISLNK(st->st_mode) && (has_symlinks || !S_ISREG(st->st_mode)))`
        !meta.is_symlink() && (has_symlinks || !meta.is_file())
    } else {
        // `S_IFREG`: a type change, or — only the owner x bit counts — a mode change.
        !meta.is_file()
            || (trust_executable_bit && (entry.mode == Mode::FILE_EXECUTABLE) != meta.is_executable())
    };
    type_or_mode
        || !entry.stat.matches(current, opts)
        // "Racily smudged entry?": an already-zeroed size on a non-empty blob.
        || (entry.stat.size == 0 && entry.id != *empty_blob)
}

/// `is_racy_stat()` (read-cache.c:353-366): the entry is racy when the index is not strictly newer
/// than the file it recorded. The nanosecond refinement there is behind `USE_NSEC`, which the
/// git this port targets is not built with — measured on the stock binary, which smudges an
/// entry whose recorded nanoseconds are *earlier* than the index's within the same second. So
/// the comparison is on whole seconds, exactly as the non-`USE_NSEC` branch does it.
pub(crate) fn is_racy_stat(index: &gix::index::File, stat: &gix::index::entry::Stat) -> bool {
    let isec = index.timestamp().unix_seconds();
    isec != 0 && isec as u32 <= stat.mtime.secs
}

/// `!ie_match_stat(istate, ce, &st, 0)` (read-cache.c:383-442) for `entry` of `index`, whose
/// worktree file is `full`: whether the checked-out file is known to hold the entry's content.
/// `core.fileMode` and `core.symlinks` are `trust_executable_bit` and `has_symlinks`.
///
/// The fsmonitor arms (`refresh_fsmonitor()`, `CE_FSMONITOR_VALID`) are not taken: no index this
/// port reads carries a valid fsmonitor bit.
pub(crate) fn ie_match_stat_unchanged(
    index: &gix::index::File,
    entry: &gix::index::Entry,
    full: &std::path::Path,
    opts: gix::index::entry::stat::Options,
    trust_executable_bit: bool,
    has_symlinks: bool,
) -> bool {
    use gix::index::entry::Flags;
    // "If it's marked as always valid in the index, it's valid whatever the checked-out copy
    // says. skip-worktree has the same effect with higher precedence."
    if entry.flags.intersects(Flags::SKIP_WORKTREE | Flags::ASSUME_VALID) {
        return true;
    }
    ie_match_stat_unchanged_ignoring_valid(index, entry, full, opts, trust_executable_bit, has_symlinks)
}

/// `!ie_match_stat(istate, ce, &st, CE_MATCH_IGNORE_VALID | CE_MATCH_IGNORE_SKIP_WORKTREE)`
/// (read-cache.c:388-402): [`ie_match_stat_unchanged`] without the assume-valid and
/// skip-worktree shortcut, which is how `verify_uptodate_1()` (unpack-trees.c:2244-2245) asks
/// about an entry whose bits it does not trust.
pub(crate) fn ie_match_stat_unchanged_ignoring_valid(
    index: &gix::index::File,
    entry: &gix::index::Entry,
    full: &std::path::Path,
    opts: gix::index::entry::stat::Options,
    trust_executable_bit: bool,
    has_symlinks: bool,
) -> bool {
    use gix::index::entry::Flags;
    // "Intent-to-add entries have not been added, so the index entry by definition never
    // matches what is in the work tree until it actually gets added."
    if entry.flags.contains(Flags::INTENT_TO_ADD) {
        return false;
    }
    // The caller's `lstat()`; a path it cannot stat never gets here in git.
    let Ok(meta) = std::fs::symlink_metadata(full) else {
        return false;
    };
    let Ok(fs_meta) = gix::index::fs::Metadata::from_path_no_follow(full) else {
        return false;
    };
    let Ok(current) = gix::index::entry::Stat::from_fs(&fs_meta) else {
        return false;
    };
    let empty_blob = index.object_hash().empty_blob();
    // `changed = ce_match_stat_basic(ce, st);`
    if match_stat_basic_differs(entry, &fs_meta, &current, opts, trust_executable_bit, has_symlinks, &empty_blob) {
        return false;
    }
    // `if (!changed && is_racy_timestamp(istate, ce)) changed |= ce_modified_check_fs(istate, ce, st);`
    // `is_racy_timestamp()` is never true for a gitlink.
    let racy = entry.mode != gix::index::entry::Mode::COMMIT && is_racy_stat(index, &entry.stat);
    !(racy && modified_check_fs(index.object_hash(), full, &meta, &entry.id))
}

/// Smudge every racily-clean entry of `index`, as `do_write_index()` does before serialising.
///
/// ```c
/// if (!ce_uptodate(ce) && is_racy_timestamp(istate, ce))
///         ce_smudge_racily_clean_entry(istate, ce);
/// ```
///
/// (read-cache.c:2902-2903.) An entry a command has just verified against the worktree carries
/// [`UPTODATE`](gix::index::entry::Flags::UPTODATE) (`ce_mark_uptodate()`), and git trusts that
/// instead of hashing it a second time.
///
/// A no-op for an index with no timestamp (never read from disk), for a bare repository, and for
/// entries whose `mtime` is older than the index's own — the overwhelming majority.
pub fn smudge_racily_clean(repo: &gix::Repository, index: &mut gix::index::File) {
    let object_hash = index.object_hash();
    let mut filter = None;
    let smudge: Vec<usize> = racy_comparisons(repo, index)
        .into_iter()
        // `ce_modified_check_fs()`: the stat agrees, so the content has to answer.
        .filter(|(idx, full, meta)| {
            let entry = &index.entries()[*idx];
            if meta.file_type().is_file() {
                compare_data(repo, &mut filter, entry.path(index), full, &entry.id)
            } else {
                modified_check_fs(object_hash, full, meta, &entry.id)
            }
        })
        .map(|(idx, _, _)| idx)
        .collect();

    for idx in smudge {
        // `ce->ce_stat_data.sd_size = 0` — the one field git touches, so nothing else about the
        // entry is disturbed.
        index.entries_mut()[idx].stat.size = 0;
    }
}

/// `ce_compare_data()` (read-cache.c:231-245) for the regular file `full` the index records at
/// `rela_path`: `index_fd()` is handed the entry's name, so the bytes go through
/// `convert_to_git()` — clean filter, `ident`, EOL normalisation — before they are hashed, and a
/// file that only differs from its blob by what checkout put back is not a change. Its flags are
/// 0, so no `core.safecrlf` round-trip check runs. A file that cannot be read or converted is
/// `match = -1`, a change.
fn compare_data(
    repo: &gix::Repository,
    filter: &mut Option<crate::porcelain::convert_to_git::WorktreeFilter>,
    rela_path: &gix::bstr::BStr,
    full: &std::path::Path,
    id: &gix::ObjectId,
) -> bool {
    let Ok(bytes) = std::fs::read(full) else {
        return true;
    };
    let filter = match filter {
        Some(f) => f,
        none => match crate::porcelain::convert_to_git::WorktreeFilter::new(repo, false, false) {
            Ok(f) => none.insert(f),
            Err(_) => return true,
        },
    };
    match filter.convert(repo, &gix::path::from_bstr(rela_path), &bytes) {
        Ok(stored) => gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &stored)
            .is_ok_and(|hash| hash != *id),
        Err(_) => true,
    }
}

/// Does the smudge loop of `do_write_index()` hash a regular file through `index_fd()`?
///
/// `ce_modified_check_fs()`'s `S_IFREG` arm is `ce_compare_data()` (read-cache.c:231-245), which
/// opens the file and hands it to `index_fd()`; that function's first step,
/// `would_convert_to_git_filter_fd()` (object-file.c:952), is an attribute lookup. So a write
/// that has a racily clean regular file to compare is where a bad `--attr-source` /
/// `GIT_ATTR_SOURCE` dies (attr.c:1221-1226), even in a command that never asked for an
/// attribute itself. A file `git_open_cloexec()` cannot open never reaches `index_fd()`.
pub(crate) fn smudge_reads_attributes(repo: &gix::Repository, index: &gix::index::File) -> bool {
    racy_comparisons(repo, index)
        .iter()
        .any(|(_, full, meta)| meta.file_type().is_file() && std::fs::File::open(full).is_ok())
}

/// The entries `do_write_index()` (read-cache.c:2912-2917) takes as far as
/// `ce_modified_check_fs()` inside `ce_smudge_racily_clean_entry()` (read-cache.c:2574-2594),
/// each with its worktree path and `lstat`: not `CE_REMOVE`, not `ce_uptodate()`, racy, and with
/// a stat `ce_match_stat_basic()` cannot already tell apart.
fn racy_comparisons(
    repo: &gix::Repository,
    index: &gix::index::File,
) -> Vec<(usize, std::path::PathBuf, std::fs::Metadata)> {
    let mut out = Vec::new();
    if repo.workdir().is_none() {
        return out;
    }
    let timestamp = index.timestamp();
    if timestamp.unix_seconds() == 0 {
        return out;
    }

    let racy = |stat: &gix::index::entry::Stat| is_racy_stat(index, stat);

    // `match_stat_data()` reads `core.trustctime` / `core.checkStat`; `ce_match_stat_basic()`
    // reads `trust_executable_bit` (`core.fileMode`) and `has_symlinks` (`core.symlinks`).
    let stat_opts = repo.stat_options().unwrap_or_default();
    let snapshot = repo.config_snapshot();
    let trust_executable_bit = snapshot.boolean("core.fileMode").unwrap_or(true);
    let has_symlinks = snapshot.boolean("core.symlinks").unwrap_or(true);
    let empty_blob = index.object_hash().empty_blob();

    {
        let backing = index.path_backing();
        for (idx, entry) in index.entries().iter().enumerate() {
            // `if (ce->ce_flags & CE_REMOVE) continue;` and `!ce_uptodate(ce)`.
            if entry
                .flags
                .intersects(gix::index::entry::Flags::REMOVE | gix::index::entry::Flags::UPTODATE)
            {
                continue;
            }
            // Gitlinks always consult the nested repository, so git never calls the smudge for
            // them (`is_racy_timestamp()` returns 0 for `S_ISGITLINK`).
            if entry.mode == gix::index::entry::Mode::COMMIT || !racy(&entry.stat) {
                continue;
            }
            let path = entry.path_in(backing);
            let Some(full) = repo.workdir_path(path) else { continue };
            // `if (lstat(ce->name, &st) < 0) return;`
            let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
            let Ok(fs_meta) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
                continue;
            };
            let Ok(current) = gix::index::entry::Stat::from_fs(&fs_meta) else {
                continue;
            };
            // `if (ce_match_stat_basic(ce, &st)) return;`: a stat that already differs will be
            // reported anyway, so there is nothing to smudge.
            if match_stat_basic_differs(
                entry,
                &fs_meta,
                &current,
                stat_opts,
                trust_executable_bit,
                has_symlinks,
                &empty_blob,
            ) {
                continue;
            }
            out.push((idx, full, meta));
        }
    }
    out
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
/// `do_write_index()`'s first act, before a single byte is written:
///
/// ```c
/// if (!istate->version)
///         istate->version = get_index_format_default(the_repository);
/// ```
///
/// (read-cache.c:2865-2866.) A state read off disk carries the version its file was
/// written in and keeps it — `git -c index.version=4 add b` on a version 2 index leaves
/// it at 2 — while a state built from scratch, which is what every command gets when
/// `.git/index` does not exist yet, has none and so is the only case that consults
/// `index.version` / `GIT_INDEX_VERSION`.
///
/// `gix`'s [`Version`](gix::index::Version) has no zero to stand for "unset", so the
/// distinction rides along on
/// [`State::version_is_unset()`](gix::index::State::version_is_unset()); a caller that
/// already resolved the version itself (`update-index --index-version <n>`,
/// `read-tree`'s fresh-state options) has filled in `options.version` and is left alone.
/// `o->internal.result.version = o->internal.src_index->version` (unpack-trees.c:1940): an index
/// rebuilt from a tree is rewritten in the version of the one it replaces, whatever `index.version`
/// says. An `old` that was never read off disk has no version to hand on.
pub fn inherit_version(old: &gix::index::File, new: &mut gix::index::File) {
    if !old.version_is_unset() {
        new.set_version(old.version());
    }
}

fn resolve_index_version(repo: &gix::Repository, index: &gix::index::File, options: &mut gix::index::write::Options) {
    if options.version.is_none() && index.version_is_unset() {
        options.version = Some(crate::config::index_format_default(repo));
    }
}

pub fn write_split(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    write_split_holding(repo, index, None, options, request)
}

/// [`write`] through `held`, the lock on the index's own path — [`hold_locked_index`] —
/// that the caller took before it read the index, as git's `write_locked_index()` writes
/// through the `lock_file` its caller got from `repo_hold_locked_index()`
/// (read-cache.c:3309). The lock is committed by the write, and rolled back when it is
/// dropped unwritten.
pub fn write_holding(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    held: gix::lock::File,
) -> Result<(), gix::index::file::write::Error> {
    let options = crate::config::index_write_options(repo);
    write_split_holding(repo, index, Some(held), options, gix::index::file::split::Request::Keep)
}

/// `repo_hold_locked_index(repo, &lock, 0)` (lockfile.h, over `hold_lock_file_for_update()`):
/// take `<index>.lock` now, without dying — `None` when another process holds it or the
/// directory cannot be written, which is git's `fd < 0`.
pub fn hold_locked_index(repo: &gix::Repository) -> Option<gix::lock::File> {
    gix::lock::File::acquire_to_update_resource(repo.index_path(), gix::lock::acquire::Fail::Immediately, None).ok()
}

fn write_split_holding(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    held: Option<gix::lock::File>,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    let mut options = options;
    resolve_index_version(repo, index, &mut options);
    // Before the smudge, because that is where `do_write_locked_index()` puts it:
    // `convert_to_sparse()` runs at read-cache.c:3143 and `do_write_index()` — which
    // holds the smudge loop at :2903 — only after it.
    let prepared = crate::sparse_index::before_write(repo, index);
    smudge_racily_clean(repo, index);
    // `alternate_index_output` (read-cache.c:3332): `read-tree --index-output=<file>` and
    // friends write somewhere that is not the repository's index, and git writes a whole
    // index there whatever shape the real one has.
    let result = if index.path() != repo.index_path() {
        index.write(options).map(|_| ())
    } else {
        write_locked_inner(repo, index, held, options, request)
    };
    crate::sparse_index::after_write(repo, index, prepared);
    result
}

/// git's `write_locked_index()` proper for `update-index`, whose caller has already resolved the
/// index version and the split-index request itself.
///
/// The racy-clean smudge still runs: it lives in `do_write_index()` (read-cache.c:2902-2903), under
/// every caller. Skipping it here was how `update-index --refresh` lost a change —
/// `has_racy_timestamp()` makes the refresh rewrite the index (builtin/update-index.c:740-750), the
/// entry it had just reported as modified was written back with its old size, and once that write
/// landed in a later second the entry no longer looked racy, so `diff-files` called it clean. The
/// entries the refresh *did* verify carry `UPTODATE` and are not hashed again.
pub fn write_locked(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    let mut options = options;
    resolve_index_version(repo, index, &mut options);
    let prepared = crate::sparse_index::before_write(repo, index);
    smudge_racily_clean(repo, index);
    let result = write_locked_inner(repo, index, None, options, request);
    crate::sparse_index::after_write(repo, index, prepared);
    result
}

/// The serialisation half of [`write_locked`], so the two entry points above can each run
/// `do_write_locked_index()`'s sparse conversion exactly once.
fn write_locked_inner(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    held: Option<gix::lock::File>,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    let request = tweak_split_index(repo, request);
    let git_dir = repo.git_dir().to_owned();
    let max_percent = crate::config::split_index_max_percent_change(repo);
    let mut bad_expiry = None;
    let result = {
        let mut clean = |current: gix::ObjectId| clean_shared_index_files(repo, &git_dir, current, &mut bad_expiry);
        index.write_locked_holding(held, &git_dir, request, max_percent, options, &mut clean)
    };
    // `repo_config_get_expiry()`'s `git_die_config()` (config.c:2480-2492), taken once the
    // write has been abandoned and its lock released, as git's `die()` releases it.
    if let Some(err) = bad_expiry {
        crate::config::die_config(Some(repo), "splitindex.sharedindexexpire", err.as_deref());
    }
    match result {
        Ok(_) => Ok(()),
        Err(gix::index::file::split::Error::Write(err)) => Err(err),
        Err(err) => Err(gix::index::file::write::Error::Io(std::io::Error::other(err).into())),
    }
}

/// `clean_shared_index_files()` (read-cache.c:3226-3253), which `write_shared_index()` runs
/// once the shared index `current` is in place: every other `sharedindex.*` in `git_dir` that
/// `should_delete_shared_index()` (read-cache.c:3209-3224) finds no newer than
/// `splitIndex.sharedIndexExpire` is unlinked.
///
/// An expiry git would die over is left in `bad_expiry` — `Some(None)` for a valueless key,
/// whose `missing value` error the config layer words — and the write is abandoned.
fn clean_shared_index_files(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
    current: gix::ObjectId,
    bad_expiry: &mut Option<Option<String>>,
) -> std::io::Result<()> {
    // `get_shared_index_expire_date()` (read-cache.c:3186-3207): `2.weeks.ago` unless
    // configured, and `repo_config_get_expiry()` refuses anything but `now` that resolves to
    // the present or later.
    let expire = match crate::config::last_value_implicit(repo, "splitIndex.sharedIndexExpire") {
        None => "2.weeks.ago".to_owned(),
        Some(None) => {
            *bad_expiry = Some(Some("missing value for 'splitindex.sharedindexexpire'".to_owned()));
            return Err(std::io::Error::other("splitindex.sharedindexexpire"));
        }
        Some(Some(value)) => {
            if value != "now" && crate::date::approxidate(&value) >= crate::date::approxidate("now") {
                *bad_expiry = Some(Some(format!("Invalid splitindex.sharedindexexpire: '{value}'")));
                return Err(std::io::Error::other("splitindex.sharedindexexpire"));
            }
            value
        }
    };
    let expiration = crate::date::approxidate(&expire);
    let dir = match std::fs::read_dir(git_dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("error: unable to open git dir: {}: {}", git_dir.display(), crate::external::strerror(&err));
            return Ok(());
        }
    };
    let current = current.to_string();
    for de in dir.flatten() {
        let name = de.file_name();
        let Some(hex) = name.to_str().and_then(|n| n.strip_prefix("sharedindex.")) else {
            continue;
        };
        if hex == current || expiration == 0 {
            continue;
        }
        let path = git_dir.join(&name);
        let mtime = match std::fs::metadata(&path) {
            Ok(meta) => meta.modified().ok(),
            Err(err) => {
                eprintln!("error: could not stat '{}': {}", path.display(), crate::external::strerror(&err));
                continue;
            }
        };
        let seconds = mtime
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        if seconds > expiration {
            continue;
        }
        if let Err(err) = std::fs::remove_file(&path) {
            eprintln!("warning: unable to unlink: {}: {}", path.display(), crate::external::strerror(&err));
        }
    }
    Ok(())
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

/// What the first attribute lookup of a worktree checkout of `subset` is preceded by.
pub(crate) enum AttributeLookup {
    /// An existing file in the way is unlinked first (`checkout_entry()` removes it before it
    /// writes the replacement), so a `die()` at the lookup leaves it gone.
    AfterUnlink(std::path::PathBuf),
    /// Nothing is touched first.
    Plain,
}

/// Would `checkout_entry()` reach an attribute lookup for any file or symlink in `subset`, and
/// if so, for the first one in index order, what has been done to the worktree by then?
///
/// `checkout_entry()` (entry.c) returns early for an entry whose file `ie_match_stat()` calls
/// unchanged, and `ie_match_stat()` only reads the file (`ce_compare_data()` -> `index_fd()` ->
/// attributes) for a racily clean entry. So the first attribute lookup — the one that dies on a
/// bad `--attr-source` / `GIT_ATTR_SOURCE` — happens when a regular file has to be written
/// (missing, stat-different, or not the index's own) or compared because its recorded `mtime`
/// is not older than the index's.
///
/// A symlink differs, as measured against stock: writing a changed or missing one never asks
/// for attributes, but comparing a racy one does.
pub(crate) fn first_attribute_lookup(
    repo: &gix::Repository,
    cur: &gix::index::File,
    subset: &gix::index::File,
) -> Option<AttributeLookup> {
    use gix::index::entry::Mode;
    let stat_opts = repo.stat_options().unwrap_or_default();
    let snapshot = repo.config_snapshot();
    let trust_executable_bit = snapshot.boolean("core.fileMode").unwrap_or(true);
    let has_symlinks = snapshot.boolean("core.symlinks").unwrap_or(true);
    let empty_blob = cur.object_hash().empty_blob();
    let timestamp = cur.timestamp().unix_seconds();
    let backing = subset.path_backing();
    subset.entries().iter().find_map(|wanted| {
        let regular = wanted.mode == Mode::FILE || wanted.mode == Mode::FILE_EXECUTABLE;
        if !regular && wanted.mode != Mode::SYMLINK {
            return None;
        }
        let path = wanted.path_in(backing);
        let full = repo.workdir_path(path)?;
        // A write: the file in the way, if there is one, goes first.
        let write = |exists: bool| {
            regular.then(|| if exists { AttributeLookup::AfterUnlink(full.clone()) } else { AttributeLookup::Plain })
        };
        let exists = std::fs::symlink_metadata(&full).is_ok();
        // A source entry that is the index's own keeps the index's stat data; any other is a
        // fresh entry with none, which can only differ from the file.
        let Some(own) = cur
            .entry_by_path_and_stage(path, gix::index::entry::Stage::Unconflicted)
            .filter(|own| own.id == wanted.id && own.mode == wanted.mode)
        else {
            return write(exists);
        };
        let Ok(fs_meta) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
            return write(exists);
        };
        let Ok(current) = gix::index::entry::Stat::from_fs(&fs_meta) else {
            return write(exists);
        };
        if match_stat_basic_differs(own, &fs_meta, &current, stat_opts, trust_executable_bit, has_symlinks, &empty_blob) {
            return write(exists);
        }
        (timestamp != 0 && timestamp <= i64::from(own.stat.mtime.secs)).then_some(AttributeLookup::Plain)
    })
}
