//! `tmp-objdir.c` — a scratch object directory that becomes the process's primary
//! object store, so objects a command has to *create in order to look at* never
//! reach the repository it is reading.
//!
//! git's only in-tree user of the "replace the primary ODB" half of this file is
//! `--remerge-diff`: `do_remerge_diff()` re-runs the recorded merge to get a tree
//! to diff against, and that re-run writes blobs and trees. Those objects are
//! wanted for the length of one diff and are garbage afterwards, so
//! `do_remerge_diff()` opens one of these lazily and hands it the writes
//! (log-tree.c:1044-1049, git v2.55.0):
//!
//! ```c
//!     if (opt->remerge_diff && !opt->remerge_objdir) {
//!             opt->remerge_objdir = tmp_objdir_create(the_repository, "remerge-diff");
//!             if (!opt->remerge_objdir)
//!                     return error(_("unable to create temporary object directory"));
//!             tmp_objdir_replace_primary_odb(opt->remerge_objdir, 1);
//!     }
//! ```
//!
//! ### What is ported, and what the port substitutes
//!
//! * **The directory.** `tmp_objdir_create()` (tmp-objdir.c:135-184) `mkdtemp`s
//!   `<objects>/tmp_objdir-<prefix>-XXXXXX` — *inside* the real object directory,
//!   deliberately, so `builtin/prune.c` can recognize and sweep one left behind by
//!   a crash — and `setup_tmp_objdir()` (tmp-objdir.c:123-133) creates its `pack`
//!   subdirectory. [`TmpObjdir::create`] does both, with the same name.
//! * **The swap.** `tmp_objdir_replace_primary_odb()` (tmp-objdir.c:330-337) calls
//!   `odb_set_temporary_primary_source()`, which links the *old* primary in as the
//!   first alternate (odb.c:239-253):
//!
//!   ```c
//!     /*
//!      * Make a new primary odb and link the old primary ODB in as an
//!      * alternate
//!      */
//!     source = odb_source_new(odb, dir, false);
//!   ```
//!
//!   gitoxide has no in-memory source list to splice, so the port states the same
//!   relationship the way the object store already understands it: an
//!   `info/alternates` inside the scratch directory naming the real object
//!   directory. `gix_odb::Store` resolves that file when it loads its index
//!   (`gix-odb/src/store_impls/dynamic/load_index.rs:234`), and its `Write`
//!   implementation writes to `loose_dbs[0]` — the primary, i.e. the scratch
//!   directory (`gix-odb/src/store_impls/dynamic/write.rs:31-42`). Reads therefore
//!   see both stores and writes reach only the scratch one, which is exactly the
//!   arrangement `odb_set_temporary_primary_source()` builds. The file lives and
//!   dies inside the scratch directory, so the real store is not touched to make
//!   it.
//! * **The discard.** `tmp_objdir_discard_objects()` (tmp-objdir.c:81-84) empties
//!   the directory but keeps it, which is what `do_remerge_diff()` calls after each
//!   merge so one commit's scratch objects cannot be seen by the next
//!   (log-tree.c:1087). [`TmpObjdir::discard_objects`] does that, and re-lays the
//!   `pack` directory and the `info/alternates` the emptying removed — the two
//!   pieces git keeps because for it they were never files in the first place.
//! * **The teardown.** `tmp_objdir_destroy()` (tmp-objdir.c:55-74) restores the
//!   previous primary and `remove_dir_recursively()`s the directory, and git
//!   installs it with `atexit()` so a `die()` on the error path still sweeps
//!   (tmp-objdir.c:167-170). Here that is [`Drop`], which runs on the error path
//!   because `?` unwinds through it.
//!
//! `tmp_objdir_migrate()`, `tmp_objdir_env()` and `tmp_objdir_add_as_alternate()`
//! — the quarantine half of the file, used by `receive-pack` to *keep* what a
//! scratch directory collected — are not ported: nothing in this tree has a
//! quarantine yet, and a migrate with no caller would be untested code.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A scratch object directory that is the primary store of [`Self::repo`], with
/// the repository's real object directory behind it as an alternate.
///
/// Dropping it removes the directory and everything written into it.
pub struct TmpObjdir {
    /// `t->path`: `<objects>/tmp_objdir-<prefix>-XXXXXX`.
    path: PathBuf,
    /// The real object directory, which is the alternate.
    alternate: PathBuf,
    /// A clone of the caller's repository whose object database is `path`.
    ///
    /// git swaps the source list inside the one `struct repository`, so *every*
    /// later write in the process lands in the scratch directory. Nothing in
    /// gitoxide can be swapped underneath an existing `Repository`, so the port
    /// scopes the swap to a clone: a caller writes through this handle for as long
    /// as it wants the redirection and keeps using its own outside of that. The
    /// two share every other resource, `Repository::clone` being a handle clone.
    repo: gix::Repository,
}

