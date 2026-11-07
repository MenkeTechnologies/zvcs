//! git's `diff_filespec` helpers, shared by every port that renders a diff.
//!
//! `diff_populate_filespec()` (diff.c) is the one place git decides what bytes a
//! tree entry *diffs as*, and it is not always "the blob". A gitlink names a
//! commit in another repository — an object this object database has no reason to
//! contain — so git substitutes the single line `Subproject commit <oid>` and
//! diffs that. Everything downstream (the `--stat` line counts, the binary
//! heuristic, the patch text) is computed over whatever that function produced,
//! which is why these three helpers belong together and are stated once.

use anyhow::Result;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;

/// The bytes to diff for an entry: a real blob is read from the object database; a
/// submodule (commit entry) is rendered as its `Subproject commit <oid>` line. A blob
/// the object database does not produce is `die("unable to read %s")`
/// (diff.c:4596-4598).
///
/// A side that came back with no content is diffed as git diffs a `NULL` `s->data`: xdiff reads a
/// `NULL` buffer as no lines, and `diff_filespec_is_binary()` leaves it text (diff.c:3703-3709).
pub(crate) fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    Ok(populate(repo, id, is_submodule)?.unwrap_or_default())
}

/// [`content_of`] with git's `NULL` kept apart: `diff_populate_filespec()` as
/// `diff_filespec_is_binary()` calls it, with `check_binary` set (diff.c:3703-3704), which reads
/// the size alone first and dies when that fails, then the content (diff.c:4580-4617); see
/// [`read_content`] for the `None` it can hand back.
pub(crate) fn populate(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Option<Vec<u8>>> {
    if is_submodule {
        return Ok(Some(format!("Subproject commit {}\n", id.to_hex()).into_bytes()));
    }
    populate_blob(&repo.objects, id)
}

/// [`populate`] of a blob side through the object database handle alone.
pub(crate) fn populate_blob(objects: &gix::OdbHandle, id: ObjectId) -> Result<Option<Vec<u8>>> {
    read_size(objects, id)?;
    read_content(objects, id)
}

/// The read `diff_populate_filespec()` makes for `check_binary` or `size_only`: `sizep` alone
/// (diff.c:4580-4598), which `packed_object_info()` answers from the entry's header, or from the
/// delta's own header for a delta (packfile.c:1330-1350), and whose failure is the die.
pub(crate) fn read_size(objects: &gix::OdbHandle, id: ObjectId) -> Result<u64> {
    match objects.try_size(&id) {
        Ok(Some(size)) => Ok(size),
        _ => Err(crate::fatal::die(format!("unable to read {id}"))),
    }
}

/// The read `diff_populate_filespec()` makes for content: `contentp` and `sizep` and no `typep`
/// (diff.c:4580-4617). A packed entry that does not unpack then succeeds with `s->data` `NULL`
/// rather than failing (packfile.c:1323-1327), after the `error()` lines the unpack printed. `None`
/// is that `NULL`; since `diff_populate_filespec()` returns early only for data it holds
/// (diff.c:4496-4497), every later populate of the side reads the object, and prints those lines,
/// again.
pub(crate) fn read_content(objects: &gix::OdbHandle, id: ObjectId) -> Result<Option<Vec<u8>>> {
    use gix::prelude::Find;
    let mut buf = Vec::new();
    let (found, null) = gix::odb::store::find::read_content_without_type(|| {
        objects.try_find(&id, &mut buf).map(|object| object.is_some())
    });
    match (found, null) {
        (_, Some(_size)) => Ok(None),
        (Ok(true), None) => Ok(Some(buf)),
        _ => Err(crate::fatal::die(format!("unable to read {id}"))),
    }
}

/// [`read_content`] for a caller that keeps the size of a `NULL` read: `Err` carries the size
/// `unpack_entry()` left in `*sizep` (packfile.c:1703-1706), which `should_break()` measures
/// (diffcore-break.c:73-80). `None` is an object no source produced.
pub(crate) fn read_content_or_null(objects: &gix::OdbHandle, id: ObjectId) -> Option<std::result::Result<Vec<u8>, u64>> {
    use gix::prelude::Find;
    let mut buf = Vec::new();
    let (found, null) = gix::odb::store::find::read_content_without_type(|| {
        objects.try_find(&id, &mut buf).map(|object| object.is_some())
    });
    match (found, null) {
        (_, Some(size)) => Some(Err(size)),
        (Ok(true), None) => Some(Ok(buf)),
        _ => None,
    }
}

/// `r->index` as `reuse_worktree_file()` (diff.c:4367-4447) consults it, once a command has read
/// it: `cmd_diff_tree()` always does (builtin/diff-tree.c:133), `builtin/diff-pairs.c` only when
/// queueing a gitlink reaches `repo_read_gitmodules()`.
pub(crate) struct Istate {
    index: gix::index::File,
    workdir: std::path::PathBuf,
    stat: gix::index::entry::stat::Options,
    /// `trust_executable_bit` (`core.fileMode`).
    trust_executable_bit: bool,
    /// `has_symlinks` (`core.symlinks`).
    has_symlinks: bool,
}

impl Istate {
    /// `repo_read_index()`. `None` is an `istate->cache` that stays `NULL`: no index file
    /// (`do_read_index()`'s `ENOENT` arm, read-cache.c:2208-2213), an index that cannot be read,
    /// or no work tree whose files could be reused.
    pub(crate) fn read(repo: &gix::Repository) -> Option<Self> {
        let workdir = repo.workdir()?.to_path_buf();
        let index = repo.open_index().ok()?;
        let snapshot = repo.config_snapshot();
        Some(Istate {
            index,
            workdir,
            stat: repo.stat_options().ok()?,
            trust_executable_bit: snapshot.boolean("core.fileMode").unwrap_or(true),
            has_symlinks: snapshot.boolean("core.symlinks").unwrap_or(true),
        })
    }

    /// The top of the work tree.
    pub(crate) fn workdir(&self) -> &std::path::Path {
        &self.workdir
    }

    /// The work-tree file at `name`.
    pub(crate) fn worktree_path(&self, name: &gix::bstr::BStr) -> std::path::PathBuf {
        self.workdir.join(gix::path::from_bstr(name).as_ref())
    }

    /// `reuse_worktree_file()` (diff.c:4367-4447): whether the index says the file checked out at
    /// `name` holds exactly `oid`, so its bytes can stand in for the blob's.
    ///
    /// The `!FAST_WORKING_DIRECTORY && !want_file && has_object_pack()` arm (diff.c:4400-4402)
    /// never fires: `NO_FAST_WORKING_DIRECTORY` is set only for Windows (config.mak.uname:296).
    pub(crate) fn reuse_worktree_file(
        &self,
        repo: &gix::Repository,
        name: &gix::bstr::BStr,
        oid: &ObjectId,
        want_file: bool,
    ) -> bool {
        use gix::index::entry::{Flags, Mode, Stage};
        // `pos = index_name_pos(istate, name, len); if (pos < 0) return 0;`
        let Some(ce) = self.index.entry_by_path_and_stage(name, Stage::Unconflicted) else {
            return false;
        };
        // "This is not the sha1 we are looking for, or unreusable because it is not a regular
        // file."
        if ce.id != *oid || !matches!(ce.mode, Mode::FILE | Mode::FILE_EXECUTABLE) {
            return false;
        }
        // "If ce is marked as "assume unchanged", there is no guarantee that work tree matches
        // what we are looking for."
        if ce.flags.intersects(Flags::ASSUME_VALID | Flags::SKIP_WORKTREE) {
            return false;
        }
        // "If this path does not match our sparse-checkout definition, then the file will not be
        // in the working directory."
        if !crate::sparse_index::path_in_sparse_checkout(repo, name) {
            return false;
        }
        // "If we'd have to convert the file contents anyway, that makes the optimization not
        // worthwhile." Asked after the index lookup rather than before it (diff.c:4408-4410):
        // every check here only answers yes or no, and this one reads attributes.
        if !want_file
            && crate::untracked_cache::Convert::new(repo, &self.index)
                .map_or(true, |mut convert| convert.would_convert_to_git(name))
        {
            return false;
        }
        // `ce_uptodate(ce)` is an in-core mark a fresh `repo_read_index()` never sets, so the
        // answer is `!lstat(name, &st) && !ie_match_stat(istate, ce, &st, 0)`.
        crate::index_racy::ie_match_stat_unchanged(
            &self.index,
            ce,
            &self.worktree_path(name),
            self.stat,
            self.trust_executable_bit,
            self.has_symlinks,
        )
    }

    /// `diff_populate_filespec()`'s read of a reused regular file (diff.c:4505-4560): `lstat()`,
    /// an empty file as `""`, and the bytes. A file that cannot be stat'ed or opened is
    /// `err_empty`, the empty content. No conversion runs: [`Self::reuse_worktree_file`] refused
    /// every path `convert_to_git()` would change.
    pub(crate) fn populate(&self, name: &gix::bstr::BStr) -> Vec<u8> {
        std::fs::read(self.worktree_path(name)).unwrap_or_default()
    }
}

/// `builtin_diffstat()`'s `fill_mmfile()` of each side after `diff_filespec_is_binary()` populated
/// it (diff.c:681-695, 4190-4235): a side whose data is still `NULL` is populated once more, without
/// `check_binary`, which reads its content again. `null_sides` are the ids of the sides that came
/// back `NULL`, old before new.
pub(crate) fn fill_mmfile_again(repo: &gix::Repository, null_sides: impl IntoIterator<Item = ObjectId>) -> Result<()> {
    for id in null_sides {
        read_content(&repo.objects, id)?;
    }
    Ok(())
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes.
pub(crate) fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// `diff_filespec_is_binary()` (diff.c:3712-3734): the path's diff driver decides
/// when it has an opinion — `driver` is [`super::cat_file::diff_attr_binary`]'s
/// answer — and only its `-1` falls through to [`is_binary`] on the bytes. A side
/// with no data is not binary unless its driver says so.
pub(crate) fn filespec_is_binary(driver: Option<bool>, data: &[u8]) -> bool {
    driver.unwrap_or_else(|| is_binary(data))
}

/// Total added and removed lines, for `--stat`. Uses the same hunk machinery as
/// the patch so the two can never disagree about what changed.
pub(crate) fn count_changed_lines(old: &[u8], new: &[u8]) -> Result<(usize, usize)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let counter = LineCounter {
        added: 0,
        deleted: 0,
    };
    Ok(UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3)).consume()?)
}

