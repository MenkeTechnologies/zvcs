use gix_object::bstr::BString;

use crate::{
    FullName, FullNameRef, Reference, Target, packed,
    packed::transaction::buffer_into_transaction,
    store_impl::{
        file,
        file::{
            Transaction, loose,
            transaction::{Edit, PackedRefs, ErrorKind, Rejection},
        },
    },
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefEditsExt, RefLog},
};

impl Transaction<'_, '_> {
    /// Read the current value of a reference from loose storage, falling back to packed refs.
    ///
    /// Must be called while holding the lock for `name` so the subsequent CAS check
    /// compares against a value that cannot be changed concurrently.
    fn read_existing_ref(
        store: &file::Store,
        name: &FullNameRef,
        packed: Option<&packed::Buffer>,
    ) -> Result<Option<Reference>, Error> {
        store
            .ref_contents(name)
            .map_err(Error::from)
            .and_then(|maybe_loose| {
                maybe_loose
                    .map(|buf| {
                        loose::Reference::try_from_path(name.to_owned(), &buf, store.object_hash)
                            .map(Reference::from)
                            .map_err(Error::from)
                    })
                    .transpose()
            })
            .or_else(|err| match err {
                Error::ReferenceDecode(_) => Ok(None),
                other => Err(other),
            })
            .and_then(|maybe_loose| match (maybe_loose, packed) {
                (None, Some(packed)) => packed
                    .try_find(name)
                    .map(|opt| opt.map(Into::into))
                    .map_err(Error::from),
                (None, None) => Ok(None),
                (maybe_loose, _) => Ok(maybe_loose),
            })
    }

    /// Map a lock-acquisition failure to our error type, surfacing genuine I/O errors
    /// (such as a path collision reported as `NotADirectory`) as [`Error::Io`] rather than
    /// burying them in [`Error::LockAcquire`], which is reserved for actual contention.
    // This happens for path collisions where `a` is a ref file, and `a/b` is the lock to be created.
    fn lock_acquire_error(err: gix_lock::acquire::Error, full_name: &str) -> Error {
        match err {
            gix_lock::acquire::Error::Io(err) => Error::Io(err),
            source => Error::LockAcquire {
                source,
                full_name: full_name.into(),
            },
        }
    }

    /// Follow the symref chain starting at `start` and return the object id it ends on,
    /// or `None` if the chain is dangling or longer than git's `SYMREF_MAXDEPTH`.
    ///
    /// Mirrors the `refs_resolve_ref_unsafe(&refs->base, referent.buf, 0, &lock->old_oid, NULL)`
    /// call `lock_ref_for_update()` (refs/files-backend.c) makes for a symbolic reference
    /// updated with `REF_NO_DEREF`: the reflog's "old" side is the value the symref resolved
    /// to, not a null id.
    fn resolve_symref_chain(
        store: &file::Store,
        start: &FullName,
        packed: Option<&packed::Buffer>,
    ) -> Option<gix_hash::ObjectId> {
        let mut name = start.clone();
        // git's SYMREF_MAXDEPTH.
        for _ in 0..5 {
            match Self::read_existing_ref(store, name.as_ref(), packed).ok()?? {
                Reference {
                    target: Target::Object(id),
                    ..
                } => return Some(id),
                Reference {
                    target: Target::Symbolic(next),
                    ..
                } => name = next,
            }
        }
        None
    }

    fn lock_ref_and_apply_change(
        store: &file::Store,
        lock_fail_mode: gix_lock::acquire::Fail,
        packed: Option<&packed::Buffer>,
        change: &mut Edit,
        direct_to_packed_refs: bool,
        must_not_exist_strictly: bool,
    ) -> Result<(), Error> {
        use std::io::Write;
        assert!(
            change.lock.is_none(),
            "locks can only be acquired once and it's all or nothing"
        );

        file::files_ref_lock();

        // Reject Windows reserved device names before acquiring the lock.
        // The lock file itself (e.g. `CON.lock`) is also a device name,
        // so acquiring it would fail or open the device instead of
        // returning the configured validation error.
        store.check_windows_device_name(change.update.name.as_ref())?;

        // Set when the reference being updated is itself symbolic: see [`Self::resolve_symref_chain`].
        // Applied after the borrow of `change.update.change` below ends.
        let mut symref_previous_oid = None;
        // git's `REF_ISSYMREF` (`lock_raw_ref()`, refs/files-backend.c:628, v2.55.0), read from the
        // same existing value as `symref_previous_oid` above and kept for the unchanged-value
        // shortcut in `commit()` — see [`Edit::previous_is_symbolic`].
        let mut previous_is_symbolic = false;
        let verify_only = change.verify_only;

        let lock = match &mut change.update.change {
            Change::Delete { expected, .. } => {
                let (base, relative_path) = store.reference_path_with_base(change.update.name.as_ref());
                let lock = gix_lock::Marker::acquire_to_hold_resource(
                    base.join(relative_path.as_ref()),
                    lock_fail_mode,
                    Some(kind_directory(&base, &relative_path)),
                )
                .map_err(|err| Self::lock_acquire_error(err, "borrowcheck won't allow change.name()"))?;

                let existing_ref = Self::read_existing_ref(store, change.update.name.as_ref(), packed)?;

                match (&expected, &existing_ref) {
                    (PreviousValue::MustNotExist, _) => {
                        panic!("BUG: MustNotExist constraint makes no sense if references are to be deleted")
                    }
                    (PreviousValue::ExistingMustMatch(_) | PreviousValue::Any, None)
                    | (PreviousValue::MustExist | PreviousValue::Any, Some(_)) => {}
                    (PreviousValue::MustExist | PreviousValue::MustExistAndMatch(_), None) => {
                        // `lock_raw_ref()` fails a `mustexist` lock on a missing ref inside
                        // itself and `error_return:` unlocks without pruning, since the lock
                        // never reaches `files_transaction_cleanup()` (refs/files-backend.c:
                        // 885-893, 993-995): the directories it created stay.
                        lock.release_keeping_directories();
                        return Err(Error::DeleteReferenceMustExist {
                            full_name: change.name(),
                        });
                    }
                    (
                        PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous),
                        Some(existing),
                    ) => {
                        let actual = existing.target.clone();
                        if *previous != actual {
                            let expected = previous.clone();
                            return Err(Error::ReferenceOutOfDate {
                                full_name: change.name(),
                                expected,
                                actual,
                            });
                        }
                    }
                }

                // Keep the previous value for the caller and ourselves. Maybe they want to keep a log of sorts.
                if let Some(existing) = existing_ref {
                    *expected = PreviousValue::MustExistAndMatch(existing.target);
                }

                Some(lock)
            }
            Change::Update { expected, new, .. } => {
                let (base, relative_path) = store.reference_path_with_base(change.update.name.as_ref());
                let obtain_lock = || {
                    gix_lock::File::acquire_to_update_resource(
                        base.join(relative_path.as_ref()),
                        lock_fail_mode,
                        Some(kind_directory(&base, &relative_path)),
                    )
                    .map_err(|err| {
                        Self::lock_acquire_error(
                            err,
                            "borrowcheck won't allow change.name() and this will be corrected by caller",
                        )
                    })
                };
                let mut lock = obtain_lock()?;

                let existing_ref = Self::read_existing_ref(store, change.update.name.as_ref(), packed)?;

                if let Some(Reference {
                    target: Target::Symbolic(referent),
                    ..
                }) = &existing_ref
                {
                    previous_is_symbolic = true;
                    symref_previous_oid = Self::resolve_symref_chain(store, referent, packed);
                }

                match (&expected, &existing_ref) {
                    (PreviousValue::Any, _)
                    | (PreviousValue::MustExist, Some(_))
                    | (PreviousValue::MustNotExist | PreviousValue::ExistingMustMatch(_), None) => {}
                    (PreviousValue::MustExist, None) => {
                        let expected = Target::Object(store.object_hash.null());
                        let full_name = change.name();
                        // See the deletion's `MustExist` arm above.
                        lock.release_keeping_directories();
                        return Err(Error::MustExist { full_name, expected });
                    }
                    // `check_old_oid()` refuses whatever the reference holds when the old value
                    // is the null id (refs/files-backend.c:2647-2652); gix accepts one that
                    // already holds the new value, which a verification does not have.
                    (PreviousValue::MustNotExist, Some(existing)) => {
                        if existing.target != *new || verify_only || must_not_exist_strictly {
                            let new = new.clone();
                            return Err(Error::MustNotExist {
                                full_name: change.name(),
                                actual: existing.target.clone(),
                                new,
                            });
                        }
                    }
                    (
                        PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous),
                        Some(existing),
                    ) => {
                        // A symbolic reference updated without being dereferenced has its
                        // old object id checked against the value its referent resolves to
                        // (`lock_ref_for_update()`, refs/files-backend.c:2764-2789).
                        let referent_matches = matches!(
                            (&*previous, &existing.target),
                            (Target::Object(id), Target::Symbolic(_)) if symref_previous_oid == Some(*id)
                        );
                        if *previous != existing.target && !referent_matches {
                            let actual = existing.target.clone();
                            let expected = previous.to_owned();
                            let full_name = change.name();
                            return Err(Error::ReferenceOutOfDate {
                                full_name,
                                actual,
                                expected,
                            });
                        }
                    }

                    (PreviousValue::MustExistAndMatch(previous), None) => {
                        let expected = previous.to_owned();
                        let full_name = change.name();
                        // See the deletion's `MustExist` arm above.
                        lock.release_keeping_directories();
                        return Err(Error::MustExist { full_name, expected });
                    }
                }

                fn new_would_change_existing(new: &Target, existing: &Target) -> (bool, bool) {
                    match (new, existing) {
                        (Target::Object(new), Target::Object(old)) => (old != new, false),
                        (Target::Symbolic(new), Target::Symbolic(old)) => (old != new, true),
                        (Target::Object(_), _) => (true, false),
                        (Target::Symbolic(_), _) => (true, true),
                    }
                }

                let (is_effective, is_symbolic) = if let Some(existing) = existing_ref {
                    let (effective, is_symbolic) = new_would_change_existing(new, &existing.target);
                    *expected = PreviousValue::MustExistAndMatch(existing.target);
                    (effective, is_symbolic)
                } else {
                    (true, matches!(new, Target::Symbolic(_)))
                };

                let keep_lock_for_loose_source_delete = direct_to_packed_refs && matches!(new, Target::Object(_));
                if verify_only {
                    // A verification has no `REF_HAVE_NEW`: the lock is held and nothing is
                    // written to it (refs/files-backend.c:2857-2860, :2889-2901).
                    Some(lock.close()?)
                } else if (is_effective && !direct_to_packed_refs) || is_symbolic {
                    lock.with_mut(|file| match new {
                        Target::Object(oid) => writeln!(file, "{oid}"),
                        Target::Symbolic(name) => writeln!(file, "ref: {}", name.0),
                    })?;
                    Some(lock.close()?)
                } else if keep_lock_for_loose_source_delete {
                    Some(lock.close()?)
                } else {
                    None
                }
            }
        };
        // Only a fallback: a symref that *is* being dereferenced gets the same value written
        // here again by its split-off child once that child is locked.
        if let Some(oid) = symref_previous_oid {
            change.leaf_referent_previous_oid = Some(oid);
        }
        change.previous_is_symbolic = previous_is_symbolic;
        change.lock = lock;
        Ok(())
    }
}