impl TmpObjdir {
    /// `tmp_objdir_create(r, prefix)` followed immediately by
    /// `tmp_objdir_replace_primary_odb(t, 1)` — the pair every caller of the
    /// primary-ODB half makes, and the only pairing this port offers, since a
    /// scratch directory nothing writes to has no use here.
    ///
    /// `will_destroy` is implicitly 1: this port has no migrate, so the contents
    /// are always thrown away.
    pub fn create(repo: &gix::Repository, prefix: &str) -> Result<TmpObjdir> {
        let alternate = repo.objects.store_ref().path().to_owned();
        let alternate = gix::path::realpath(&alternate).unwrap_or(alternate);
        let path = mkdtemp(&alternate, prefix)?;

        // `setup_tmp_objdir()` (tmp-objdir.c:123-133).
        std::fs::create_dir(path.join("pack"))
            .with_context(|| format!("creating {}/pack", path.display()))?;
        write_alternates(&path, &alternate)?;

        // The scratch store, with the real one behind it. `Slots::Given(1)` would be
        // wrong: the alternate brings its own packs, and the slot map has to hold
        // them, so the defaults the repository itself was opened with are used.
        let handle = gix::odb::at_opts(
            path.clone(),
            Vec::new(),
            gix::odb::store::init::Options {
                object_hash: repo.object_hash(),
                ..Default::default()
            },
        )
        .with_context(|| format!("opening temporary object directory {}", path.display()))?;
        let mut scratch = repo.clone();
        // `.with_write_passthrough()`: gitoxide's proxy would otherwise buffer writes
        // in memory, which is a *third* store and not the one git redirects into.
        // This is how `gix::Repository` builds its own handle
        // (`gix/src/repository/impls.rs:53`).
        scratch.objects = gix::odb::memory::Proxy::from(handle).with_write_passthrough();

        Ok(TmpObjdir {
            path,
            alternate,
            repo: scratch,
        })
    }

    /// The repository whose writes land in the scratch directory. Reads see the
    /// real object store too, through the alternate.
    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// `tmp_objdir_discard_objects()` (tmp-objdir.c:81-84): empty the directory,
    /// keeping the directory itself.
    ///
    /// The `pack` directory and `info/alternates` go with the emptying and are
    /// re-laid, because in git neither is a file: `pack` is recreated only at
    /// create time because nothing in the scratch directory ever removes it, and
    /// the alternate link is a pointer in `odb->sources` that an unlink cannot
    /// reach. Losing the link would cost the scratch store its view of the real
    /// one the next time `gix_odb` reloaded its index.
    pub fn discard_objects(&self) -> Result<()> {
        for entry in std::fs::read_dir(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .with_context(|| format!("removing {}", path.display()))?;
        }
        std::fs::create_dir(self.path.join("pack"))?;
        write_alternates(&self.path, &self.alternate)
    }
}

impl Drop for TmpObjdir {
    /// `tmp_objdir_destroy()` (tmp-objdir.c:55-74), whose `remove_dir_recursively()`
    /// is the whole reason a failed `--remerge-diff` leaves no trace. git reaches it
    /// from an `atexit()` handler so that a `die()` sweeps too; here the unwinding
    /// `?` does the same job.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `objects/info/alternates` naming `alternate`, the port's spelling of
/// `odb_set_temporary_primary_source()`'s "link the old primary ODB in as an
/// alternate" (odb.c:239-243).
fn write_alternates(root: &Path, alternate: &Path) -> Result<()> {
    let info = root.join("info");
    if !info.exists() {
        std::fs::create_dir(&info).with_context(|| format!("creating {}", info.display()))?;
    }
    let mut line = gix::path::into_bstr(alternate).into_owned();
    line.push(b'\n');
    std::fs::write(info.join("alternates"), &line)
        .with_context(|| format!("writing {}/alternates", info.display()))
}

/// `mkdtemp("<objects>/tmp_objdir-<prefix>-XXXXXX")` (tmp-objdir.c:154-164).
///
/// The name is git's verbatim, including the `tmp_` that `builtin/prune.c`
/// recognizes when it sweeps a directory a crashed process left behind
/// (tmp-objdir.c:149-153), so a repository this port crashed in is cleanable by
/// stock git.
fn mkdtemp(objects: &Path, prefix: &str) -> Result<PathBuf> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (u64::from(std::process::id()) << 32);
    let mut last = None;
    // `mkdtemp` itself retries a bounded number of times before reporting EEXIST.
    for _ in 0..256 {
        let mut suffix = String::with_capacity(6);
        for _ in 0..6 {
            // xorshift64: a name generator, not a security primitive — the same role
            // `mkdtemp`'s own counter plays.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            suffix.push(ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char);
        }
        let candidate = objects.join(format!("tmp_objdir-{prefix}-{suffix}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                last = Some((candidate, e));
                break;
            }
        }
    }
    match last {
        Some((path, e)) => Err(anyhow::Error::new(e)
            .context(format!("creating temporary object directory {}", path.display()))),
        None => anyhow::bail!(
            "unable to create temporary object directory under {}",
            objects.display()
        ),
    }
}