/// [`count_changed_lines`] with a whitespace rule applied to the *comparison* only,
/// the way `-w`/`-b`/`--ignore-space-at-eol` work: every line is normalized before
/// the two sides are matched up, so a line whose only change is whitespace pairs
/// with its counterpart and drops out of the tally.
///
/// The history commands need this because git computes their `--stat` counts from
/// the same pairs the patch would print — a commit whose whole diff is whitespace
/// leaves the queue empty under `-w`, and the stat, name and raw formats then have
/// nothing to report.
pub(crate) fn count_changed_lines_ws(
    old: &[u8],
    new: &[u8],
    ws: super::diff::Whitespace,
) -> Result<(usize, usize)> {
    if ws == super::diff::Whitespace::Keep {
        return count_changed_lines(old, new);
    }
    let norm = |data: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for line in super::diff::byte_lines(data) {
            out.extend_from_slice(&super::diff::normalize_line(line, ws));
            out.push(b'\n');
        }
        out
    };
    count_changed_lines(&norm(old), &norm(new))
}

/// Counts changed lines, ignoring context.
struct LineCounter {
    added: usize,
    deleted: usize,
}

impl ConsumeHunk for LineCounter {
    type Out = (usize, usize);

    fn consume_hunk(&mut self, _header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        for &(kind, _) in lines {
            match kind {
                DiffLineKind::Add => self.added += 1,
                DiffLineKind::Remove => self.deleted += 1,
                DiffLineKind::Context => {}
            }
        }
        Ok(())
    }

    fn finish(self) -> (usize, usize) {
        (self.added, self.deleted)
    }
}
