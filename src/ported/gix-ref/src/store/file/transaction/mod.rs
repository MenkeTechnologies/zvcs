use std::fmt::Formatter;

use gix_hash::ObjectId;
use gix_object::bstr::BString;

use crate::{
    store_impl::{file, file::Transaction},
    transaction::RefEdit,
};

/// How to handle packed refs during a transaction
#[derive(Default)]
pub enum PackedRefs<'a> {
    /// Only propagate deletions of references. This is the default.
    /// This means deleted references are removed from disk if they are loose and from the packed-refs file if they are present.
    #[default]
    DeletionsOnly,
    /// Propagate deletions as well as updates to references which are peeled and contain an object id.
    ///
    /// This means deleted references are removed from disk if they are loose and from the packed-refs file if they are present,
    /// while updates are also written into the loose file as well as into packed-refs, potentially creating an entry.
    DeletionsAndNonSymbolicUpdates(Box<dyn gix_object::Find + 'a>),
    /// Propagate deletions as well as updates to references which are peeled and contain an object id. Furthermore delete the
    /// reference which is originally updated if it exists. If it doesn't, the new value will be written into the packed ref right away.
    /// Note that this doesn't affect symbolic references at all, which can't be placed into packed refs.
    ///
    /// Thus, this is similar to `DeletionsAndNonSymbolicUpdates`, but removes the loose reference after the update, leaving only their copy
    /// in `packed-refs`.
    DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(Box<dyn gix_object::Find + 'a>),
}

#[derive(Debug)]
pub(in crate::store_impl::file) struct Edit {
    update: RefEdit,
    lock: Option<gix_lock::Marker>,
    /// Set if this update is coming from a symbolic reference and used to make it appear like it is the one that is handled,
    /// instead of the referent reference.
    parent_index: Option<usize>,
    /// For symbolic refs, this is the previous OID to put into the reflog instead of our own previous value. It's the
    /// peeled value of the leaf referent.
    leaf_referent_previous_oid: Option<ObjectId>,
    /// git's `REF_LOG_ONLY`, which is narrower than [`RefLog::Only`][crate::transaction::RefLog::Only].
    ///
    /// `RefLog::Only` on a deletion is overloaded. A caller can pass it to mean "delete the reflog and
    /// leave the reference alone" — gix's own feature, with no counterpart in git. The splitter also
    /// *assigns* it to the symbolic half of a dereferenced edit, moving the caller's original mode onto
    /// the referent; that half is git's `REF_LOG_ONLY`, and it must gain a reflog entry rather than lose
    /// its log. The two are told apart by the referent's mode: `RefLog::AndReference` there means the
    /// caller asked to delete a reference, so this half is the log-only mirror of that deletion.
    ///
    /// Only ever set on a deletion; updates carry git's flag faithfully in `RefLog::Only` already.
    log_only_split: bool,
    /// git's `REF_ISSYMREF`: the reference *being updated* held `ref: <name>` before this edit.
    ///
    /// `lock_raw_ref()` sets it from the value found on disk (refs/files-backend.c:530 and :628,
    /// v2.55.0), and the shortcut that drops an update writing the value a reference already holds
    /// is guarded by it:
    ///
    /// ```c
    /// if (!(update->type & REF_ISSYMREF) &&
    ///     oideq(&lock->old_oid, &update->new_oid)) {
    ///         /*
    ///          * The reference already has the desired
    ///          * value, so we don't need to write it.
    ///          */
    /// } else {
    ///         ret = write_ref_to_lockfile(refs, lock, &update->new_oid, err);
    ///         ...
    ///         update->flags |= REF_NEEDS_COMMIT;
    /// }
    /// ```
    ///
    /// (`lock_ref_for_update()`, refs/files-backend.c:2806-2833, v2.55.0.) `old_oid` for a symref
    /// under `REF_NO_DEREF` is the id the *referent* resolves to, so the two can compare equal
    /// while the update is still a real change: the reference stops being symbolic. git therefore
    /// takes the write-and-flag branch, and `files_transaction_finish()` writes the reflog from
    /// that same `REF_NEEDS_COMMIT` (refs/files-backend.c:3301-3307). Which is why
    /// `git update-ref --no-deref HEAD $(git rev-parse HEAD)` appends `<id> <id> <ident> <ts>`
    /// to `.git/logs/HEAD` and leaves `HEAD` detached.
    previous_is_symbolic: bool,
}

impl Edit {
    fn name(&self) -> BString {
        self.update.name.0.clone()
    }
}

impl std::borrow::Borrow<RefEdit> for Edit {
    fn borrow(&self) -> &RefEdit {
        &self.update
    }
}

impl std::borrow::BorrowMut<RefEdit> for Edit {
    fn borrow_mut(&mut self) -> &mut RefEdit {
        &mut self.update
    }
}

/// Edits
impl file::Store {
    /// Open a transaction with the given `edits`, and determine how to fail if a `lock` cannot be obtained.
    /// A snapshot of packed references will be obtained automatically if needed to fulfill this transaction
    /// and will be provided as result of a successful transaction. Note that upon transaction failure, packed-refs
    /// will never have been altered.
    ///
    /// The transaction inherits the parent namespace.
    pub fn transaction(&self) -> Transaction<'_, '_> {
        Transaction {
            store: self,
            packed_transaction: None,
            packed_buffer: None,
            updates: None,
            packed_refs: PackedRefs::default(),
        }
    }
}

impl<'p> Transaction<'_, 'p> {
    /// Configure the way packed refs are handled during the transaction
    pub fn packed_refs(mut self, packed_refs: PackedRefs<'p>) -> Self {
        self.packed_refs = packed_refs;
        self
    }
}

impl std::fmt::Debug for Transaction<'_, '_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("store", self.store)
            .field("edits", &self.updates.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

///
pub mod prepare;

///
pub mod commit;
