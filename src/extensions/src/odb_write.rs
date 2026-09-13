//! `odb_write_object()` for the primary loose source, with git's early return for
//! an object the database already holds.
//!
//! `odb_source_loose_write_object()` (odb/source-loose.c:585-625) hashes the
//! buffer first (`write_object_file_prepare()`, :617) and only then asks
//! `odb_freshen_object()` (odb.c:826-835) whether any source already has it. A
//! yes returns success without touching the directory at all, which is why a
//! `write-tree` over an index whose trees exist succeeds against a read-only
//! `.git/objects`. Only a no reaches `write_loose_object()`, whose temporary file
//! is the first thing that can fail (`start_loose_object_common()`,
//! object-file.c:667-677).
//!
//! The object id is known before the write is attempted, and git's callers keep
//! it even when the write fails: `try_threeway()` (apply.c:3749-3751) ignores the
//! return value and carries on with the id. [`WriteFailure`] therefore carries
//! the id alongside the `error()` text git printed.

use std::path::{Path, PathBuf};

use gix::ObjectId;

/// A write `odb_write_object()` reported as `-1`.
#[derive(Debug)]
pub struct WriteFailure {
    /// The id `write_object_file_prepare()` computed before the write failed.
    pub id: ObjectId,
    /// The `error()` message git printed, without its `error: ` prefix.
    pub message: String,
}

impl std::fmt::Display for WriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WriteFailure {}

/// `odb_write_object(odb, buf, len, type, &oid)` into `repo`'s primary source.
pub fn write_object(
    repo: &gix::Repository,
    kind: gix::object::Kind,
    buf: &[u8],
) -> Result<ObjectId, WriteFailure> {
    let id = gix::objs::compute_hash(repo.object_hash(), kind, buf).map_err(|e| WriteFailure {
        id: ObjectId::null(repo.object_hash()),
        message: e.to_string(),
    })?;
    // "Normally if we have it in the pack then we do not bother writing it out
    // into .git/objects/??/?{38} file." (odb/source-loose.c:614-619)
    if freshen_object(repo, &id) {
        return Ok(id);
    }
    use gix::objs::Write;
    repo.objects
        .write_buf_with_known_id(kind, buf, id)
        .map_err(|err| WriteFailure {
            id,
            message: write_error_message(repo, &err),
        })
}

/// The `error()` git prints for a failed loose write.
///
/// A temporary file that cannot be created is `start_loose_object_common()`'s
/// pair (object-file.c:667-677): `EACCES` names the database, anything else is
/// `error_errno("unable to create temporary file")`. Every other failure keeps
/// gitoxide's wording, as the rest of the port does for write errors.
fn write_error_message(repo: &gix::Repository, err: &gix::objs::write::Error) -> String {
    use gix::odb::loose::write::Error as LooseError;
    // The loose store boxes its tempfile error before `?` boxes it again.
    let loose = err
        .downcast_ref::<LooseError>()
        .or_else(|| err.downcast_ref::<Box<LooseError>>().map(AsRef::as_ref));
    if let Some(LooseError::Io {
        source: gix::hash::io::Error::Io(io),
        message: "create named temp file in",
        ..
    }) = loose
    {
        return if io.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "insufficient permission for adding an object to repository database {}",
                objdir_display(repo)
            )
        } else {
            format!("unable to create temporary file: {}", crate::external::strerror(io))
        };
    }
    err.to_string()
}

/// `odb_freshen_object()` (odb.c:826-835): the first source that has `id` and can
/// bump its mtime answers yes. Each files source tries its packs before its loose
/// objects (`odb_source_files_freshen_object()`, odb/source-files.c:151-158).
///
/// The empty tree is not special-cased: git's in-memory source only answers for
/// objects cached into it (`odb_source_inmemory_freshen_object()`,
/// odb/source-inmemory.c:297-304), so writing it still goes to disk.
fn freshen_object(repo: &gix::Repository, id: &gix::hash::oid) -> bool {
    use gix::objs::Exists;
    // Nothing to freshen in any source; skip the per-source walk on the hot path
    // of writing a genuinely new object.
    if !repo.objects.exists(id) {
        return false;
    }
    let store = repo.objects.store_ref();
    let mut sources = vec![store.path().to_path_buf()];
    sources.extend(store.alternate_db_paths().unwrap_or_default());
    sources
        .iter()
        .any(|objdir| freshen_packed_object(objdir, id, repo.object_hash()) || freshen_loose_object(objdir, id))
}

/// `odb_source_loose_freshen_object()` (odb/source-loose.c:576-583) →
/// `check_and_freshen_file(path, 1)` (object-file.c:81-88).
fn freshen_loose_object(objdir: &Path, id: &gix::hash::oid) -> bool {
    let hex = id.to_hex().to_string();
    check_and_freshen_file(&objdir.join(&hex[..2]).join(&hex[2..]))
}