impl Transaction<'_, '_> {
    /// Prepare for calling [`commit(…)`][Transaction::commit()] in a way that can be rolled back perfectly.
    ///
    /// If the operation succeeds, the transaction can be committed or dropped to cause a rollback automatically.
    /// Rollbacks happen automatically on failure and they tend to be perfect.
    /// This method is idempotent.
    pub fn prepare(
        self,
        edits: impl IntoIterator<Item = RefEdit>,
        ref_files_lock_fail_mode: gix_lock::acquire::Fail,
        packed_refs_lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<Self, Error> {
        self.prepare_inner(
            &mut edits.into_iter(),
            ref_files_lock_fail_mode,
            packed_refs_lock_fail_mode,
        )
    }

    fn prepare_inner(
        mut self,
        edits: &mut dyn Iterator<Item = RefEdit>,
        ref_files_lock_fail_mode: gix_lock::acquire::Fail,
        packed_refs_lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<Self, Error> {
        assert!(self.updates.is_none(), "BUG: Must not call prepare(…) multiple times");
        let store = self.store;
        if let Some(backend) = store.reftable() {
            return self.prepare_reftable(backend, edits.collect());
        }
        let edits: Vec<RefEdit> = edits.collect();
        // A transaction migrating values into `packed-refs` is git's `files_pack_refs()`,
        // whose hook runs are those of the `packed-refs` transaction it commits
        // (refs/files-backend.c:1478-1533): each reference as an update with no old value.
        let packing_lines = (!matches!(self.packed_refs, PackedRefs::DeletionsOnly)).then(|| {
            let mut lines = Vec::new();
            for edit in &edits {
                if let Change::Update {
                    new: Target::Object(oid), ..
                } = &edit.change
                {
                    file::transaction::hook_line(
                        &mut lines,
                        store.object_hash,
                        None,
                        Some(file::transaction::HookValue::Object(oid)),
                        edit.name.as_bstr(),
                    );
                }
            }
            lines
        });
        // `ref_transaction_prepare()` runs the hook at `preparing` over the updates as
        // the caller queued them, before the backend locks or splits anything
        // (refs.c:2711-2717).
        match &packing_lines {
            Some(lines) => {
                if !file::transaction_hook(store, "preparing", lines) {
                    file::transaction_hook_refused(store, "preparing", &[lines]);
                }
            }
            None => self.run_preparing_hook(&edits),
        }
        let mut updates: Vec<_> = edits
            .into_iter()
            .enumerate()
            .map(|(idx, update)| Edit {
                update,
                lock: None,
                parent_index: None,
                leaf_referent_previous_oid: None,
                log_only_split: false,
                previous_is_symbolic: false,
                rejected: false,
                dropped: false,
                verify_only: self.options.verify_only.contains(&idx),
            })
            .collect();
        // A transaction that may fail partially splits each update as `lock_ref_for_update()`
        // reaches it (refs/files-backend.c:2699-2702, :2781-2793), so a split that runs into
        // another update refuses the one update it came from; see [`Self::split_on_lock()`].
        // Any other transaction splits up front, where such a split fails it.
        let allow_failure = self.options.allow_failure;
        if allow_failure {
            // `ref_update_reject_duplicates()` (refs.c:2708-2710).
            updates.assure_one_name_has_one_edit().map_err(|name| {
                Error::PreprocessingFailed(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("A reference named '{name}' has multiple edits"),
                ))
            })?;
        } else {
            updates
                .pre_process(
                    &mut |name| {
                        let symbolic_refs_are_never_packed = None;
                        store
                            .find_existing_inner(name, symbolic_refs_are_never_packed)
                            .map(|r| r.target)
                            .ok()
                    },
                    &mut |idx, update| Edit {
                        update,
                        lock: None,
                        parent_index: Some(idx),
                        leaf_referent_previous_oid: None,
                        log_only_split: false,
                        previous_is_symbolic: false,
                        rejected: false,
                        dropped: false,
                        verify_only: false,
                    },
                )
                .map_err(Error::PreprocessingFailed)?;
            // The splits come after their parents, so one pass hands the flag down a chain.
            for idx in 0..updates.len() {
                if let Some(parent) = updates[idx].parent_index {
                    if updates[parent].verify_only {
                        mark_verify_split(&mut updates, idx, parent, store.object_hash);
                    }
                }
            }
        }

        // Identify the halves that are git's `REF_LOG_ONLY` rather than a caller asking for
        // gix's reflog-only deletion — see [`Edit::log_only_split`]. `split_symref_update()`
        // hands the referent the caller's original mode and forces `RefLog::Only` on the
        // symbolic half, so a child that still deletes its reference marks its parent as the
        // log-only mirror of a real deletion.
        for idx in 0..updates.len() {
            let Some(parent) = updates[idx].parent_index else { continue };
            if matches!(
                updates[idx].update.change,
                Change::Delete {
                    log: RefLog::AndReference,
                    ..
                }
            ) {
                updates[parent].log_only_split = true;
            }
        }

        // `split_head_update()` (refs/files-backend.c): "If update is a direct update of
        // head_ref (the reference pointed to by HEAD), then add an extra REF_LOG_ONLY update
        // for HEAD." Without it `update-ref refs/heads/main <rev>` on the checked-out branch
        // moves the branch but leaves `.git/logs/HEAD` short one entry.
        //
        // `head_ref` is HEAD's *immediate* referent (git resolves it with
        // `RESOLVE_REF_NO_RECURSE`) and only counts when HEAD is symbolic.
        //
        // A transaction that migrates refs into `packed-refs` is exempt: `files_pack_refs()`
        // stamps its updates `REF_SKIP_CREATE_REFLOG`, which `split_head_update()` checks
        // first. Packing changes no value, so a `HEAD` entry would be pure noise — and these
        // transactions commit without a committer, so demanding a reflog fails them outright.
        let packing_refs = matches!(
            self.packed_refs,
            PackedRefs::DeletionsAndNonSymbolicUpdates(_)
                | PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_)
        );
        let head_name: FullName = "HEAD".try_into().expect("HEAD is a valid ref name");
        let head_ref = if packing_refs {
            None
        } else {
            store
                .find_existing_inner(head_name.as_ref().as_partial_name(), None)
                .ok()
                .and_then(|r| match r.target {
                    Target::Symbolic(referent) => Some(referent),
                    Target::Object(_) => None,
                })
        };
        // `(index of the branch update, index of the HEAD log-only update)`.
        let mut head_split_indices: Vec<(usize, usize)> = Vec::new();
        if let (Some(head_ref), false) = (&head_ref, allow_failure) {
            // git errors out on a transaction that already touches HEAD; we simply
            // leave such a transaction alone, since the caller has stated what the
            // HEAD entry should be and `assure_one_name_has_one_edit` above would
            // otherwise have to be re-run.
            let head_already_edited = updates.iter().any(|e| e.update.name == head_name);
            // At most one edit can name `head_ref` — `pre_process` above already
            // rejected duplicates — and at most one HEAD entry may exist.
            let branch = (!head_already_edited)
                .then(|| (0..updates.len()).find(|&idx| needs_head_split(&updates, idx, head_ref)))
                .flatten();
            if let Some(idx) = branch {
                updates.push(head_mirror(&updates[idx], &head_name, self.options.log_symref_updates, store.object_hash));
                head_split_indices.push((idx, updates.len() - 1));
            }
        }

        // `files_transaction_prepare()` locks `packed-refs` for any transaction
        // holding a deletion that is not log-only (`refs/files-backend.c:2982-3036`),
        // and a migrating transaction locks it too (`:1478`) — whether or not a
        // `packed-refs` file exists, which the lock decision below does consider.
        let takes_packed_refs_lock = !matches!(self.packed_refs, PackedRefs::DeletionsOnly)
            || updates.iter().any(|edit| {
                matches!(edit.update.change, Change::Delete { log, .. } if log != RefLog::Only)
            });
        if takes_packed_refs_lock {
            file::packed_refs_lock();
        }
        // A transaction that may fail partially locks the loose references first and
        // builds the nested `packed-refs` transaction from the updates that were not
        // refused, as `files_transaction_prepare()` does (refs/files-backend.c:3019-3058);
        // until then `packed-refs` is read unlocked (:3060-3073). Otherwise it is
        // prepared up front.
        if allow_failure {
            self.packed_buffer = self.store.assure_packed_refs_uptodate()?;
        } else {
            self.prepare_packed_transaction(&updates, packed_refs_lock_fail_mode)?;
        }

        // The packed transaction's buffer when there is one, the unlocked snapshot otherwise —
        // see [`Transaction::packed_buffer`][file::Transaction].
        let packed_for_lookups = self
            .packed_transaction
            .as_ref()
            .and_then(packed::Transaction::buffer)
            .or_else(|| self.packed_buffer.as_ref().map(|b| &***b));
        // The updates as `files_transaction_prepare()` leaves them for the hook at
        // `prepared`: split, with the caller's old values (`lock_ref_for_update()`
        // records what it finds on disk in the lock, not the update). Each line is taken
        // as its update is reached, once a split made it log-only and before the lock
        // records the value found; a half split off an update refused before it was split
        // is not among them, so the lines are put together once the locking is done.
        let kind = self.store.object_hash;
        let mut update_lines: Vec<Vec<u8>> = Vec::with_capacity(updates.len());
        // `transaction->refnames` for the splits of a transaction that may fail partially:
        // the names of the updates queued that are not `REF_LOG_ONLY`, sorted by
        // `ref_transaction_prepare()` (refs.c:2708), then each referent a split adds
        // appended unsorted (refs.c:1356-1361) — see [`string_list_find()`].
        let mut refnames: Vec<BString> = updates
            .iter()
            .filter(|edit| log_mode_of(&edit.update.change) != RefLog::Only)
            .map(Edit::name)
            .collect();
        refnames.sort();
        // `transaction->rejections` and `refnames_to_check` (refs/files-backend.c:2961),
        // the updates of references that did not exist, to be checked against
        // `packed-refs` once every loose reference is locked.
        let mut rejections = Vec::new();
        let mut refnames_to_check = Vec::new();
        // `backend_data->ref_locks` (refs/files-backend.c:2715-2733): the names a lock was
        // taken for, which a later update of the same name reuses; see [`Self::reused_lock()`].
        let mut ref_locks: std::collections::HashSet<BString> = Default::default();
        // `lock_ref_for_update()` may append updates, which are reached in turn.
        for cid in 0.. {
            if cid >= updates.len() {
                break;
            }
            if allow_failure && split_off_refused(&updates, cid, &head_split_indices) {
                updates[cid].dropped = true;
                update_lines.push(Vec::new());
                continue;
            }
            // `split_head_update()` runs before the reference is locked and
            // `split_symref_update()` once it is (refs/files-backend.c:2699-2702, :2781-2793).
            let mut head_conflict = None;
            let mut symref_conflict = None;
            if allow_failure {
                if let Some(head_ref) = &head_ref {
                    head_conflict = split_head_update(
                        &mut updates,
                        cid,
                        head_ref,
                        &head_name,
                        &refnames,
                        &mut head_split_indices,
                        self.options.log_symref_updates,
                        kind,
                    );
                }
                if head_conflict.is_none() && !ref_locks.contains(&updates[cid].name()) {
                    symref_conflict = split_symref_update(store, &mut updates, cid, &mut refnames);
                }
            }
            update_lines.push(match &packing_lines {
                Some(_) => Vec::new(),
                None => {
                    let edit = &updates[cid];
                    let mut line = Vec::new();
                    // An update whose split was refused was never made log-only.
                    let update = match &symref_conflict {
                        Some((_, original)) => &RefEdit {
                            change: original.clone(),
                            ..edit.update.clone()
                        },
                        None => &edit.update,
                    };
                    file::transaction::edit_hook_line(&mut line, kind, update, edit.verify_only);
                    line
                }
            });
            if let Some(message) = head_conflict {
                reject(
                    &mut updates,
                    cid,
                    ErrorKind::NameConflict,
                    message,
                    &mut rejections,
                    &mut refnames,
                    kind,
                );
                continue;
            }
            let (locked, refusal) = if allow_failure && ref_locks.contains(&updates[cid].name()) {
                let head_split = head_split_indices.iter().any(|&(_, head)| head == cid);
                match self.reused_lock(&updates, cid, packed_for_lookups, head_split) {
                    Ok(refusal) => (Ok(()), refusal),
                    Err(err) => (Err(err), None),
                }
            } else {
                let locked = Self::lock_ref_and_apply_change(
                    self.store,
                    ref_files_lock_fail_mode,
                    packed_for_lookups,
                    &mut updates[cid],
                    matches!(
                        self.packed_refs,
                        PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_)
                    ),
                    self.options.must_not_exist_strictly,
                );
                // The lock stays in `ref_locks` once `lock_raw_ref()` took it, whatever
                // `check_old_oid()` says after (refs/files-backend.c:2729-2733).
                if matches!(
                    locked,
                    Ok(()) | Err(Error::ReferenceOutOfDate { .. } | Error::MustNotExist { .. })
                ) {
                    ref_locks.insert(updates[cid].name());
                }
                // `split_symref_update()` refused to split the update, which keeps its values
                // (refs/files-backend.c:2567-2575); it is reached only once the lock was taken.
                let symref_conflict = symref_conflict.and_then(|(message, original)| {
                    updates[cid].update.change = original;
                    locked.is_ok().then_some((ErrorKind::NameConflict, message))
                });
                // `ref_transaction_maybe_set_rejected()` once `lock_ref_for_update()` failed
                // (refs/files-backend.c:3021-3033): an error about the update's values refuses
                // the update alone, any other fails the transaction.
                let refusal = match &locked {
                    Ok(()) if symref_conflict.is_some() => symref_conflict,
                    Ok(()) => self.check_locked_name(&updates, cid, packed_for_lookups, &mut refnames_to_check),
                    Err(err) => self.refusal(&updates, cid, err, packed_for_lookups),
                };
                (locked, refusal)
            };
            let locked = match refusal {
                Some((kind, message)) if allow_failure => {
                    reject(
                        &mut updates,
                        cid,
                        kind,
                        message,
                        &mut rejections,
                        &mut refnames,
                        self.store.object_hash,
                    );
                    continue;
                }
                // Any other transaction fails on `lock_raw_ref()`'s name conflict with
                // the text `refs_verify_refname_available()` gave it
                // (refs/files-backend.c:795-829, :903-934), which the other refusals
                // already carry in their own error.
                Some((kind @ ErrorKind::NameConflict, message)) => Err(Error::Reftable {
                    kind,
                    message: message.into(),
                }),
                _ => locked,
            };
            if let Err(err) = locked {
                let err = match err {
                    Error::LockAcquire {
                        source,
                        full_name: _bogus,
                    } => Error::LockAcquire {
                        source,
                        full_name: original_name(&updates, cid),
                    },
                    other => other,
                };
                // `files_transaction_cleanup()` aborts the nested `packed-refs`
                // transaction holding the deletions locked so far
                // (refs/files-backend.c:2930-2936).
                let packed_lines = packed_deletion_lines(kind, &updates[..cid]);
                if !packed_lines.is_empty() {
                    file::transaction_hook(self.store, "aborted", &packed_lines);
                }
                return Err(err);
            }

            // traverse parent chain from leaf/peeled ref and set the leaf previous oid accordingly
            // to help with their reflog entries
            let change = &updates[cid];
            if let (Some(crate::TargetRef::Object(oid)), Some(parent_idx)) =
                (change.update.change.previous_value(), change.parent_index)
            {
                let oid = oid.to_owned();
                let mut parent_idx_cursor = Some(parent_idx);
                while let Some(parent) = parent_idx_cursor.take().map(|idx| &mut updates[idx]) {
                    parent_idx_cursor = parent.parent_index;
                    parent.leaf_referent_previous_oid = Some(oid);
                }
            }
        }

