//! git's `REF_TRANSACTION_FLAG_INITIAL` transaction, which only `git clone`'s
//! `write_remote_refs()` opens (builtin/clone.c:473-498, v2.56.0).

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString};

use crate::{
    FullName, Target,
    store_impl::{file, packed},
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
};

/// The error returned by [`file::Store::initial_transaction_commit()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    /// `ref_update_reject_duplicates()` (refs.c), git's wording.
    #[error("multiple updates for ref '{name}' not allowed")]
    Duplicate { name: BString },
    #[error("The lock for the packed-ref file could not be obtained")]
    PackedLock(#[from] gix_lock::acquire::Error),
    #[error("The packed ref buffer could not be loaded")]
    PackedOpen(#[from] packed::buffer::open::Error),
    #[error(transparent)]
    PackedPrepare(#[from] packed::transaction::prepare::Error),
    #[error(transparent)]
    PackedCommit(#[from] packed::transaction::commit::Error),
    #[error("The packed ref file could not be written")]
    Io(#[from] std::io::Error),
    #[error("The packed ref file could not be committed")]
    LockCommit(#[from] gix_lock::commit::Error<gix_lock::File>),
    #[error(transparent)]
    Prepare(#[from] super::prepare::Error),
    #[error(transparent)]
    Commit(#[from] super::commit::Error),
}

impl file::Store {
    /// Create every reference of `creations`, in order, in one transaction begun with
    /// `REF_TRANSACTION_FLAG_INITIAL` and committed with `ref_transaction_commit()`, as
    /// `write_remote_refs()` does with each `ref_transaction_create(t, r->peer_ref->name,
    /// &r->old_oid, NULL, 0, NULL, &err)` (builtin/clone.c:473-498, v2.56.0).
    ///
    /// `ref_transaction_prepare()` refuses a name queued twice before anything else
    /// (`ref_update_reject_duplicates()`, refs.c:2708-2709), then runs the
    /// `reference-transaction` hook at `preparing` and, once the backend prepared,
    /// at `prepared` (refs.c:2711-2727). `ref_transaction_commit()` skips the
    /// `committed` run for this flag (refs.c:2783-2785).
    ///
    /// The files backend prepares nothing (`files_transaction_prepare()`,
    /// refs/files-backend.c:2966-2967) and `files_transaction_finish_initial()` hands
    /// every update to a `packed-refs` transaction begun with the same flag
    /// (refs/files-backend.c:3194-3300): the lock is taken, that transaction runs the
    /// hook at `preparing` and `prepared` and writes the file, which it does even when
    /// it holds no update (`packed_transaction_prepare()`, refs/packed-backend.c:1725-1732).
    /// No loose file and no reflog is written. The annotated tags get their `^` lines
    /// from `objects`, as `ref_transaction_update()` peels them (refs.c:1438-1446).
    ///
    /// The reftable backend writes the references in one table, each with a reflog
    /// entry without message where `should_write_log()` asks for one (`write_transaction_table()`,
    /// refs/reftable-backend.c), so the hook sees `preparing` and `prepared` and the table
    /// is then written as an ordinary transaction would write it.
    ///
    /// Only object ids are created: `write_remote_refs()` queues nothing else, so the
    /// loose transaction `files_transaction_finish_initial()` opens for a symbolic ref, a
    /// root ref or a `REF_LOG_ONLY` update is never needed here.
    pub fn initial_transaction_commit<'a>(
        &self,
        creations: &[(FullName, ObjectId)],
        objects: Box<dyn gix_object::Find + 'a>,
        committer: Option<gix_actor::SignatureRef<'_>>,
        lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<(), Error> {
        file::first_use();
        let mut names: Vec<&BStr> = creations.iter().map(|(name, _)| name.as_bstr()).collect();
        names.sort();
        if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(Error::Duplicate { name: pair[0].to_owned() });
        }

        let mut lines = Vec::new();
        for (name, id) in creations {
            file::transaction::hook_line(
                &mut lines,
                self.object_hash,
                None,
                Some(file::transaction::HookValue::Object(id)),
                name.as_bstr(),
            );
        }
        let edit = |(name, id): &(FullName, ObjectId)| RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: BString::default(),
                },
                expected: PreviousValue::MustNotExist,
                new: Target::Object(*id),
            },
            name: name.clone(),
            deref: false,
        };

        if self.reftable().is_some() {
            for state in ["preparing", "prepared"] {
                if !file::transaction_hook(self, state, &lines) {
                    file::transaction_hook_refused(self, state, &[&lines]);
                }
            }
            return file::without_transaction_hook(|| -> Result<(), Error> {
                self.transaction()
                    .packed_refs(file::transaction::PackedRefs::DeletionsAndNonSymbolicUpdates(objects))
                    .prepare(creations.iter().map(edit), lock_fail_mode, lock_fail_mode)?
                    .commit(committer)?;
                Ok(())
            });
        }

        // The outer transaction: `ref_transaction_prepare()` around a backend that prepares nothing.
        for state in ["preparing", "prepared"] {
            if !file::transaction_hook(self, state, &lines) {
                file::transaction_hook_refused(self, state, &[&lines]);
            }
        }

        // `files_transaction_finish_initial()`: `packed_refs_lock()`, then the nested transaction.
        crate::store_impl::file::write_options();
        file::packed_refs_lock();
        let path = self.packed_refs_path();
        let mut lock = gix_lock::File::acquire_to_update_resource(&path, lock_fail_mode, None)?;
        if !file::transaction_hook(self, "preparing", &lines) {
            drop(lock);
            file::transaction_hook_refused(self, "preparing", &[&lines]);
        }
        if creations.is_empty() {
            // `write_with_updates()` rewrites the file from what it holds and the (here
            // no) updates; a clone's ref store has no `packed-refs` yet, so that is the
            // header alone. An existing file is carried over as it is.
            match std::fs::read(&path) {
                Ok(existing) => std::io::Write::write_all(&mut lock, &existing)?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    std::io::Write::write_all(&mut lock, packed::transaction::HEADER_LINE)?;
                }
                Err(err) => return Err(err.into()),
            }
            if !file::transaction_hook(self, "prepared", &lines) {
                drop(lock);
                file::transaction_hook_refused(self, "prepared", &[&lines]);
            }
            lock.commit()?;
        } else {
            let transaction = packed::Transaction::new_from_pack_and_lock(
                self.assure_packed_refs_uptodate()?,
                lock,
                self.precompose_unicode,
                self.namespace.clone(),
            )
            .prepare(&mut creations.iter().map(edit), &*objects)?;
            if !file::transaction_hook(self, "prepared", &lines) {
                drop(transaction);
                file::transaction_hook_refused(self, "prepared", &[&lines]);
            }
            transaction.commit()?;
        }
        // The nested transaction committed the file; the store's cached view of it is stale.
        self.force_refresh_packed_buffer()?;
        Ok(())
    }
}
