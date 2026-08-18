//! The three worktree-side rules `diff-lib.c` and `wt-status.c` share when they turn
//! an index entry into a diff filespec, kept in one place so the callers cannot drift.
//!
//! gix's index↔worktree comparison answers a slightly different question than git's:
//! it reports `Change::Removed` for *any* non-submodule entry whose path `lstat`s as a
//! directory (`gix-status/src/index_as_worktree/function.rs:379-394`), while
//! `check_removed()` (diff-lib.c:42) first asks whether that directory is a repository.
//! Every caller that consumes gix's verdict has to re-ask, so the lookup lives here:
//!
//! * [`gitlink_head`] — `repo_resolve_gitlink_ref()` (refs.c:2283).
//! * [`removed_became_gitlink`] — the `S_ISDIR(st->st_mode)` arm of `check_removed()`.
//! * [`set_absent_resource`] — the *invalid filespec* the blob platform has no state
//!   for, needed by every caller that renders a patch for a vanished path.
//! * [`subproject_image`] — `diff_populate_gitlink()` (diff.c:4475).
//!
//! [`super::diff`] carries its own copies of the last two, shaped around its private
//! `NewSide` enum; they should fold into this module the next time that file is
//! touched.

use anyhow::Result;
use gix::bstr::BStr;
use gix::diff::blob::ResourceKind;
use gix::objs::tree::EntryKind;
use gix::ObjectId;
use std::path::Path;

/// `repo_resolve_gitlink_ref(r, <rela_path>, "HEAD", &sub)` (refs.c:2283): the commit
/// the repository at `workdir/rela_path` has checked out, or `None` when the path is
/// not a repository at all or its `HEAD` is unborn.
///
/// The C returns `-1` both when `repo_get_submodule_ref_store()` finds no ref store and
/// when the resolved id is null, and `check_removed()` treats the two identically — so
/// one `Option` covers both.
pub(crate) fn gitlink_head(workdir: &Path, rela_path: &BStr) -> Option<ObjectId> {
    let abs = workdir.join(gix::path::from_bstr(rela_path).as_ref());
    gix::open(&abs)
        .ok()?
        .head_id()
        .ok()
        .map(gix::Id::detach)
        .filter(|id| !id.is_null())
}

/// `check_removed()`'s directory arm (diff-lib.c:58):
///
/// ```c
/// if (S_ISDIR(st->st_mode)) {
///         struct object_id sub;
///         if (!S_ISGITLINK(ce->ce_mode) &&
///             repo_resolve_gitlink_ref(the_repository, ce->name, "HEAD", &sub))
///                 return 1;
/// }
/// return 0;
/// ```
///
/// gix has already decided the entry is `Change::Removed`; this re-asks git's question
/// for the one case where the two disagree. `Some(head)` means the vanished blob's name
/// was taken by a checked-out repository, so git reports a *type change* to `160000`
/// rather than a deletion — `T` in `--raw`, `mode change 100644 => 160000` in
/// `--summary`, `.T` with the `SC` submodule field in `status --porcelain=v2`. `None`
/// means the removal stands.
///
/// The `lstat` is deliberate: a symlink pointing at a repository is `S_IFLNK`, not
/// `S_IFDIR`, so the C never reaches the gitlink lookup for it — and neither does gix,
/// which uses `Metadata::from_path_no_follow` for the same decision.
pub(crate) fn removed_became_gitlink(workdir: &Path, rela_path: &BStr) -> Option<ObjectId> {
    let abs = workdir.join(gix::path::from_bstr(rela_path).as_ref());
    if !std::fs::symlink_metadata(&abs).is_ok_and(|md| md.is_dir()) {
        return None;
    }
    gitlink_head(workdir, rela_path)
}

/// `diff_populate_gitlink()` (diff.c:4475): the one-line image git gives a gitlink
/// filespec, with `-dirty` glued on whenever any `dirty_submodule` bit is set.
pub(crate) fn subproject_image(id: ObjectId, dirty: bool) -> Vec<u8> {
    let mut v = b"Subproject commit ".to_vec();
    v.extend_from_slice(id.to_hex().to_string().as_bytes());
    if dirty {
        v.extend_from_slice(b"-dirty");
    }
    v.push(b'\n');
    v
}

/// Hand the blob platform git's *invalid filespec*: the side of a pair that does not
/// exist.
///
/// `diff_populate_filespec()` (diff.c:4062) returns immediately for a filespec whose
/// `oid_valid` and `is_stdin` are both clear and whose mode is zero — an absent side
/// never reaches the worktree, whichever side of the pair it is on. The blob platform
/// has no such state: it decides between "read this path off disk" and "resolve this id
/// in the odb" purely by whether a `WorktreeRoots` entry covers the side
/// (`gix-diff/src/blob/pipeline.rs:271`), and a null id only *looks* absent under a root
/// because the file is normally gone.
///
/// Two shapes break that assumption, and both are ordinary deletions to git:
///
/// * something else has taken the name — `rm f && mkdir f` — and the read fails with
///   `Is a directory (os error 21)`, killing the whole diff;
/// * the name is still readable through a *symlinked leading path* — `rm -rf d &&
///   ln -s elsewhere d` with `elsewhere/f` present — and the read silently succeeds,
///   so the deletion renders with a header and no hunk.
///
/// So the root is lifted for the one call. With `roots.by_kind(kind)` `None`, the
/// pipeline's `id.is_null()` arm (`pipeline.rs:399`) reports no data at all, which is
/// exactly the empty filespec. The lift also moves the platform's cache key from the
/// path to the (null) id, so an absent side shares one entry instead of shadowing the
/// worktree entry for that path.
pub(crate) fn set_absent_resource(
    cache: &mut gix::diff::blob::Platform,
    kind: ResourceKind,
    mode: EntryKind,
    rela_path: &BStr,
    objects: &gix::OdbHandle,
    null: ObjectId,
) -> Result<()> {
    let root = match kind {
        ResourceKind::OldOrSource => cache.filter.roots.old_root.take(),
        ResourceKind::NewOrDestination => cache.filter.roots.new_root.take(),
    };
    let res = cache.set_resource(null, mode, rela_path, kind, objects);
    match kind {
        ResourceKind::OldOrSource => cache.filter.roots.old_root = root,
        ResourceKind::NewOrDestination => cache.filter.roots.new_root = root,
    }
    res?;
    Ok(())
}