        // `refs_verify_refnames_available(refs->packed_ref_store, &refnames_to_check,
        // &transaction->refnames, NULL, transaction, 0, err)` (refs/files-backend.c:3064-3082):
        // a reference created loose must not clash with one in `packed-refs` either, and
        // under `REF_TRANSACTION_ALLOW_FAILURE` the clash refuses that update alone;
        // otherwise it fails the transaction with `REF_TRANSACTION_ERROR_NAME_CONFLICT`.
        for cid in refnames_to_check {
            if updates[cid].rejected {
                continue;
            }
            let extras = transaction_refnames(&updates);
            let name = updates[cid].update.name.as_bstr().to_owned();
            let Some(message) = refname_conflict(self.store, packed_for_lookups, false, name.as_ref(), &extras) else {
                continue;
            };
            if !allow_failure {
                // `files_transaction_cleanup()` aborts the nested `packed-refs`
                // transaction holding the deletions (refs/files-backend.c:2930-2936).
                let packed_lines = packed_deletion_lines(kind, &updates);
                if !packed_lines.is_empty() {
                    file::transaction_hook(self.store, "aborted", &packed_lines);
                }
                return Err(Error::Reftable {
                    kind: ErrorKind::NameConflict,
                    message: message.into(),
                });
            }
            reject(
                &mut updates,
                cid,
                ErrorKind::NameConflict,
                message,
                &mut rejections,
                &mut refnames,
                self.store.object_hash,
            );
        }
        let prepared_lines = packing_lines.unwrap_or_else(|| {
            updates
                .iter()
                .zip(&update_lines)
                .filter(|(edit, _)| !edit.dropped)
                .flat_map(|(_, line)| line.iter().copied())
                .collect()
        });

