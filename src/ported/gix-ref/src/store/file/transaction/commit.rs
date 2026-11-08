use crate::{
    Target,
    store_impl::file::{Transaction, transaction::PackedRefs},
    transaction::{Change, LogChange, RefEdit, RefLog},
};

impl Transaction<'_, '_> {
    /// Make all [prepared][Transaction::prepare()] permanent and return the performed edits which represent the current
    /// state of the affected refs in the ref store in that instant. Please note that the obtained edits may have been
    /// adjusted to contain more dependent edits or additional information.
    /// `committer` is used in the reflog and only if the reflog is actually written, which is why it is optional. Please note
    /// that if `None` is passed and the reflog needs to be written, the operation will be aborted late and a few refs may have been
    /// successfully committed already, making clear the non-atomic nature of multi-file edits.
    ///
    /// On error the transaction may have been performed partially, depending on the nature of the error, and no attempt to roll back
    /// partial changes is made.
    ///
    /// In this stage, we perform the following operations:
    ///
    /// * update the ref log
    /// * move updated refs into place
    /// * delete reflogs and empty parent directories
    /// * delete packed refs
    /// * delete their corresponding reference (if applicable)
    ///   along with empty parent directories
    ///
    /// Note that transactions will be prepared automatically as needed.
    pub fn commit<'a>(self, committer: impl Into<Option<gix_actor::SignatureRef<'a>>>) -> Result<Vec<RefEdit>, Error> {
        self.commit_inner(committer.into())
    }

    fn commit_inner(mut self, committer: Option<gix_actor::SignatureRef<'_>>) -> Result<Vec<RefEdit>, Error> {
        if let Some(backend) = self.store.reftable() {
            assert!(self.updates.is_some(), "BUG: must call prepare before commit");
            let data = self.reftable.take().expect("prepare keeps the reftable transaction");
            let edits = backend.transaction_finish(data, committer)?;
            // `ref_transaction_commit()` runs the hook at `committed` once the backend
            // has finished, whatever it exits with (refs.c:2783-2785).
            if let Some(feed) = &self.hook {
                crate::file::transaction_hook(self.store, "committed", &feed.lines);
            }
            return Ok(edits);
        }
        // `files_transaction_finish()` opens with `files_ref_store_write_options(refs)`
        // (refs/files-backend.c:3327, v2.56.0), the lazy config read that dies on a bad
        // `core.logAllRefUpdates` or `core.preferSymlinkRefs`.
        crate::store_impl::file::write_options();
        let mut updates = self.updates.expect("BUG: must call prepare before commit");
        // `files_transaction_finish()` passes over every update with a `rejection_err`
        // (refs/files-backend.c:3352, :3400, :3434), and a half never split off is not
        // in the transaction at all.
        updates.retain(|edit| !edit.rejected && !edit.dropped);
        let delete_loose_refs = matches!(
            self.packed_refs,
            PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_)
        );

        // Perform updates first so live commits remain referenced
        for change in &mut updates {
            assert!(!change.update.deref, "Deref mode is turned into splits and turned off");
            match &change.update.change {
                // reflog first, then reference
                Change::Update { log, new, expected } => {
                    let lock = change.lock.take();
                    let (update_ref, update_reflog) = match log.mode {
                        RefLog::Only => (false, true),
                        RefLog::AndReference => (true, true),
                    };
                    if update_reflog {
                        // `parse_and_write_reflog()` for a symbolic update (refs/files-backend.c:3154-3170):
                        // the entry records what the new target resolves to, and `lock->old_oid`, the
                        // value the reference resolved to before (`lock_ref_for_update()`,
                        // refs/files-backend.c:2764-2789). A dangling target logs nothing.
                        let symref_log = match new {
                            Target::Symbolic(target) if self.options.log_symref_updates => Some((
                                resolved_id(self.store, change.update.name.as_ref()),
                                resolved_id(self.store, target.as_ref()),
                            )),
                            _ => None,
                        };
                        let log_update = match new {
                            Target::Symbolic(_) if symref_log.is_some() => symref_log
                                .as_ref()
                                .and_then(|(previous, new_oid)| Some((*previous, new_oid.as_ref()?))),
                            Target::Symbolic(_) => {
                                // Special HACK: no reflog for symref changes as there is no OID involved which the reflog needs.
                                // Unless, the ref is new and we can obtain a peeled id
                                // identified by the expectation of what could be there, as is the case when cloning.
                                //
                                // git does log one, under the *resolved* value of the new target
                                // (`parse_and_write_reflog()`, refs/files-backend.c:3079). Closing
                                // the gap here alone makes it worse rather than better: every
                                // caller in `src/extensions` that points `HEAD` at a branch
                                // already writes that line by hand — `switch::attach_head`,
                                // `branch`'s rename and its worktree repointing,
                                // `checkout::append_head_log` — precisely because this hack
                                // predates them, so logging it here too appends it twice. Measured
                                // over the corpus, that is 53 cases lost across `switch`,
                                // `checkout`, `branch`, `clone`, `restore`, `stash` and
                                // `worktree` against 4 gained in `update-ref`. The gap and its
                                // repairs have to be removed in one change, not this one.
                                match expected {
                                    PreviousValue::ExistingMustMatch(Target::Object(oid)) => {
                                        Some((Some(gix_hash::ObjectId::null(oid.kind())), oid))
                                    }
                                    _ => None,
                                }
                            }
                            Target::Object(new_oid) => {
                                let previous = match expected {
                                    // Here, this means that the ref already existed, and that it will receive (even transitively)
                                    // the given value
                                    PreviousValue::MustExistAndMatch(Target::Object(oid)) => Some(oid.to_owned()),
                                    _ => None,
                                }
                                .or(change.leaf_referent_previous_oid);
                                Some((previous, new_oid))
                            }
                        };
                        if let Some((previous, new_oid)) = log_update {
                            // `files_transaction_finish()` writes the reflog when
                            // `REF_NEEDS_COMMIT || REF_LOG_ONLY`. `lock_ref_for_update()`
                            // withholds `REF_NEEDS_COMMIT` when the value is unchanged, but a
                            // log-only update — the `HEAD` half of a dereferenced or
                            // head-split edit — is logged regardless. That is why `git reset`
                            // with no argument still appends `reset: moving to HEAD` to
                            // `.git/logs/HEAD` while leaving the branch log untouched.
                            //
                            // The unchanged-value half of that is `!(update->type & REF_ISSYMREF)
                            // && oideq(&lock->old_oid, &update->new_oid)`
                            // (refs/files-backend.c:2807-2808, v2.55.0): a reference that *was*
                            // symbolic is never dropped for being "unchanged", because replacing
                            // `ref: refs/heads/main` with the id it resolves to detaches it. Hence
                            // the third disjunct — see [`Edit::previous_is_symbolic`].
                            // A symbolic update always has `REF_NEEDS_COMMIT` (refs/files-backend.c:2839-2846).
                            let do_update = log.mode == RefLog::Only
                                || previous.as_ref() != Some(new_oid)
                                || change.previous_is_symbolic
                                || symref_log.is_some();
                            if do_update {
                                self.store.reflog_create_or_append(
                                    change.update.name.as_ref(),
                                    previous,
                                    new_oid,
                                    committer,
                                    log.message.as_ref(),
                                    log.force_create_reflog,
                                )?;
                            }
                        }
                    }
                    // Don't do anything else while keeping the lock after potentially updating the reflog.
                    // We delay deletion of the reference and dropping the lock to after the packed-refs were
                    // safely written.
                    if delete_loose_refs && matches!(new, Target::Object(_)) {
                        change.lock = lock;
                        continue;
                    }
                    if update_ref {
                        if let Some(Err(err)) = lock.map(gix_lock::Marker::commit) {
                            // TODO: when Kind::IsADirectory becomes stable, use that.
                            let err = if err.instance.resource_path().is_dir() {
                                gix_tempfile::remove_dir::empty_depth_first(err.instance.resource_path())
                                    .map_err(std::io::Error::other)
                                    .and_then(|_| err.instance.commit().map_err(|err| err.error))
                                    .err()
                            } else {
                                Some(err.error)
                            };

                            if let Some(err) = err {
                                return Err(Error::LockCommit {
                                    source: err,
                                    full_name: change.name(),
                                });
                            }
                        }
                    }
                }
                // A deletion never sets `REF_NEEDS_COMMIT` (`lock_ref_for_update()` skips the
                // write-and-flag branch for `REF_DELETING`), so it logs nothing and its log is
                // unlinked below — *unless* it is git's `REF_LOG_ONLY`, the shape
                // `split_symref_update()` and `split_head_update()` leave behind for `HEAD`.
                // Both `update-ref -d HEAD` and `update-ref -d refs/heads/main` (with `HEAD`
                // pointing at it) therefore append `<old> <null>` to `.git/logs/HEAD` and keep
                // that log, while the branch and its own log are removed.
                //
                // `log_only_split` and not `RefLog::Only`: a caller passing `RefLog::Only`
                // directly means gix's "delete the reflog, keep the reference", which has no
                // git counterpart and must still delete the log.
                Change::Delete { expected, message, .. } => {
                    if change.log_only_split {
                        let previous = match expected {
                            PreviousValue::MustExistAndMatch(Target::Object(oid))
                            | PreviousValue::ExistingMustMatch(Target::Object(oid)) => Some(oid.to_owned()),
                            _ => None,
                        }
                        .or(change.leaf_referent_previous_oid);
                        self.store.reflog_create_or_append(
                            change.update.name.as_ref(),
                            previous,
                            &self.store.object_hash.null(),
                            committer,
                            message.as_ref(),
                            false,
                        )?;
                    }
                }
            }
        }

        for change in &mut updates {
            let (reflog_root, relative_name) = self.store.reflog_base_and_relative_path(change.update.name.as_ref());
            // `files_transaction_finish()` unlinks the reflog only for updates that are
            // `REF_DELETING && !REF_LOG_ONLY`; the log-only mirror exists precisely to add an
            // entry to a log that stays. A caller-requested `RefLog::Only` deletion is not that
            // flag — it is a request to remove the log — so it falls through and deletes.
            if change.log_only_split {
                continue;
            }
            match &change.update.change {
                Change::Update { .. } => {}
                Change::Delete { .. } => {
                    // Reflog deletion happens first in case it fails a ref without log is less terrible than
                    // a log without a reference.
                    let reflog_path = reflog_root.join(relative_name);
                    if let Err(err) = std::fs::remove_file(&reflog_path) {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            return Err(Error::DeleteReflog {
                                source: err,
                                full_name: change.name(),
                            });
                        }
                    } else {
                        // `try_remove_empty_parents(refs, refname, REMOVE_EMPTY_PARENTS_REFLOG)`
                        // (refs/files-backend.c:3402-3410, 1330-1371) spares `logs/refs/` and the
                        // `logs/refs/<kind>/` below it, as the loose-ref pruning further down does.
                        let components = change.update.name.as_bstr().split(|&b| b == b'/').count();
                        let mut dir = reflog_path.parent();
                        for _ in 0..components.saturating_sub(3) {
                            let Some(d) = dir else { break };
                            if std::fs::remove_dir(d).is_err() {
                                break;
                            }
                            dir = d.parent();
                        }
                    }
                }
            }
        }

        let mut hook = self.hook.take();
        let packing = !matches!(self.packed_refs, PackedRefs::DeletionsOnly);
        if let Some(t) = self.packed_transaction {
            t.commit().map_err(Error::PackedTransactionCommit)?;
            // `files_transaction_finish()` commits the nested `packed-refs` transaction
            // before it removes the loose files (refs/files-backend.c:3420-3428).
            if let Some(packed) = hook.as_ref().and_then(|feed| feed.packed.as_ref()) {
                crate::file::transaction_hook(self.store, "committed", packed);
            }
            // Always refresh ourselves right away to avoid races. We ignore errors as there may be many reasons this fails, and it's not
            // critical to be done here. In other words, the pack may be refreshed at a later time and then it might work.
            self.store.force_refresh_packed_buffer().ok();
        }
        // `files_pack_refs()` commits its `packed-refs` transaction before it prunes
        // the loose references (refs/files-backend.c:1555-1573).
        if packing {
            if let Some(feed) = hook.take() {
                crate::file::transaction_hook(self.store, "committed", &feed.lines);
            }
        }

        // `files_pack_refs()` prepends each reference to prune to its list
        // (refs/files-backend.c:1557-1560), so `prune_refs()` removes them in the reverse
        // of the order they were packed in.
        let order: Vec<usize> = if delete_loose_refs {
            (0..updates.len()).rev().collect()
        } else {
            (0..updates.len()).collect()
        };
        for idx in order {
            let change = &mut updates[idx];
            let take_lock_and_delete = match &change.update.change {
                Change::Update {
                    log: LogChange { mode, .. },
                    new,
                    ..
                } => delete_loose_refs && *mode == RefLog::AndReference && matches!(new, Target::Object(_)),
                Change::Delete { log: mode, .. } => *mode == RefLog::AndReference,
            };
            if take_lock_and_delete {
                // `prune_ref()` removes each packed loose reference in a transaction of
                // its own, `REF_HAVE_OLD | REF_HAVE_NEW | REF_IS_PRUNING` from its value to
                // the null id, which runs the hook (refs/files-backend.c:1373-1395).
                let mut prune_lines = Vec::new();
                if let Change::Update {
                    new: Target::Object(oid), ..
                } = &change.update.change
                {
                    crate::file::transaction::hook_line(
                        &mut prune_lines,
                        self.store.object_hash,
                        Some(crate::file::transaction::HookValue::Object(oid)),
                        None,
                        change.update.name.as_bstr(),
                    );
                    for state in ["preparing", "prepared"] {
                        if !crate::file::transaction_hook(self.store, state, &prune_lines) {
                            gix_tempfile::registry::cleanup_tempfiles();
                            crate::file::transaction_hook_refused(self.store, state, &[&prune_lines]);
                        }
                    }
                }
                let lock = change.lock.take();
                let reference_path = self.store.reference_path(change.update.name.as_ref());
                if let Err(err) = std::fs::remove_file(reference_path) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        return Err(Error::DeleteReference {
                            err,
                            full_name: change.name(),
                        });
                    }
                }
                drop(lock);
                if !prune_lines.is_empty() {
                    crate::file::transaction_hook(self.store, "committed", &prune_lines);
                }
            }
        }
        // `files_transaction_finish()` marks every `REF_DELETING && !REF_LOG_ONLY`
        // update `REF_DELETED_RMDIR`, packed or loose, and once the locks are gone
        // runs `try_remove_empty_parents(refs, refname, REMOVE_EMPTY_PARENTS_REF)`
        // for each (refs/files-backend.c:3430-3471): the directories between
        // `refs/<kind>/` and the ref are removed bottom-up until one is not empty.
        // Each deleted ref's path, and how many of its parent directories lie below
        // `refs/<kind>/`.
        let deleted: Vec<(std::path::PathBuf, usize)> = updates
            .iter()
            .filter(|c| {
                !c.log_only_split
                    && matches!(
                        c.update.change,
                        Change::Delete {
                            log: RefLog::AndReference,
                            ..
                        }
                    )
            })
            .map(|c| {
                let components = c.update.name.as_bstr().split(|&b| b == b'/').count();
                (
                    self.store.reference_path(c.update.name.as_ref()),
                    components.saturating_sub(3),
                )
            })
            .collect();
        let edits = updates.into_iter().map(|edit| edit.update).collect();
        for (path, parents) in deleted {
            let mut dir = path.parent();
            for _ in 0..parents {
                let Some(d) = dir else { break };
                if std::fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
        if let Some(feed) = &hook {
            crate::file::transaction_hook(self.store, "committed", &feed.lines);
        }
        Ok(edits)
    }
}
mod error {
    use gix_object::bstr::BString;

