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
/// Hashing raw can still disagree with a filtered blob (`core.autocrlf`, a clean filter). Erring
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
            // `ce_match_stat_basic()`: a stat that already differs will be reported anyway, so
            // there is nothing to smudge.
            if current.size != entry.stat.size || current.mtime.secs != entry.stat.mtime.secs {
                continue;
            }
            // `ce_modified_check_fs()`: the stat agrees, so the content has to answer.
            if !modified_check_fs(object_hash, &full, &meta, &entry.id) {
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
    // Before the smudge, because that is where `do_write_locked_index()` puts it:
    // `convert_to_sparse()` runs at read-cache.c:3129 and `do_write_index()` — which
    // holds the smudge loop at :2903 — only at :3138.
    convert_to_sparse(repo, index);
    smudge_racily_clean(repo, index);
    // `alternate_index_output` (read-cache.c:3332): `read-tree --index-output=<file>` and
    // friends write somewhere that is not the repository's index, and git writes a whole
    // index there whatever shape the real one has.
    if index.path() != repo.index_path() {
        return index.write(options);
    }
    write_locked_inner(repo, index, options, request)
}

/// git's `write_locked_index()` proper, without the smudge its `do_write_index()` does —
/// for the one caller, `update-index`, that already resolved every entry it touched.
pub fn write_locked(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    options: gix::index::write::Options,
    request: gix::index::file::split::Request,
) -> Result<(), gix::index::file::write::Error> {
    convert_to_sparse(repo, index);
    write_locked_inner(repo, index, options, request)
}

/// The serialisation half of [`write_locked`], so the two entry points above can each run
/// `do_write_locked_index()`'s sparse conversion exactly once.
fn write_locked_inner(
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

/// `convert_to_sparse()` (sparse-index.c:201-259), as far as an index this port also has to
/// be able to *read* can go: the cache-tree half.
///
/// `do_write_locked_index()` calls it before every index write (read-cache.c:3129), so in a
/// cone-mode sparse repository with `index.sparse=true` the cache-tree is freed and rebuilt
/// on the way out of every command that writes the index:
///
/// ```c
/// if (!cache_tree_fully_valid(istate->cache_tree)) {
///         cache_tree_free(&istate->cache_tree);
///         if (cache_tree_update(istate, WRITE_TREE_MISSING_OK))
///                 return 0;
/// }
/// ```
///
/// (sparse-index.c:224-237.) That rebuild is what leaves a fully valid `TREE` behind a
/// `-c index.sparse=true add` of a path that was already staged unchanged, and what mints
/// the two tree objects a `-c index.sparse=true rm --cached` implies — neither of which any
/// of the verbs themselves ask for.
///
/// ### What is deliberately not here
///
/// `convert_to_sparse_rec()` (sparse-index.c:60-130) — collapsing a wholly-excluded
/// directory into one sparse directory entry — is left out, and with it the `sdir`
/// extension and `istate->sparse_index = INDEX_COLLAPSED`. A collapsed index is only
/// legible to a reader that expands it again (`ensure_full_index()`, sparse-index.c:462),
/// which this port does not do, so writing one would leave every other command in this
/// binary reading an index it misunderstands. The index written here therefore stays full
/// where git's would be collapsed; stock git expands its own the next time a command reads
/// it, and both sides arrive at the same full index carrying the same cache-tree.
///
/// The second rebuild (`cache_tree_free()` + `cache_tree_update(istate, 0)`,
/// sparse-index.c:246-248) belongs to that collapse — it exists to recompute the extension
/// over the *collapsed* entries — so it is left out with it. With nothing collapsed it
/// would rebuild the tree that was just built.
fn convert_to_sparse(repo: &gix::Repository, index: &mut gix::index::File) {
    use gix::index::extension::tree::update as cache_tree;

    // `!istate->cache_nr` (sparse-index.c:207). `istate->sparse_index == INDEX_COLLAPSED`
    // cannot arise: nothing in this port ever collapses one.
    if index.entries().is_empty() || !is_sparse_index_allowed(repo, index) {
        return;
    }
    // `index_has_unmerged_entries()` (sparse-index.c:218-222): "If we have unmerged entries,
    // then stay full. Unmerged entries prevent the cache-tree extension from working."
    if index.entries().iter().any(|e| e.stage() != gix::index::entry::Stage::Unconflicted) {
        return;
    }

    let odb = crate::porcelain::write_tree::RepoOdb { repo };
    if index.cache_tree_fully_valid(&odb) {
        return;
    }
    // `cache_tree_free()` then `cache_tree_update(istate, WRITE_TREE_MISSING_OK)`: the whole
    // extension is discarded first, so what comes back is derived from the entries alone.
    // `MISSING_OK` because the rebuild may need trees the repository does not hold yet, and
    // a failure is "silently return" (sparse-index.c:228-236) — git leaves the index alone
    // rather than refuse to write it.
    index.remove_tree();
    let _ = index.cache_tree_update(
        &odb,
        cache_tree::Options {
            missing_ok: true,
            repair: false,
        },
    );
}

/// `is_sparse_index_allowed()` (sparse-index.c:153-199) for a caller that is writing the
/// index, which is git's `flags == 0` — never `SPARSE_INDEX_MEMORY_ONLY`.
///
/// The pattern set the function also loads (`init_sparse_checkout_patterns()`, dir.c:1552,
/// called from sparse-index.c:184) is not read here, because its only reader is
/// `convert_to_sparse_rec()`'s `path_in_sparse_checkout()` — the collapse
/// [`convert_to_sparse`] does not do. What sparse-index.c:195 then tests,
/// `pl->use_cone_patterns`, is `core_sparse_checkout_cone` (dir.c:3513) unless parsing the
/// pattern file found a non-cone pattern and cleared it (dir.c:927-930). A hand-written
/// non-cone `.git/info/sparse-checkout` under `core.sparseCheckoutCone=true` is therefore
/// the one premise where this gate is wider than git's.
fn is_sparse_index_allowed(repo: &gix::Repository, index: &gix::index::File) -> bool {
    let snapshot = repo.config_snapshot();
    // `core_apply_sparse_checkout` and `core_sparse_checkout_cone` (environment.c), both
    // plain booleans that default to off.
    if !snapshot.boolean("core.sparseCheckout").unwrap_or(false)
        || !snapshot.boolean("core.sparseCheckoutCone").unwrap_or(false)
    {
        return false;
    }
    // "The sparse index is not (yet) integrated with a split index" (sparse-index.c:163-167).
    if index.split_index().is_some() || index.had_link() {
        return false;
    }
    // `r->settings.sparse_index`, which is `repo_cfg_bool(r, "index.sparse", …, 0)`
    // (repo-settings.c:63) — off unless the repository or the command line says otherwise.
    snapshot.boolean("index.sparse").unwrap_or(false)
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