        // `lock_ref_for_update()`: a `REF_LOG_VIA_SPLIT` update takes `update->old_oid` under
        // `REF_HAVE_OLD` and copies `old_oid` from its parent lock otherwise
        // (refs/files-backend.c:2758-2762), so the HEAD entry reads exactly like the branch
        // entry it mirrors. A create has `REF_HAVE_OLD` with the null id, which a refused one
        // still logs from.
        for (branch_idx, head_idx) in head_split_indices {
            let expected = match &updates[branch_idx].update.change {
                Change::Update { expected, .. } | Change::Delete { expected, .. } => expected,
            };
            let old = match expected {
                PreviousValue::MustNotExist => Some(kind.null()),
                _ => match updates[branch_idx].update.change.previous_value() {
                    Some(crate::TargetRef::Object(oid)) => Some(oid.to_owned()),
                    _ => None,
                },
            };
            if let Some(old) = old {
                updates[head_idx].leaf_referent_previous_oid = Some(old);
            }
        }
        self.rejections = rejections;
        if allow_failure {
            self.prepare_packed_transaction(&updates, packed_refs_lock_fail_mode)?;
        }
        let packed_for_lookups = self
            .packed_transaction
            .as_ref()
            .and_then(packed::Transaction::buffer)
            .or_else(|| self.packed_buffer.as_ref().map(|b| &***b));

        // The nested `packed-refs` transaction `files_transaction_prepare()` opens for
        // the deletions (refs/files-backend.c:3036-3058, :3084-3118): prepared, and so
        // seen by the hook at `preparing` and `prepared`, when `packed-refs` holds one of
        // the references, and aborted straight away otherwise.
        let packed_lines = packed_deletion_lines(kind, &updates);
        let mut packed_feed = None;
        if !packed_lines.is_empty() {
            let needed = {
                let snapshot;
                let buffer = match packed_for_lookups {
                    Some(buffer) => Some(buffer),
                    None => {
                        snapshot = self.store.assure_packed_refs_uptodate().ok().flatten();
                        snapshot.as_ref().map(|b| &***b)
                    }
                };
                buffer.is_some_and(|buffer| {
                    updates.iter().any(|edit| {
                        is_packed_deletion(edit)
                            && possibly_adjust_name_for_prefixes(edit.update.name.as_ref())
                                .is_some_and(|name| matches!(buffer.try_find(name.as_ref()), Ok(Some(_))))
                    })
                })
            };
            if needed {
                for state in ["preparing", "prepared"] {
                    if !file::transaction_hook(self.store, state, &packed_lines) {
                        self.updates = Some(updates);
                        let store = self.store;
                        drop(self);
                        file::transaction_hook_refused(store, state, &[&packed_lines]);
                    }
                }
                packed_feed = Some(packed_lines);
            } else {
                file::transaction_hook(self.store, "aborted", &packed_lines);
            }
        }