    use crate::store_impl::{file, packed};

    /// The error returned by various [`Transaction`][super::Transaction] methods.
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("The packed-ref transaction could not be committed")]
        PackedTransactionCommit(#[source] packed::transaction::commit::Error),
        #[error("Edit preprocessing failed with error")]
        PreprocessingFailed { source: std::io::Error },
        #[error("The change for reference {full_name:?} could not be committed")]
        LockCommit { source: std::io::Error, full_name: BString },
        #[error("The reference {full_name} could not be deleted")]
        DeleteReference { full_name: BString, err: std::io::Error },
        #[error("The reflog of reference {full_name:?} could not be deleted")]
        DeleteReflog { full_name: BString, source: std::io::Error },
        #[error("The reflog could not be created or updated")]
        CreateOrUpdateRefLog(#[from] file::log::create_or_update::Error),
        /// A store with the reftable backend failed to write or commit a table;
        /// `message` is git's `reftable: transaction failure: <reason>`.
        #[error("{message}")]
        Reftable { message: BString },
    }
}
pub use error::Error;

use crate::transaction::PreviousValue;

/// `refs_resolve_ref_unsafe(refs, name, RESOLVE_REF_READING, &oid, NULL)` (refs.c): the object `name`
/// resolves to through at most `SYMREF_MAXDEPTH` (5) symbolic references, if any.
fn resolved_id(store: &crate::store_impl::file::Store, name: &crate::FullNameRef) -> Option<gix_hash::ObjectId> {
    let mut name = name.to_owned();
    for _ in 0..=5 {
        match store.try_find(name.as_ref()).ok()??.target {
            Target::Object(oid) => return Some(oid),
            Target::Symbolic(next) => name = next,
        }
    }
    None
}