/// `packfile_store_freshen_object()` (packfile.c:2172-2186): locate the pack the
/// store would read `id` from, refuse a cruft pack, and bump the `.pack`'s mtime.
fn freshen_packed_object(objdir: &Path, id: &gix::hash::oid, hash: gix::hash::Kind) -> bool {
    let Some(pack) = find_pack_entry(objdir, id, hash) else {
        return false;
    };
    // `p->is_cruft` is set when the `.mtimes` sibling exists (packfile.c:836-838).
    if pack.with_extension("mtimes").exists() {
        return false;
    }
    utime_now(&pack)
}

/// `find_pack_entry()` (packfile.c:2149-2170) over one source, returning the
/// `.pack` path.
///
/// The multi-pack index answers first (`fill_midx_entry()`, midx.c:592-628, which
/// only accepts a pack that is still a regular file). Packs it covers never enter
/// the store's list (`prepare_pack()`, packfile.c:998-1005); the rest are searched
/// youngest first, the order `sort_pack()` (packfile.c:1044-1069) gives packs of a
/// single source, whose `pack_local` flags are all equal.
fn find_pack_entry(objdir: &Path, id: &gix::hash::oid, hash: gix::hash::Kind) -> Option<PathBuf> {
    let pack_dir = objdir.join("pack");
    let midx = gix::odb::pack::multi_index::File::at(pack_dir.join("multi-pack-index"), None).ok();
    if let Some(midx) = &midx {
        if let Some(entry) = midx.lookup(id) {
            let (pack_id, _) = midx.pack_id_and_pack_offset_at_index(entry);
            let pack = pack_dir
                .join(&midx.index_names()[pack_id as usize])
                .with_extension("pack");
            if std::fs::metadata(&pack).is_ok_and(|m| m.is_file()) {
                return Some(pack);
            }
        }
    }

    let mut packs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&pack_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = PathBuf::from(entry.file_name());
            if name.extension().is_none_or(|ext| ext != "idx") {
                return None;
            }
            if midx.as_ref().is_some_and(|m| m.index_names().contains(&name)) {
                return None;
            }
            let pack = pack_dir.join(&name).with_extension("pack");
            // `add_packed_git()` drops a pack whose `.pack` is not a regular file
            // (packfile.c:840-844); its mtime is the sort key.
            let md = std::fs::metadata(&pack).ok().filter(|m| m.is_file())?;
            Some((md.modified().ok()?, pack))
        })
        .collect();
    packs.sort_by(|a, b| b.0.cmp(&a.0));
    packs.into_iter().map(|(_, pack)| pack).find(|pack| {
        gix::odb::pack::index::File::at(pack.with_extension("idx"), hash)
            .is_ok_and(|idx| idx.lookup(id).is_some())
    })
}

/// `check_and_freshen_file(fn, 1)` (object-file.c:81-88): present and its mtime
/// could be set to now.
fn check_and_freshen_file(path: &Path) -> bool {
    path.exists() && utime_now(path)
}

/// `freshen_file()` (object-file.c:68-72): `!utime(fn, NULL)`.
fn utime_now(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string, and a null `times` asks
    // for the current time, exactly as git's call does.
    unsafe { libc::utime(c_path.as_ptr(), std::ptr::null()) == 0 }
}

/// `loose->base.path` as git spells it: `git_dir + "/objects"`, which after
/// `setup_git_directory_gently()` has chdir'd to the top of the work tree is
/// `.git/objects` from any subdirectory, and `./objects` in a bare repository
/// entered at its git directory. Anything else (`GIT_DIR`, `GIT_OBJECT_DIRECTORY`,
/// a linked worktree) keeps the path as it was discovered.
fn objdir_display(repo: &gix::Repository) -> String {
    let objdir = repo.objects.store_ref().path();
    let Ok(cwd) = std::env::current_dir() else {
        return objdir.display().to_string();
    };
    // `gix` hands back the path as it discovered it — `./.git/objects` at the top,
    // `../.git/objects` below it — so compare against the directory git chdir'd to.
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let base = canon(&repo.workdir().map_or_else(|| cwd.clone(), |w| cwd.join(w)));
    let Ok(rel) = canon(&cwd.join(objdir)).strip_prefix(&base).map(Path::to_path_buf) else {
        return objdir.display().to_string();
    };
    match rel.to_str() {
        Some("objects") => "./objects".to_string(),
        Some(".git/objects") => ".git/objects".to_string(),
        _ => objdir.display().to_string(),
    }
}