        self.updates = Some(updates);
        // `ref_transaction_prepare()` runs the hook at `prepared` once the backend has
        // prepared, and a refusal aborts the transaction — the nested one first — and
        // dies (refs.c:2723-2727).
        if !file::transaction_hook(self.store, "prepared", &prepared_lines) {
            let store = self.store;
            drop(self);
            let mut aborted: Vec<&[u8]> = packed_feed.iter().map(Vec::as_slice).collect();
            aborted.push(&prepared_lines);
            file::transaction_hook_refused(store, "prepared", &aborted);
        }
        self.hook = Some(file::transaction::HookFeed {
            lines: prepared_lines,
            packed: packed_feed,
        });
        Ok(self)
    }

    /// The nested `packed-refs` transaction of `updates`: the deletions it has to make there,
    /// or the values migrated into it, and an unlocked snapshot for the edits that only read it.
    fn prepare_packed_transaction(
        &mut self,
        updates: &[Edit],
        packed_refs_lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<(), Error> {
        let mut maybe_updates_for_packed_refs = match self.packed_refs {
            PackedRefs::DeletionsAndNonSymbolicUpdates(_)
            | PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_) => Some(0_usize),
            PackedRefs::DeletionsOnly => None,
        };
        if maybe_updates_for_packed_refs.is_some()
            || self.store.packed_refs_path().is_file()
            || self.store.packed_refs_lock_path().is_file()
        {
            let mut edits_for_packed_transaction = Vec::<RefEdit>::new();
            let mut needs_packed_refs_lookups = false;
            for edit in updates {
                // A refused update takes no part in the `packed-refs` transaction
                // (`continue` after `ref_transaction_maybe_set_rejected()`,
                // refs/files-backend.c:3027-3031).
                if edit.rejected || edit.dropped {
                    continue;
                }
                // A verification writes nothing, but reads the value `packed-refs` holds.
                if edit.verify_only {
                    needs_packed_refs_lookups = true;
                    continue;
                }
                let log_mode = match edit.update.change {
                    Change::Update {
                        log: LogChange { mode, .. },
                        ..
                    } => mode,
                    Change::Delete { log, .. } => log,
                };
                if log_mode == RefLog::Only {
                    continue;
                }
                let name = match possibly_adjust_name_for_prefixes(edit.update.name.as_ref()) {
                    Some(n) => n,
                    None => continue,
                };
                if let Some(ref mut num_updates) = maybe_updates_for_packed_refs {
                    if let Change::Update {
                        new: Target::Object(_), ..
                    } = edit.update.change
                    {
                        edits_for_packed_transaction.push(RefEdit {
                            name,
                            ..edit.update.clone()
                        });
                        *num_updates += 1;
                        continue;
                    }
                }
                match edit.update.change {
                    Change::Update {
                        expected: PreviousValue::ExistingMustMatch(_) | PreviousValue::MustExistAndMatch(_),
                        ..
                    } => needs_packed_refs_lookups = true,
                    Change::Delete { .. } => {
                        edits_for_packed_transaction.push(RefEdit {
                            name,
                            ..edit.update.clone()
                        });
                    }
                    _ => {
                        needs_packed_refs_lookups = true;
                    }
                }
            }

            // `packed-refs.lock` is taken if and only if we have an edit to *write* into
            // `packed-refs`, which is precisely git's rule.
            //
            // `files_transaction_prepare()` builds its `packed_transaction` only from updates that
            // delete a reference (`refs/files-backend.c:2982-3007`):
            //
            //     if (update->flags & REF_DELETING &&
            //         !(update->flags & REF_LOG_ONLY) &&
            //         !(update->flags & REF_IS_PRUNING)) {
            //             /*
            //              * This reference has to be deleted from
            //              * packed-refs if it exists there.
            //              */
            //
            // and takes the lock only if that transaction came into being (`:3031-3036`):
            //
            //     if (packed_transaction) {
            //             if (packed_refs_lock(refs->packed_ref_store, 0, err)) {
            //
            // `edits_for_packed_transaction` is that same set: deletions, plus the values a
            // `pack-refs`-style transaction migrates into `packed-refs` — which is the other place
            // git locks, `files_pack_refs()` at `:1478`.
            //
            // `needs_packed_refs_lookups` deliberately does *not* take part in this decision.
            // Creating a loose reference, or updating one whose current value happens to live in
            // `packed-refs`, only ever *reads* the file, and git does that read unlocked on
            // purpose (`:3010-3023`):
            //
            //     * Verify that none of the loose reference that we're about to write
            //     * conflict with any existing packed references. Ideally, we'd do this
            //     * check after the packed-refs are locked so that the file cannot
            //     * change underneath our feet. But introducing such a lock now would
            //     * probably do more harm than good as users rely on there not being a
            //     * global lock with the "files" backend.
            //     …
            //     * So instead, we accept the race for now.
            //
            // Taking the lock for a read is what let one `packed-refs.lock` — a concurrent
            // `pack-refs`, or one left behind by a killed process — fail every reference creation
            // in the repository, where stock git creates the loose file and succeeds.
            if !edits_for_packed_transaction.is_empty() {
                let packed_transaction: Option<_> =
                    if maybe_updates_for_packed_refs.unwrap_or(0) > 0 || self.store.packed_refs_lock_path().is_file() {
                        // We have to create a packed-ref even if it doesn't exist.
                        //
                        // The `packed-refs.lock` probe stands in for git locking unconditionally
                        // once it has a deletion to make: a lock that is currently held means a
                        // `packed-refs` may be about to appear, and git keeps its own lock for the
                        // whole transaction so "somebody else doesn't pack a reference that we are
                        // trying to delete" (`refs/files-backend.c:3053-3056`).
                        self.store
                            .packed_transaction(packed_refs_lock_fail_mode)
                            .map_err(|err| match err {
                                file::packed::transaction::Error::BufferOpen(err) => Error::from(err),
                                file::packed::transaction::Error::TransactionLock(err) => {
                                    Error::PackedTransactionAcquire(err)
                                }
                            })?
                            .into()
                    } else {
                        // A packed transaction is optional - we only have deletions that can't be made if
                        // no packed-ref file exists anyway
                        self.store
                            .assure_packed_refs_uptodate()?
                            .map(|p| {
                                buffer_into_transaction(
                                    p,
                                    packed_refs_lock_fail_mode,
                                    self.store.precompose_unicode,
                                    self.store.namespace.clone(),
                                )
                                .map_err(Error::PackedTransactionAcquire)
                            })
                            .transpose()?
                    };
                if let Some(transaction) = packed_transaction {
                    self.packed_transaction = Some(match &mut self.packed_refs {
                        PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(f)
                        | PackedRefs::DeletionsAndNonSymbolicUpdates(f) => {
                            transaction.prepare(&mut edits_for_packed_transaction.into_iter(), &**f)?
                        }
                        PackedRefs::DeletionsOnly => transaction
                            .prepare(&mut edits_for_packed_transaction.into_iter(), &gix_object::find::Never)?,
                    });
                }
            }

            // Whatever is left that still has to consult `packed-refs` gets a plain snapshot.
            // `Store::assure_packed_refs_uptodate()` opens the file for reading and takes no lock,
            // so this is the unlocked read quoted above. A packed transaction, when we have one,
            // already carries a buffer read under the lock and wins.
            if self.packed_transaction.is_none() && needs_packed_refs_lookups {
                self.packed_buffer = self.store.assure_packed_refs_uptodate()?;
            }
        }
        Ok(())
    }

    /// The hook at `preparing` over the caller's `edits` (refs.c:2711-2717); a
    /// refusal aborts the transaction, still open, and dies.
    ///
    /// `ref_update_reject_duplicates()` refuses a transaction naming a reference
    /// twice before the hook runs (refs.c:2708-2709), so such a transaction does
    /// not reach it here either.
    fn run_preparing_hook(&self, edits: &[RefEdit]) {
        let mut names: Vec<_> = edits
            .iter()
            .filter(|edit| log_mode_of(&edit.change) != RefLog::Only)
            .map(|edit| &edit.name)
            .collect();
        names.sort();
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return;
        }
        let mut lines = Vec::new();
        for (idx, edit) in edits.iter().enumerate() {
            let verify_only = self.options.verify_only.contains(&idx);
            file::transaction::edit_hook_line(&mut lines, self.store.object_hash, edit, verify_only);
        }
        if !file::transaction_hook(self.store, "preparing", &lines) {
            file::transaction_hook_refused(self.store, "preparing", &[&lines]);
        }
    }

    /// Rollback all intermediate state and return the `RefEdits` as we know them thus far.
    ///
    /// Note that they have been altered compared to what was initially provided as they have
    /// been split and know about their current state on disk.
    ///
    /// # Note
    ///
    /// A rollback happens automatically as this instance is dropped as well.
    pub fn rollback(self) -> Vec<RefEdit> {
        self.updates
            .map(|updates| updates.into_iter().map(|u| u.update).collect())
            .unwrap_or_default()
    }

    /// Prepare `edits` in a store with the reftable backend, see
    /// [`reftable_be_transaction_prepare()`](crate::store_impl::reftable::Backend).
    ///
    /// The prepared state is kept by the transaction; the edits are what
    /// [`rollback()`](Self::rollback()) reports, splits included.
    fn prepare_reftable(
        mut self,
        backend: &crate::store_impl::reftable::Backend,
        edits: Vec<RefEdit>,
    ) -> Result<Self, Error> {
        let objects = match &self.packed_refs {
            PackedRefs::DeletionsAndNonSymbolicUpdates(objects)
            | PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(objects) => Some(&**objects),
            PackedRefs::DeletionsOnly => None,
        };
        // `transaction_refname_valid()` refuses a pseudoref when the update is
        // queued (refs.c:1368-1394), so a transaction holding one never reaches
        // the hook; the backend reports the refusal below.
        if !edits
            .iter()
            .any(|edit| crate::store_impl::reftable::is_pseudo_ref(edit.name.as_bstr()))
        {
            self.run_preparing_hook(&edits);
        }
        let data = backend.transaction_prepare(
            edits,
            self.store.object_hash,
            self.store.write_reflog,
            self.store.namespace.as_ref(),
            objects,
            &self.options,
        )?;
        let updates: Vec<Edit> = data
            .edits()
            .into_iter()
            .map(|update| Edit {
                update,
                lock: None,
                parent_index: None,
                leaf_referent_previous_oid: None,
                log_only_split: false,
                previous_is_symbolic: false,
                rejected: false,
                dropped: false,
                verify_only: false,
            })
            .collect();
        self.updates = Some(updates);
        let lines = data.hook_stdin();
        if !file::transaction_hook(self.store, "prepared", &lines) {
            let store = self.store;
            drop(data);
            drop(self);
            file::transaction_hook_refused(store, "prepared", &[&lines]);
        }
        self.hook = Some(file::transaction::HookFeed { lines, packed: None });
        self.reftable = Some(data);
        Ok(self)
    }
}

/// A transaction that may fail partially, git's `REF_TRANSACTION_ALLOW_FAILURE`:
/// which failures of `lock_ref_for_update()` refuse just the update.
impl Transaction<'_, '_> {
    /// The error code and text `lock_ref_for_update()` fails update `cid` with for
    /// `err`, when it is one `ref_transaction_maybe_set_rejected()` takes: anything but
    /// `REF_TRANSACTION_ERROR_GENERIC` (refs.c:1290-1295). Each message names the update
    /// the caller queued, `ref_update_original_update_refname()` (refs.c:3150-3156).
    fn refusal(
        &self,
        updates: &[Edit],
        cid: usize,
        err: &Error,
        packed: Option<&packed::Buffer>,
    ) -> Option<(ErrorKind, String)> {
        let name = updates[cid].update.name.as_bstr();
        let original = original_name(updates, cid);
        let (kind, reason) = match err {
            // `lock_raw_ref()` with `mustexist` (refs/files-backend.c:885-897, :922-930).
            Error::DeleteReferenceMustExist { .. } | Error::MustExist { .. } => (
                ErrorKind::NonexistentRef,
                format!("unable to resolve reference '{name}'"),
            ),
            // `check_old_oid()` (refs/files-backend.c:2617-2667).
            Error::MustNotExist { .. } => (ErrorKind::CreateExists, "reference already exists".into()),
            Error::ReferenceOutOfDate {
                expected: Target::Object(expected),
                actual,
                ..
            } => {
                let current = match actual {
                    Target::Object(id) => Some(*id),
                    Target::Symbolic(referent) => Self::resolve_symref_chain(self.store, referent, packed),
                };
                match current {
                    Some(current) => (
                        ErrorKind::IncorrectOldValue,
                        format!("is at {current} but expected {expected}"),
                    ),
                    None => (
                        ErrorKind::NonexistentRef,
                        format!("reference is missing but expected {expected}"),
                    ),
                }
            }
            // `unable_to_lock_message()` of a lock someone else holds, which
            // `lock_raw_ref()` reports as `REF_TRANSACTION_ERROR_CREATE_EXISTS`
            // (refs/files-backend.c:845-866, lockfile.c:254-299).
            Error::LockAcquire {
                source: gix_lock::acquire::Error::PermanentlyLocked {
                    resource_path, holder, ..
                },
                ..
            } => {
                let absolute = std::path::absolute(resource_path).unwrap_or_else(|_| resource_path.clone());
                (
                    ErrorKind::CreateExists,
                    format!(
                        "Unable to create '{}.lock': File exists.\n\n{holder}",
                        absolute.display()
                    ),
                )
            }
            // `safe_create_leading_directories()` meeting a file where a directory has
            // to be, `SCLD_EXISTS`, which `refs_verify_refname_available()` explains
            // (refs/files-backend.c:795-829).
            Error::Io(_) => {
                let extras = transaction_refnames(updates);
                let message = refname_conflict(self.store, packed, true, name, &extras)?;
                (ErrorKind::NameConflict, message)
            }
            _ => return None,
        };
        Some((kind, format!("cannot lock ref '{original}': {reason}")))
    }

