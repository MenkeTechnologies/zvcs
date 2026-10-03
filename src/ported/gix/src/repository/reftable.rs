//! Operations of the `reftable` ref storage backend that have no counterpart in
//! the reference API shared with the `files` backend: listing, creating,
//! deleting and expiring reflogs, `optimize`, renaming and copying references
//! with their reflogs, and `fsck` (git's `refs_be_reftable` entries of the same
//! names, refs/reftable-backend.c, v2.56.0).
//!
//! Each fails with [`Error::NotReftable`] in a repository using another format;
//! callers branch on [`Repository::ref_storage()`](crate::Repository::ref_storage()).

use gix_ref::{
    FullName, FullNameRef,
    bstr::BStr,
    reftable::{Backend, ExpireFlags, ExpirePolicy, FsckReport},
    store::RefStorage,
};

/// The error returned by the reftable operations of a [`Repository`](crate::Repository).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The repository does not store its references in reftables.
    #[error("the repository does not use the reftable ref storage format")]
    NotReftable,
    /// The backend failed.
    #[error(transparent)]
    Backend(#[from] gix_ref::reftable::Error),
}

impl crate::Repository {
    /// The format this repository's references are stored in,
    /// `the_repository->ref_storage_format`.
    pub fn ref_storage(&self) -> RefStorage {
        self.refs.ref_storage()
    }

    fn reftable_backend(&self) -> Result<&Backend, Error> {
        self.refs.reftable().ok_or(Error::NotReftable)
    }

    /// The names of all references that have a reflog, in name order
    /// (`reftable_be_reflog_iterator_begin()`).
    pub fn reftable_reflog_names(&self) -> Result<Vec<FullName>, Error> {
        Ok(self.reftable_backend()?.reflog_names()?)
    }

    /// Create an empty reflog for `name` (`reftable_be_create_reflog()`).
    pub fn reftable_create_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        Ok(self.reftable_backend()?.create_reflog(name)?)
    }

    /// Delete the reflog of `name` (`reftable_be_delete_reflog()`).
    pub fn reftable_delete_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        Ok(self.reftable_backend()?.delete_reflog(name)?)
    }

    /// Expire the reflog entries of `name` that `policy` prunes
    /// (`reftable_be_reflog_expire()`).
    pub fn reftable_reflog_expire(
        &self,
        name: &FullNameRef,
        flags: ExpireFlags,
        policy: &mut dyn ExpirePolicy,
    ) -> Result<(), Error> {
        Ok(self.reftable_backend()?.reflog_expire(name, flags, policy)?)
    }

    /// Compact the stacks, only as far as needed with `auto`
    /// (`reftable_be_optimize()`, `REFS_OPTIMIZE_AUTO`).
    pub fn reftable_optimize(&self, auto: bool) -> Result<(), Error> {
        Ok(self.reftable_backend()?.optimize(auto)?)
    }

    /// Whether [`reftable_optimize()`](Self::reftable_optimize()) would do
    /// anything (`reftable_be_optimize_required()`).
    pub fn reftable_optimize_required(&self, auto: bool) -> Result<bool, Error> {
        Ok(self.reftable_backend()?.optimize_required(auto)?)
    }

    /// Move `old` with its reflog to `new`, logging `logmsg` as `committer`
    /// (`reftable_be_rename_ref()`).
    pub fn reftable_rename_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        Ok(self.reftable_backend()?.rename_ref(old, new, committer, logmsg)?)
    }

    /// Copy `old` with its reflog to `new`, logging `logmsg` as `committer`
    /// (`reftable_be_copy_ref()`).
    pub fn reftable_copy_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        Ok(self.reftable_backend()?.copy_ref(old, new, committer, logmsg)?)
    }

    /// Check the stack of `worktree` (`None`: the main worktree) and its
    /// references (`reftable_be_fsck()`), passing each problem to `report` and
    /// each progress message to `verbose`. `Ok(true)` if errors were found.
    pub fn reftable_fsck(
        &self,
        worktree: Option<&BStr>,
        report: &mut dyn FnMut(FsckReport<'_>) -> i32,
        verbose: &mut dyn FnMut(&str),
    ) -> Result<bool, Error> {
        Ok(self.reftable_backend()?.fsck(worktree, report, verbose)?)
    }
}
