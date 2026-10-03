//! Maintenance of reflogs and stacks: reflog creation, deletion and expiry,
//! `optimize`, rename/copy and fsck (refs/reftable-backend.c:1699-2055,
//! 2368-2860), plus laying a reftable store down on disk and removing it
//! (refs/reftable-backend.c:497-559).
//!
//! These are public as the repository level calls them directly; the file
//! store has no equivalent operations to dispatch from.

use std::path::Path;

use gix_object::bstr::BStr;

use super::{Backend, Error};
use crate::FullNameRef;

/// `enum expire_reflog_flags` (refs.h:1134-1138).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExpireFlags {
    /// `EXPIRE_REFLOGS_DRY_RUN`: decide, but change nothing.
    pub dry_run: bool,
    /// `EXPIRE_REFLOGS_UPDATE_REF`: point the reference at the newest kept entry.
    pub update_ref: bool,
    /// `EXPIRE_REFLOGS_REWRITE`: chain the old value of each kept entry to the
    /// new value of the one before it.
    pub rewrite: bool,
}

/// The callbacks of `refs_reflog_expire()` (refs.h:1140-1164) deciding which
/// entries go.
pub trait ExpirePolicy {
    /// `reflog_expiry_prepare_fn`: called once with the reference and its
    /// current value before any entry is looked at.
    fn prepare(&mut self, refname: &FullNameRef, oid: &gix_hash::oid);
    /// `reflog_expiry_should_prune_fn`: whether `entry` is to be expired.
    fn should_prune(&mut self, entry: &crate::log::Line) -> bool;
    /// `reflog_expiry_cleanup_fn`: called once after the last entry.
    fn cleanup(&mut self);
}

/// One problem `fsck` found, what `fsck_report_ref()` receives.
#[derive(Debug, Clone)]
pub struct FsckReport<'a> {
    /// `report.path`: the offending table or reference.
    pub path: &'a BStr,
    /// The camel-cased `fsck_msg_id`, like `badReftableTableName`.
    pub msg_id: &'static str,
    /// The message, without the path and id.
    pub message: String,
}

impl Backend {
    /// `reftable_be_create_on_disk()` (refs/reftable-backend.c:497-511) with
    /// `refs_create_refdir_stubs()` (refs.c:2199-2220): lay down an empty
    /// reftable store in `git_dir`.
    pub fn create_on_disk(git_dir: &Path) -> Result<(), Error> {
        let _ = git_dir;
        Err(Error::unsupported("create_on_disk"))
    }

    /// `reftable_be_remove_on_disk()` (refs/reftable-backend.c:513-559): delete
    /// the reftable store of `git_dir`. Each failure is added to the returned
    /// message, git's `err`, and the rest still attempted.
    pub fn remove_on_disk(git_dir: &Path) -> Result<(), String> {
        let _ = git_dir;
        Err(Error::unsupported("remove_on_disk").to_string())
    }

    /// `reftable_be_create_reflog()` (refs/reftable-backend.c:2400-2432).
    pub fn create_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        let _ = name;
        Err(Error::unsupported("create_reflog"))
    }

    /// `reftable_be_delete_reflog()` (refs/reftable-backend.c:2480-2510).
    pub fn delete_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        let _ = name;
        Err(Error::unsupported("delete_reflog"))
    }

    /// `reftable_be_reflog_expire()` (refs/reftable-backend.c:2575-2737):
    /// write tombstones for the entries `policy` prunes.
    pub fn reflog_expire(
        &self,
        name: &FullNameRef,
        flags: ExpireFlags,
        policy: &mut dyn ExpirePolicy,
    ) -> Result<(), Error> {
        let _ = (name, flags, policy);
        Err(Error::unsupported("reflog_expire"))
    }

    /// `reftable_be_optimize()` (refs/reftable-backend.c:1699-1730): compact the
    /// stack, only as far as needed with `auto` (`REFS_OPTIMIZE_AUTO`).
    pub fn optimize(&self, auto: bool) -> Result<(), Error> {
        let _ = auto;
        Err(Error::unsupported("optimize"))
    }

    /// `reftable_be_optimize_required()` (refs/reftable-backend.c:1732-1754).
    pub fn optimize_required(&self, auto: bool) -> Result<bool, Error> {
        let _ = auto;
        Err(Error::unsupported("optimize_required"))
    }

    /// `reftable_be_rename_ref()` (refs/reftable-backend.c:1987-2016): move
    /// `old` with its reflog to `new`, logging `logmsg` as `committer`.
    pub fn rename_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        let _ = (old, new, committer, logmsg);
        Err(Error::unsupported("rename_ref"))
    }

    /// `reftable_be_copy_ref()` (refs/reftable-backend.c:2018-2047).
    pub fn copy_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        let _ = (old, new, committer, logmsg);
        Err(Error::unsupported("copy_ref"))
    }

    /// `reftable_be_fsck()` (refs/reftable-backend.c:2769-2860): check the stack
    /// of `worktree` (`None`: the main worktree) and its references, passing
    /// each problem to `report` (whose return is `fsck_report_ref()`'s) and
    /// each progress message to `verbose`. `Ok(true)` if errors were found.
    pub fn fsck(
        &self,
        worktree: Option<&BStr>,
        report: &mut dyn FnMut(FsckReport<'_>) -> i32,
        verbose: &mut dyn FnMut(&str),
    ) -> Result<bool, Error> {
        let _ = (worktree, report, verbose);
        Err(Error::unsupported("fsck"))
    }
}