    /// What `lock_raw_ref()` checks once the lock on update `cid` is taken: a
    /// directory where the reference should be that holds other references is a name
    /// conflict (`EISDIR`, refs/files-backend.c:903-934), and a reference that does not
    /// exist is queued in `refnames_to_check` for the `packed-refs` check (:974-996).
    ///
    /// `files_read_raw_ref()` reads a reference standing behind such a directory from
    /// `packed-refs` (refs/files-backend.c:593-609), so the directory is in the way only of
    /// a reference that is not packed either.
    fn check_locked_name(
        &self,
        updates: &[Edit],
        cid: usize,
        packed: Option<&packed::Buffer>,
        refnames_to_check: &mut Vec<usize>,
    ) -> Option<(ErrorKind, String)> {
        let edit = &updates[cid];
        let name = edit.update.name.as_ref();
        let missing = matches!(Self::read_existing_ref(self.store, name, packed), Ok(None));
        if missing && self.store.reference_path(name).is_dir() {
            let extras = transaction_refnames(updates);
            let message = refname_conflict(self.store, packed, true, name.as_bstr(), &extras)?;
            let original = original_name(updates, cid);
            return Some((ErrorKind::NameConflict, format!("cannot lock ref '{original}': {message}")));
        }
        if missing {
            refnames_to_check.push(cid);
        }
        None
    }

    /// `lock_ref_for_update()` for update `cid` of a reference an earlier update of the
    /// transaction locked: the lock in `backend_data->ref_locks` is reused, and
    /// `lock_raw_ref()` never sets the update's `type` (refs/files-backend.c:2715-2733).
    ///
    /// So the log-only `HEAD` update `split_head_update()` added reads as no symref, `HEAD
    /// has been racily updated` (:2737-2760), and any other update takes the regular-ref
    /// branch: its old value is checked against `lock->old_oid` (`check_old_oid()`,
    /// :2615-2667), and a new value other than that one is written to a lock the first
    /// update already closed, which fails (`write_ref_to_lockfile()`, :2056-2076). Both
    /// failures are `REF_TRANSACTION_ERROR_GENERIC` and fail the transaction; a refused old
    /// value is returned for the update alone.
    fn reused_lock(
        &self,
        updates: &[Edit],
        cid: usize,
        packed: Option<&packed::Buffer>,
        head_split: bool,
    ) -> Result<Option<(ErrorKind, String)>, Error> {
        let generic = |message: String| Error::Reftable {
            kind: ErrorKind::Generic,
            message: message.into(),
        };
        if head_split {
            return Err(generic("HEAD has been racily updated".into()));
        }
        let edit = &updates[cid];
        let name = edit.update.name.as_ref();
        // `lock->old_oid`: what the first update read, as nothing was written since.
        let current = match Self::read_existing_ref(self.store, name, packed)? {
            Some(Reference {
                target: Target::Object(id),
                ..
            }) => Some(id),
            Some(Reference {
                target: Target::Symbolic(referent),
                ..
            }) => Self::resolve_symref_chain(self.store, &referent, packed),
            None => None,
        }
        .unwrap_or_else(|| self.store.object_hash.null());
        let original = original_name(updates, cid);
        let expected = match &edit.update.change {
            Change::Update { expected, .. } | Change::Delete { expected, .. } => expected,
        };
        let refusal = match expected {
            PreviousValue::MustExistAndMatch(Target::Symbolic(target))
            | PreviousValue::ExistingMustMatch(Target::Symbolic(target)) => Some((
                ErrorKind::ExpectedSymref,
                format!(
                    "cannot lock ref '{original}': expected symref with target '{}': but is a regular ref",
                    target.as_bstr()
                ),
            )),
            PreviousValue::MustExistAndMatch(Target::Object(old))
            | PreviousValue::ExistingMustMatch(Target::Object(old))
                if *old != current =>
            {
                Some(match current.is_null() {
                    true => (
                        ErrorKind::NonexistentRef,
                        format!("cannot lock ref '{original}': reference is missing but expected {old}"),
                    ),
                    false => (
                        ErrorKind::IncorrectOldValue,
                        format!("cannot lock ref '{original}': is at {current} but expected {old}"),
                    ),
                })
            }
            PreviousValue::MustNotExist if !current.is_null() => Some((
                ErrorKind::CreateExists,
                format!("cannot lock ref '{original}': reference already exists"),
            )),
            _ => None,
        };
        if refusal.is_some() {
            return Ok(refusal);
        }
        if let Change::Update {
            log,
            new: Target::Object(new),
            ..
        } = &edit.update.change
        {
            if !edit.verify_only && log.mode != RefLog::Only && *new != current {
                let path = self.store.reference_path(name);
                let path = std::path::absolute(&path).unwrap_or(path);
                return Err(generic(format!(
                    "cannot update ref '{}': couldn't write '{}.lock'",
                    name.as_bstr(),
                    path.display()
                )));
            }
        }
        Ok(None)
    }
}

/// `ref_transaction_maybe_set_rejected()` (refs.c:1276-1312) for update `cid` of a
/// transaction that may fail partially: record why, and release its lock, which
/// `files_transaction_cleanup()` would (refs/files-backend.c:2915-2929). Its parents
/// never had the old value copied to their locks (:2792-2803), so their log entries
/// carry the null id. Its name leaves `refnames`, `transaction->refnames` (refs.c:1297-1302).
fn reject(
    updates: &mut [Edit],
    cid: usize,
    kind: ErrorKind,
    message: String,
    rejections: &mut Vec<Rejection>,
    refnames: &mut Vec<BString>,
    hash: gix_hash::Kind,
) {
    let edit = &mut updates[cid];
    // `string_list_remove()` (string-list.c:70-85).
    if let Some(pos) = string_list_find(refnames, edit.update.name.as_bstr()) {
        refnames.remove(pos);
    }
    edit.rejected = true;
    edit.lock = None;
    let expected = match &edit.update.change {
        Change::Update { expected, .. } | Change::Delete { expected, .. } => expected,
    };
    // `REF_HAVE_OLD`, with the null id for a reference that must not exist yet.
    let (old_oid, old_target) = match expected {
        PreviousValue::MustExistAndMatch(Target::Object(oid)) => (Some(*oid), None),
        PreviousValue::MustExistAndMatch(Target::Symbolic(name)) => (None, Some(name.as_bstr().to_owned())),
        PreviousValue::MustNotExist => (Some(hash.null()), None),
        _ => (None, None),
    };
    // A verification has no `REF_HAVE_NEW`.
    let (new_oid, new_target) = match &edit.update.change {
        Change::Update { .. } if edit.verify_only => (None, None),
        Change::Update {
            new: Target::Object(oid), ..
        } => (Some(*oid), None),
        Change::Update {
            new: Target::Symbolic(name),
            ..
        } => (None, Some(name.as_bstr().to_owned())),
        Change::Delete { .. } => (Some(gix_hash::ObjectId::null(hash)), None),
    };
    rejections.push(Rejection {
        refname: edit.update.name.as_bstr().to_owned(),
        new_oid,
        old_oid,
        new_target,
        old_target,
        kind,
        message: message.into(),
    });
    let mut cursor = updates[cid].parent_index;
    while let Some(parent) = cursor {
        updates[parent].leaf_referent_previous_oid = None;
        cursor = updates[parent].parent_index;
    }
}

/// Whether update `cid` was split off one that was refused before it could be split
/// (see [`Edit::dropped`]): the referent of a symbolic reference, or the `HEAD`
/// mirror of a branch update that was itself dropped.
fn split_off_refused(updates: &[Edit], cid: usize, head_split_indices: &[(usize, usize)]) -> bool {
    match updates[cid].parent_index {
        Some(parent) => updates[parent].rejected || updates[parent].dropped,
        None => head_split_indices
            .iter()
            .any(|&(branch, head)| head == cid && updates[branch].dropped),
    }
}

