use crate::{File, Version, write};

/// The error produced by [`File::write()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] gix_hash::io::Error),
    #[error("Could not acquire lock for index file")]
    AcquireLock(#[from] gix_lock::acquire::Error),
    #[error("Could not commit lock for index file")]
    CommitLock(#[from] gix_lock::commit::Error<gix_lock::File>),
}

impl File {
    /// Write the index to `out` with `options`, to be readable by [`File::at()`], returning the version that was actually written
    /// to retain all information of this index.
    ///
    /// Note that the `tree` (tree-cache) extension is written as-is and is **not** recomputed or
    /// invalidated to match the current entries; see [`File::write()`] for the implications and for
    /// the two ways to keep it honest.
    pub fn write_to(
        &self,
        mut out: impl std::io::Write,
        options: write::Options,
    ) -> Result<(Version, gix_hash::ObjectId), gix_hash::io::Error> {
        let _span = gix_features::trace::detail!("gix_index::File::write_to()", skip_hash = options.skip_hash);
        let (version, hash) = if options.skip_hash {
            let out: &mut dyn std::io::Write = &mut out;
            let version = self.state.write_to(out, options)?;
            (version, self.state.object_hash.null())
        } else {
            let mut hasher = gix_hash::io::Write::new(&mut out, self.state.object_hash);
            let out: &mut dyn std::io::Write = &mut hasher;
            let version = self.state.write_to(out, options)?;
            (version, hasher.hash.try_finalize()?)
        };
        out.write_all(hash.as_slice())?;
        Ok((version, hash))
    }

    /// Write ourselves to the path we were read from after acquiring a lock, using `options`.
    ///
    /// Note that the hash produced will be stored which is why we need to be mutable.
    ///
    /// ### The `tree` (tree-cache) extension is written as-is
    ///
    /// The `tree` extension (tree-cache) is serialized from its current in-memory state; this
    /// function does **not** recompute or invalidate it to match the entries. So if entries were
    /// modified since the index was read and nothing was done about it, the tree-cache is written
    /// back still marked valid even though it is now stale.
    ///
    /// Git uses the tree-cache to skip unchanged directories when building a tree (on `git commit` /
    /// `git write-tree`), so a stale-but-valid tree-cache can make a later commit capture outdated
    /// subtree content; more generally, `git status` and later commits can disagree about what is
    /// staged.
    ///
    /// Keeping it honest is the caller's job, and is exactly what git does at each of its own entry
    /// mutations. Either maintain it — invalidate every path you touched, and recompute before the
    /// write:
    ///
    /// ```ignore
    /// index.invalidate_path_in_tree(path);            // cache_tree_invalidate_path()
    /// index.cache_tree_update(&odb, Default::default())?;  // cache_tree_update()
    /// index.write(gix_index::write::Options::default())?;
    /// ```
    ///
    /// — or throw it away, which costs the next reader a recomputation and nothing else:
    ///
    /// ```ignore
    /// index.remove_tree();
    /// index.write(gix_index::write::Options::default())?;
    /// ```
    ///
    /// See [`State::invalidate_path_in_tree()`](crate::State::invalidate_path_in_tree()),
    /// [`State::cache_tree_update()`](crate::State::cache_tree_update()) and
    /// [`State::prime_cache_tree()`](crate::State::prime_cache_tree()); upstream tracks the
    /// automatic version as [issue #2421].
    ///
    /// [issue #2421]: https://github.com/GitoxideLabs/gitoxide/issues/2421
    pub fn write(&mut self, options: write::Options) -> Result<(), Error> {
        let _span = gix_features::trace::detail!("gix_index::File::write()", path = ?self.path);
        let mut lock = std::io::BufWriter::with_capacity(
            64 * 1024,
            gix_lock::File::acquire_to_update_resource(&self.path, gix_lock::acquire::Fail::Immediately, None)?,
        );
        let (version, digest) = self.write_to(&mut lock, options)?;
        match lock.into_inner() {
            Ok(lock) => lock.commit()?,
            Err(err) => return Err(Error::Io(err.into_error().into())),
        };
        self.state.version = version;
        self.checksum = Some(digest);
        Ok(())
    }
}
