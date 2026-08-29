//! Reading the on-disk index the way git does: a missing file is an empty index.
//!
//! ```c
//! int repo_read_index(struct repository *repo)
//! {
//!         [...]
//!         return read_index_from(repo->index, repo->index_file, repo->gitdir);
//! }
//! ```
//!
//! which reaches `do_read_index(istate, path, 0)` — `must_exist == 0`
//! (read-cache.c:2216-2224). A repository that has never staged anything, every freshly
//! `init`'d and every bare one included, therefore hands each builtin an initialized but
//! empty index rather than an error.

/// The repository's index, or an empty one when the file does not exist.
///
/// The `Err` variant of `open_index` is large; boxing it would churn every call site.
#[allow(clippy::result_large_err)]
pub fn or_empty(repo: &gix::Repository) -> Result<gix::index::File, gix::worktree::open_index::Error> {
    match repo.open_index() {
        Ok(index) => Ok(index),
        Err(gix::worktree::open_index::Error::IndexFile(gix::index::file::init::Error::Io(err)))
            if err.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(gix::index::File::from_state(
                gix::index::State::new(repo.object_hash()),
                repo.index_path(),
            ))
        }
        Err(err) => Err(err),
    }
}