/// Whether update `idx` gets the log-only `HEAD` update of `split_head_update()`
/// (refs/files-backend.c:2499-2513): it updates `head_ref`, is not `REF_LOG_ONLY`, and
/// did not come through `HEAD`, `REF_UPDATE_VIA_HEAD`.
fn needs_head_split(updates: &[Edit], idx: usize, head_ref: &FullName) -> bool {
    updates[idx].update.name == *head_ref
        && log_mode_of(&updates[idx].update.change) != RefLog::Only
        && !has_head_ancestor(updates, idx)
}

/// The log-only `HEAD` update `split_head_update()` adds for `branch`:
///
/// ```c
/// new_update = ref_transaction_add_update(
///                 transaction, "HEAD",
///                 update->flags | REF_LOG_ONLY | REF_NO_DEREF | REF_LOG_VIA_SPLIT,
///                 &update->new_oid, &update->old_oid, &update->peeled,
///                 NULL, NULL, update->committer_info, update->msg);
/// ```
///
/// (refs/files-backend.c:2531-2535). It copies the value *and* the message, whichever
/// kind of change it is. A deletion mirrors as a deletion whose log survives — that is
/// the whole point of `REF_LOG_ONLY` — so `.git/logs/HEAD` gains a `<old> <null>` line
/// while `refs/heads/main` and its own log go away. `update->new_oid` is the null id
/// without `REF_HAVE_NEW`, a verification's, and with `update->new_target`, which the
/// mirror does not take: `null_new_for_symbolic` is set when a symbolic update is logged
/// as git logs it.
fn head_mirror(branch: &Edit, head_name: &FullName, null_new_for_symbolic: bool, hash: gix_hash::Kind) -> Edit {
    let (mirrored, log_only_split) = match &branch.update.change {
        Change::Update { log, new, .. } => {
            let null_new = branch.verify_only || (null_new_for_symbolic && matches!(new, Target::Symbolic(_)));
            (
                Change::Update {
                    log: LogChange {
                        mode: RefLog::Only,
                        force_create_reflog: log.force_create_reflog,
                        message: log.message.clone(),
                    },
                    expected: PreviousValue::Any,
                    new: match null_new {
                        true => Target::Object(hash.null()),
                        false => new.clone(),
                    },
                },
                false,
            )
        }
        Change::Delete { message, .. } => (
            Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::Only,
                message: message.clone(),
            },
            true,
        ),
    };
    Edit {
        update: RefEdit {
            change: mirrored,
            name: head_name.clone(),
            deref: false,
        },
        lock: None,
        parent_index: None,
        leaf_referent_previous_oid: None,
        previous_is_symbolic: false,
        rejected: false,
        dropped: false,
        // `REF_LOG_ONLY` on a deletion: append to the log and keep it, rather than gix's
        // "delete the log, keep the reference".
        log_only_split,
        verify_only: branch.verify_only,
    }
}

/// Hand a verification down to update `child`, split off `parent`: the child verifies in
/// its place, and the log-only parent logs the null id as its new value, the
/// `update->new_oid` of an update without `REF_HAVE_NEW` (refs/files-backend.c:2587-2592,
/// :3134-3185).
fn mark_verify_split(updates: &mut [Edit], child: usize, parent: usize, hash: gix_hash::Kind) {
    updates[child].verify_only = true;
    if let Change::Update { new, .. } = &mut updates[parent].update.change {
        *new = Target::Object(hash.null());
    }
}

/// `split_head_update()` (refs/files-backend.c:2499-2547) for update `cid` of a transaction
/// that may fail partially: add the log-only `HEAD` update of an update of `head_ref`, or
/// the message refusing the update when `HEAD` is already in the transaction.
fn split_head_update(
    updates: &mut Vec<Edit>,
    cid: usize,
    head_ref: &FullName,
    head_name: &FullName,
    refnames: &[BString],
    head_split_indices: &mut Vec<(usize, usize)>,
    log_symref_updates: bool,
    hash: gix_hash::Kind,
) -> Option<String> {
    if !needs_head_split(updates, cid, head_ref) {
        return None;
    }
    if string_list_find(refnames, "HEAD".into()).is_some() {
        return Some(format!(
            "multiple updates for 'HEAD' (including one via its referent '{}') are not allowed",
            updates[cid].update.name.as_bstr()
        ));
    }
    let mirror = head_mirror(&updates[cid], head_name, log_symref_updates, hash);
    updates.push(mirror);
    head_split_indices.push((cid, updates.len() - 1));
    None
}

/// `split_symref_update()` (refs/files-backend.c:2555-2607) for update `cid` of a transaction
/// that may fail partially, once it is dereferenced through a symbolic reference: the update
/// becomes `REF_LOG_ONLY` without its old value, and one of the referent with the values
/// is appended, which is split in turn when it is reached.
///
/// When the referent is already in the transaction the update is refused instead: the
/// message and the update's change as it was are returned, and the change is left as the
/// log-only half for the lock `lock_ref_for_update()` takes before it splits.
fn split_symref_update(
    store: &file::Store,
    updates: &mut Vec<Edit>,
    cid: usize,
    refnames: &mut Vec<BString>,
) -> Option<(String, Change)> {
    let edit = &mut updates[cid];
    if !std::mem::take(&mut edit.update.deref) {
        return None;
    }
    let symbolic_refs_are_never_packed = None;
    let Ok(Reference {
        target: Target::Symbolic(referent),
        ..
    }) = store.find_existing_inner(edit.update.name.as_ref().as_partial_name(), symbolic_refs_are_never_packed)
    else {
        return None;
    };
    let original = edit.update.change.clone();
    // "Change the symbolic ref update to log only. Also, it doesn't need to check its
    // old OID value, as that will be done when new_update is processed."
    let child = match &mut edit.update.change {
        Change::Delete {
            expected,
            log,
            message,
        } => Change::Delete {
            expected: std::mem::replace(expected, PreviousValue::Any),
            log: std::mem::replace(log, RefLog::Only),
            message: message.clone(),
        },
        Change::Update { log, expected, new } => Change::Update {
            log: std::mem::replace(
                log,
                LogChange {
                    message: log.message.clone(),
                    mode: RefLog::Only,
                    force_create_reflog: log.force_create_reflog,
                },
            ),
            expected: std::mem::replace(expected, PreviousValue::Any),
            new: new.clone(),
        },
    };
    if string_list_find(refnames, referent.as_bstr()).is_some() {
        return Some((
            format!(
                "multiple updates for '{}' (including one via symref '{}') are not allowed",
                referent.as_bstr(),
                edit.update.name.as_bstr()
            ),
            original,
        ));
    }
    // The parent of a deletion is the log-only mirror of it, see [`Edit::log_only_split`].
    edit.log_only_split = matches!(
        child,
        Change::Delete {
            log: RefLog::AndReference,
            ..
        }
    );
    refnames.push(referent.as_bstr().to_owned());
    let verify_only = edit.verify_only;
    updates.push(Edit {
        update: RefEdit {
            change: child,
            name: referent,
            deref: true,
        },
        lock: None,
        parent_index: Some(cid),
        leaf_referent_previous_oid: None,
        log_only_split: false,
        previous_is_symbolic: false,
        rejected: false,
        dropped: false,
        verify_only: false,
    });
    if verify_only {
        let child = updates.len() - 1;
        mark_verify_split(updates, child, cid, store.object_hash);
    }
    None
}

/// `get_entry_index()`'s exact match (string-list.c:17-40), which `string_list_has_string()`
/// and `string_list_remove()` use: a binary search that assumes `list` is sorted, whether
/// it still is or not once splits appended to it.
fn string_list_find(list: &[BString], name: &gix_object::bstr::BStr) -> Option<usize> {
    let (mut left, mut right) = (0, list.len());
    while left < right {
        let middle = left + (right - left) / 2;
        match name.cmp(gix_object::bstr::ByteSlice::as_bstr(list[middle].as_slice())) {
            std::cmp::Ordering::Less => right = middle,
            std::cmp::Ordering::Greater => left = middle + 1,
            std::cmp::Ordering::Equal => return Some(middle),
        }
    }
    None
}

/// `ref_update_original_update_refname()` (refs.c:3150-3156): the name of the update the
/// caller queued that update `cid` was split off.
fn original_name(updates: &[Edit], cid: usize) -> BString {
    let mut idx = cid;
    while let Some(parent) = updates[idx].parent_index {
        idx = parent;
    }
    updates[idx].name()
}

/// `transaction->refnames`: the names the transaction updates, without those refused
/// (refs.c:1296-1300) or never split off.
fn transaction_refnames(updates: &[Edit]) -> std::collections::BTreeSet<BString> {
    updates
        .iter()
        .filter(|edit| !edit.rejected && !edit.dropped)
        .map(Edit::name)
        .collect()
}

