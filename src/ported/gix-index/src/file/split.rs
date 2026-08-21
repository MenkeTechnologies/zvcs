//! Writing a *split* index — git's `write_shared_index()` and
//! `write_split_index()` (read-cache.c:2338-2382, split-index.c:214-352).
//!
//! A split index is two files. `$GIT_DIR/sharedindex.<id>` holds the entries and
//! nothing else — `do_write_index(si->base, ...)` runs on a bare `index_state`
//! that only ever received a copy of `cache[]`, so it carries no tree-cache and
//! no other extension. `$GIT_DIR/index` holds whatever entries are *not* in the
//! shared half plus the `link` extension naming it, and keeps the tree-cache and
//! everything else the index had.
//!
//! What this writes is the shape `prepare_to_write_split_index()` produces when
//! nothing has diverged from the shared half yet: every entry moves across, the
//! split half is left empty, and both bitmaps are empty. git can and does write a
//! denser variant — an entry whose path is empty standing in for a shared entry
//! whose stat data was refreshed, with the corresponding bit set in the replace
//! bitmap (split-index.c:223-309) — and reading that back is what
//! [`Link::dissolve_into()`](crate::extension::Link) has always handled. Both
//! decode to exactly the same set of entries; this one simply never claims a
//! replacement it did not make.

use std::path::{Path, PathBuf};

use crate::{File, extension, write};

/// The error produced by [`File::write_split()`].
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

impl File {
    /// Write this index split in two under `git_dir`: a `sharedindex.<id>` file
    /// holding every entry, and this index — emptied of entries and carrying a
    /// `link` extension that names it — at its own path.
    ///
    /// Returns the id of the shared index, which is its trailing checksum and the
    /// name it is stored under, exactly as git's `si->base_oid` is
    /// `si->base->oid` (read-cache.c:2371).
    ///
    /// The in-memory state is left holding the `link` extension and no entries,
    /// which is what was written; re-reading the file through
    /// [`File::at_with_git_dir()`](File::at_with_git_dir()) dissolves it back into
    /// the full entry list.
    ///
    /// ### The shared half is never written with `index.skipHash`
    ///
    /// `link` refers to the shared index *by its checksum* and the reader verifies
    /// it (`expected_checksum`), so a shared index with a zeroed trailer could not
    /// be found, let alone verified. `skip_hash` therefore applies only to the
    /// split half, which is the one git's own `write_shared_index()` hashes
    /// unconditionally to get a name for.
    pub fn write_split(&mut self, git_dir: &Path, options: write::Options) -> Result<gix_hash::ObjectId, Error> {
        // `move_cache_to_base_index()` (split-index.c:102-132): the entries become
        // the shared index's, and the split index starts out with none of its own.
        let shared_id = write_shared(self, git_dir)?;

        let mut entries = Vec::new();
        std::mem::swap(&mut self.state.entries, &mut entries);
        // `si->delete_bitmap = ewah_new(); si->replace_bitmap = ewah_new();`
        // (split-index.c:220-221) — present and empty, as git writes them whenever
        // there is a base to point at.
        let empty = gix_bitmap::ewah::Vec::from_bits(&[]).expect("an empty bitmap always fits");
        self.state.set_link(Some(extension::Link {
            shared_index_checksum: shared_id,
            bitmaps: Some(extension::link::Bitmaps {
                delete: empty.clone(),
                replace: empty,
            }),
        }));

        let result = self.write(options);
        if result.is_err() {
            // Put the entries back so the caller's state still describes the
            // repository if the split half could not be committed.
            self.state.entries = entries;
            self.state.set_link(None);
        }
        result?;
        Ok(shared_id)
    }
}

/// git's `write_shared_index()` (read-cache.c:2338-2382): serialize every entry to
/// a temporary file in `git_dir`, then rename it to the `sharedindex.<checksum>`
/// its own trailer names.
///
/// `do_write_index(si->base, *temp, flags)` runs against an `index_state` that
/// holds nothing but `cache[]`, so no optional extension is written — which is why
/// [`write::Extensions::None`] is not a simplification here but the port.
fn write_shared(file: &File, git_dir: &Path) -> Result<gix_hash::ObjectId, Error> {
    let mut buf = Vec::new();
    let (_version, id) = file.write_to(
        &mut buf,
        write::Options {
            extensions: write::Extensions::None,
            skip_hash: false,
        },
    )?;

    std::fs::create_dir_all(git_dir)?;
    let final_path = shared_index_path(git_dir, id);
    // `mks_tempfile_sm(repo_git_path(the_repository, "sharedindex_XXXXXX"), 0, 0666)`
    // (read-cache.c:2362) followed by `rename_tempfile()` (read-cache.c:2377): the
    // file only ever appears under its final name complete, so a concurrent reader
    // resolving `link` either finds nothing or finds all of it.
    let temp = git_dir.join(format!("sharedindex_{id}.tmp"));
    std::fs::write(&temp, &buf)?;
    if let Err(err) = std::fs::rename(&temp, &final_path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(id)
}

/// `xstrfmt("%s/sharedindex.%s", gitdir, oid_to_hex(id))` (read-cache.c:1893).
pub fn shared_index_path(git_dir: &Path, id: gix_hash::ObjectId) -> PathBuf {
    git_dir.join(format!("sharedindex.{id}"))
}