/// `refs_verify_refname_available()` (refs.c:2789-2968) of `refname` in a files store
/// for a transaction that is not the initial one: the message of the first conflict,
/// a leading directory that is a reference or one of `extras`, or an existing reference
/// or one of `extras` below `refname/`. With `loose` unset only `packed-refs` is
/// consulted, as `files_transaction_prepare()` asks of `refs->packed_ref_store`.
fn refname_conflict(
    store: &file::Store,
    packed: Option<&packed::Buffer>,
    loose: bool,
    refname: &gix_object::bstr::BStr,
    extras: &std::collections::BTreeSet<BString>,
) -> Option<String> {
    use gix_object::bstr::ByteSlice;
    // `refs_read_raw_ref()` succeeding; any error reading it counts as absent.
    let exists = |name: &gix_object::bstr::BStr| {
        let name = FullNameRef::new_unchecked(name);
        if loose {
            matches!(Transaction::read_existing_ref(store, name, packed), Ok(Some(_)))
        } else {
            packed.is_some_and(|packed| matches!(packed.try_find(name), Ok(Some(_))))
        }
    };
    for (slash, _) in refname.iter().enumerate().filter(|(_, b)| **b == b'/') {
        let dirname = refname[..slash].as_bstr();
        if exists(dirname) {
            return Some(format!("'{dirname}' exists; cannot create '{refname}'"));
        }
        if extras.contains(dirname) {
            return Some(format!("cannot process '{refname}' and '{dirname}' at the same time"));
        }
    }
    let mut prefix = BString::from(refname);
    prefix.push(b'/');
    // `REFS_FOR_EACH_INCLUDE_BROKEN`: a reference that does not parse still counts.
    let existing: Option<BString> = if loose {
        let relative: Result<&gix_path::RelativePath, _> = prefix.as_bstr().try_into();
        relative.ok().and_then(|relative| {
            store
                .iter_prefixed_packed(relative, packed)
                .ok()?
                .find_map(|r| r.ok().map(|r| r.name.as_bstr().to_owned()))
        })
    } else {
        packed.and_then(|packed| {
            packed
                .iter_prefixed(prefix.clone())
                .ok()?
                .find_map(|r| r.ok().map(|r| r.name.as_bstr().to_owned()))
        })
    };
    if let Some(existing) = existing {
        return Some(format!("'{existing}' exists; cannot create '{refname}'"));
    }
    // `find_descendant_ref()` (refs.c:1787-1811).
    extras
        .range(prefix.clone()..)
        .take_while(|extra| extra.starts_with(&prefix))
        .next()
        .map(|extra| format!("cannot process '{refname}' and '{extra}' at the same time"))
}

/// The reflog mode an edit carries, whichever kind of change it is.
fn log_mode_of(change: &Change) -> RefLog {
    match change {
        Change::Update {
            log: LogChange { mode, .. },
            ..
        } => *mode,
        Change::Delete { log, .. } => *log,
    }
}

/// An update `files_transaction_prepare()` queues in its nested `packed-refs`
/// transaction: `REF_DELETING && !REF_LOG_ONLY && !REF_IS_PRUNING`
/// (refs/files-backend.c:3036-3039). A refused update is not queued (:3027-3031).
fn is_packed_deletion(edit: &Edit) -> bool {
    !edit.rejected && !edit.dropped && matches!(edit.update.change, Change::Delete { log, .. } if log != RefLog::Only)
}

/// The hook's lines for the nested `packed-refs` transaction of `updates`: each
/// deletion queued as `REF_HAVE_NEW | REF_NO_DEREF` with the null id and no old
/// value (refs/files-backend.c:3054-3058).
fn packed_deletion_lines(kind: gix_hash::Kind, updates: &[Edit]) -> Vec<u8> {
    let mut lines = Vec::new();
    for edit in updates.iter().filter(|edit| is_packed_deletion(edit)) {
        file::transaction::hook_line(&mut lines, kind, None, None, edit.update.name.as_bstr());
    }
    lines
}

/// Whether `idx` was split off (transitively) from an edit named `HEAD`.
///
/// This is git's `REF_UPDATE_VIA_HEAD`, which `split_symref_update()` sets when the symref
/// being dereferenced is `HEAD` and which propagates to further splits through `new_flags`.
fn has_head_ancestor(updates: &[Edit], idx: usize) -> bool {
    let mut cursor = updates[idx].parent_index;
    while let Some(parent_idx) = cursor {
        if updates[parent_idx].update.name.as_bstr() == "HEAD" {
            return true;
        }
        cursor = updates[parent_idx].parent_index;
    }
    false
}

fn possibly_adjust_name_for_prefixes(name: &FullNameRef) -> Option<FullName> {
    match name.category_and_short_name() {
        Some((c, sn)) => {
            use crate::Category::*;
            let sn = FullNameRef::new_unchecked(sn);
            match c {
                Bisect | Rewritten | WorktreePrivate | LinkedPseudoRef { .. } | PseudoRef | MainPseudoRef => None,
                Tag | LocalBranch | RemoteBranch | Note => name.into(),
                MainRef | LinkedRef { .. } => sn
                    .category()
                    .is_some_and(|cat| !cat.is_worktree_private())
                    .then_some(sn),
            }
            .map(ToOwned::to_owned)
        }
        None => Some(name.to_owned()), // allow (uncategorized/very special) refs to be packed
    }
}

mod error {
    use gix_object::bstr::BString;

    use crate::{
        Target,
        store_impl::{file, packed},
    };

    /// The error returned by various [`Transaction`][super::Transaction] methods.
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("The packed ref buffer could not be loaded")]
        Packed(#[from] packed::buffer::open::Error),
        #[error("The lock for the packed-ref file could not be obtained")]
        PackedTransactionAcquire(#[source] gix_lock::acquire::Error),
        #[error("The packed transaction could not be prepared")]
        PackedTransactionPrepare(#[from] packed::transaction::prepare::Error),
        #[error("The packed ref file could not be parsed")]
        PackedFind(#[from] packed::find::Error),
        #[error("Edit preprocessing failed with an error")]
        PreprocessingFailed(#[source] std::io::Error),
        #[error("A lock could not be obtained for reference {full_name:?}")]
        LockAcquire {
            source: gix_lock::acquire::Error,
            full_name: BString,
        },
        #[error("An IO error occurred while applying an edit")]
        Io(#[from] std::io::Error),
        #[error("The reference {full_name:?} for deletion did not exist or could not be parsed")]
        DeleteReferenceMustExist { full_name: BString },
        #[error(
            "Reference {full_name:?} was not supposed to exist when writing it with value {new:?}, but actual content was {actual:?}"
        )]
        MustNotExist {
            full_name: BString,
            actual: Target,
            new: Target,
        },
        #[error("Reference {full_name:?} was supposed to exist with value {expected}, but didn't.")]
        MustExist { full_name: BString, expected: Target },
        #[error("The reference {full_name:?} should have content {expected}, actual content was {actual}")]
        ReferenceOutOfDate {
            full_name: BString,
            expected: Target,
            actual: Target,
        },
        #[error("Could not read reference")]
        ReferenceDecode(#[from] file::loose::reference::decode::Error),
        /// A store with the reftable backend refused the transaction, or the files backend
        /// refused it for a name conflict; `message` is git's error text verbatim, like
        /// `cannot lock ref 'refs/heads/main': is at <id> but expected <id>`.
        #[error("{message}")]
        Reftable {
            kind: file::transaction::ErrorKind,
            message: BString,
        },
    }
}

pub use error::Error;

/// The directory a released ref lock may prune up to, exclusively: `refs/<kind>/`
/// for a ref below it, and for any other name the directory the lock is in, so that
/// nothing is removed.
///
/// `lock_raw_ref()` creates the lock's leading directories
/// (refs/files-backend.c:791-792), and the lock is released either by the commit
/// or, still held, by `files_transaction_cleanup()`, which follows `unlock_ref()`
/// with `try_remove_empty_parents(refs, update->refname, REMOVE_EMPTY_PARENTS_REF)`
/// (:2919-2929). That function skips the first two components of the name and
/// removes the empty directories below them, bottom-up (:1330-1371):
///
/// ```c
/// for (i = 0; i < 2; i++) { /* refs/{heads,tags,...}/ */
///         while (*p && *p != '/')
///                 p++;
///         /* tolerate duplicate slashes; see check_refname_format() */
///         while (*p == '/')
///                 p++;
/// }
/// ```
///
/// so `refs/tags/` itself is never removed, nor any directory above it — however
/// the git directory is spelled, and whether or not it lies inside a work tree.
fn kind_directory(base: &std::path::Path, relative_path: &std::path::Path) -> std::path::PathBuf {
    let mut components = relative_path.components();
    match (components.next(), components.next(), components.next()) {
        (Some(first), Some(second), Some(_)) => base.join(first).join(second),
        _ => base.join(relative_path).parent().unwrap_or(base).to_owned(),
    }
}
