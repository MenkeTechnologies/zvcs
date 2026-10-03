//! Transactions: `reftable_be_transaction_prepare()`, `_abort()` and
//! `_finish()` with `write_transaction_table()` (refs/reftable-backend.c:956-1697),
//! plus the part of the generic layer in refs.c that git runs before handing a
//! transaction to its backend.
//!
//! Each stack touched by a transaction gets its own [`Addition`], holding that
//! stack's `tables.list` lock from prepare until commit or abort; dropping a
//! [`TransactionData`] is `reftable_be_transaction_abort()`.
//!
//! The [`RefEdit`]s of gix are translated into git's `struct ref_update` first,
//! and the backend then works on those exactly like git: a symbolic reference
//! that is dereferenced is split *here*, after its stack is locked, and an
//! update of the branch `HEAD` points to gains a log-only update of `HEAD`.

use std::{cell::Cell, collections::BTreeSet, sync::Arc};

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{Addition, LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, Writer, stack::TableFile};

use super::{
    Backend, Error, StackRef, lock,
    common::{Held, StringList, Unavailable, fill_log_record, hash_of, strndup},
    is_pseudo_ref,
};
use crate::{
    FullName, Namespace, Target,
    store::WriteReflog,
    store_impl::file::transaction::{ErrorKind, Options, Rejection, commit, prepare},
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
};

/// git's `SYMREF_MAXDEPTH` (refs-internal.h).
const SYMREF_MAXDEPTH: usize = 5;

/// `struct ref_update` (refs/refs-internal.h), its flags spelled out.
#[derive(Debug)]
struct Update {
    refname: BString,
    new_oid: ObjectId,
    old_oid: ObjectId,
    new_target: Option<BString>,
    old_target: Option<BString>,
    /// `peeled` with `REF_HAVE_PEELED`.
    peeled: Option<ObjectId>,
    /// The normalized reflog message, `normalize_reflog_message()` (refs.c:1046-1053).
    msg: BString,
    /// `REF_HAVE_NEW`
    have_new: bool,
    /// `REF_HAVE_OLD`
    have_old: bool,
    /// `REF_NO_DEREF`
    no_deref: bool,
    /// `REF_LOG_ONLY`
    log_only: bool,
    /// `REF_UPDATE_VIA_HEAD`
    via_head: bool,
    /// `REF_FORCE_CREATE_REFLOG`
    force_create_reflog: bool,
    /// [`PreviousValue::MustExist`], which git has no flag for: the reference
    /// must exist, whatever its value.
    must_exist: bool,
    /// [`PreviousValue::ExistingMustMatch`], which git has no flag for: the old
    /// value is only checked if the reference exists.
    old_may_be_missing: bool,
    /// `type & REF_ISSYMREF`, set when prepare reads the reference.
    is_symref: bool,
    /// `parent_update`
    parent: Option<usize>,
    /// The value prepare found, reported in the edits a commit returns.
    previous: Option<Target>,
    /// `rejection_err`: the update was refused in a transaction that may fail
    /// partially, and is neither written nor logged.
    rejected: bool,
    /// The edit as a commit reports it.
    edit: RefEdit,
}

impl Update {
    /// `ref_update_has_null_new_value()` (refs.c:3158-3161).
    fn has_null_new_value(&self) -> bool {
        self.new_target.is_none() && self.new_oid.is_null()
    }

    /// `ref_update_expects_existing_old_ref()` (refs.c:3533-3540), extended by
    /// the expectations of gix that git has no flag for.
    fn expects_existing_old_ref(&self) -> bool {
        if self.log_only || self.old_may_be_missing {
            return false;
        }
        self.must_exist || (self.have_old && (!self.old_oid.is_null() || self.old_target.is_some()))
    }

    /// The edit a commit reports for this update: its mode reflects a split
    /// that made it log-only, its expectation the value prepare found.
    fn into_edit(self) -> RefEdit {
        let Update {
            mut edit,
            previous,
            log_only,
            ..
        } = self;
        match &mut edit.change {
            Change::Update { log, expected, .. } => {
                if log_only {
                    log.mode = RefLog::Only;
                }
                if let Some(previous) = previous {
                    *expected = PreviousValue::MustExistAndMatch(previous);
                }
            }
            Change::Delete { log, expected, .. } => {
                if log_only {
                    *log = RefLog::Only;
                }
                if let Some(previous) = previous {
                    *expected = PreviousValue::MustExistAndMatch(previous);
                }
            }
        }
        edit.deref = false;
        edit
    }
}

/// The two kinds of update `prepare_single_update()` adds to a transaction.
#[derive(Debug, Clone, Copy)]
enum Split {
    /// The log-only update of `HEAD` for an update of the branch it points to.
    HeadLog,
    /// The update of the referent of a dereferenced symbolic reference;
    /// `via_head` is `REF_UPDATE_VIA_HEAD`.
    Referent { via_head: bool },
}

/// `struct write_transaction_table_arg` (refs/reftable-backend.c:940-949):
/// the updates of one stack and the addition that holds its lock.
struct StackUpdate {
    stack: StackRef,
    addition: Addition,
    /// `struct reftable_transaction_update`: an index into
    /// [`TransactionData::updates`] and the value the reference had.
    updates: Vec<(usize, ObjectId)>,
}

/// What a prepared transaction holds until it is committed or dropped,
/// `struct reftable_transaction_data` (refs/reftable-backend.c:951-954) and
/// the updates of `struct ref_transaction`. Dropping it releases every lock.
pub struct TransactionData {
    args: Vec<StackUpdate>,
    updates: Vec<Update>,
    object_hash: gix_hash::Kind,
    log_refs_default: WriteReflog,
    /// `REF_TRANSACTION_ALLOW_FAILURE`.
    allow_failure: bool,
    /// `transaction->rejections`, in the order the updates were refused.
    rejections: Vec<Rejection>,
}

impl std::fmt::Debug for TransactionData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionData")
            .field("stacks", &self.args.len())
            .field("updates", &self.updates)
            .finish_non_exhaustive()
    }
}

impl TransactionData {
    /// The edits as prepared, splits included, for a rollback to report.
    pub(crate) fn edits(&self) -> Vec<RefEdit> {
        self.updates.iter().map(|u| u.edit.clone()).collect()
    }

    /// The updates refused so far, see [`Rejection`].
    pub(crate) fn rejections(&self) -> &[Rejection] {
        &self.rejections
    }

    /// `ref_transaction_maybe_set_rejected()` (refs.c:1276-1312): record
    /// `err` as the reason update `idx` is dropped, if the transaction may fail
    /// partially and `err` is about the update's values; `false` otherwise,
    /// and the transaction fails. A rejected name stops counting as one the
    /// transaction updates.
    fn maybe_set_rejected(&mut self, idx: usize, err: &prepare::Error, refnames: &mut StringList) -> bool {
        let prepare::Error::Reftable { kind, message } = err else {
            return false;
        };
        if !self.allow_failure || *kind == ErrorKind::Generic {
            return false;
        }
        let u = &mut self.updates[idx];
        refnames.remove(&u.refname);
        u.rejected = true;
        self.rejections.push(Rejection {
            refname: u.refname.clone(),
            new_oid: u.have_new.then_some(u.new_oid),
            old_oid: u.have_old.then_some(u.old_oid),
            new_target: u.new_target.clone(),
            old_target: u.old_target.clone(),
            kind: *kind,
            message: message.clone(),
        });
        true
    }
}

/// A failed transaction, with git's message in `err` and the kind of error.
fn rejected(kind: ErrorKind, message: impl Into<BString>) -> prepare::Error {
    prepare::Error::Reftable {
        kind,
        message: message.into(),
    }
}

/// `reftable_error_str()` of an error of the backend.
fn error_str(err: &Error) -> String {
    match err {
        Error::Reftable(err) => err.to_string(),
        Error::Io(_) => gix_reftable::Error::Io.to_string(),
    }
}

/// The message `reftable_be_transaction_prepare()` reports for an error that
/// left `err` empty (refs/reftable-backend.c:1405-1408).
fn prepare_failure(err: &Error) -> prepare::Error {
    rejected(
        ErrorKind::Generic,
        format!("reftable: transaction prepare: {}", error_str(err)),
    )
}

/// `prepare_single_update()` returning `REF_TRANSACTION_ERROR_GENERIC`
/// without a message, which the caller reports as the reftable error of that
/// value, -1.
fn generic_failure() -> prepare::Error {
    rejected(
        ErrorKind::Generic,
        format!("reftable: transaction prepare: {}", gix_reftable::Error::General),
    )
}

/// `should_autocreate_reflog()` (refs.c:1064-1078).
fn should_autocreate_reflog(config: WriteReflog, refname: &[u8]) -> bool {
    match config {
        WriteReflog::Always => true,
        WriteReflog::Normal => {
            refname.starts_with(b"refs/heads/")
                || refname.starts_with(b"refs/remotes/")
                || refname.starts_with(b"refs/notes/")
                || refname == b"HEAD"
        }
        WriteReflog::Disable => false,
    }
}

/// `peel_object(…, PEEL_OBJECT_VERIFY_TAGGED_OBJECT_TYPE)` (object.c:211-250)
/// as `ref_transaction_update()` uses it (refs.c:1433-1455): the object a tag
/// finally points to, `None` if `id` is no tag or peeling fails.
fn peel_tag(objects: &dyn gix_object::Find, id: &ObjectId) -> Option<ObjectId> {
    let mut buf = Vec::new();
    let mut next = *id;
    let mut declared: Option<gix_object::Kind> = None;
    loop {
        let data = objects.try_find(&next, &mut buf).ok()??;
        if declared.is_some_and(|kind| kind != data.kind) {
            return None;
        }
        if data.kind != gix_object::Kind::Tag {
            return (next != *id).then_some(next);
        }
        use gix_object::tag::ref_iter::Token;
        let mut tokens = gix_object::TagRefIter::from_bytes(data.data, data.object_hash);
        match (tokens.next(), tokens.next()) {
            (Some(Ok(Token::Target { id })), Some(Ok(Token::TargetKind(kind)))) => {
                next = id;
                declared = Some(kind);
            }
            _ => return None,
        }
    }
}

/// `atol()`/`atoi()`: an optionally signed decimal prefix, 0 without digits.
impl Backend {
    /// `refs_resolve_ref_unsafe()` (refs.c:2113-2200) without
    /// `RESOLVE_REF_ALLOW_BAD_NAME`: the name `refname` ends at and its object,
    /// `None` where git returns `NULL`. With `reading` the reference must
    /// exist; without, a missing one resolves to the null id.
    fn resolve(&self, refname: &BStr, reading: bool, held: Held<'_>) -> Option<(BString, ObjectId)> {
        let kind = self.object_hash();
        let mut name = refname.to_owned();
        for _ in 0..SYMREF_MAXDEPTH {
            if gix_validate::reference::name_partial(name.as_ref()).is_err() {
                return None;
            }
            match self.read_raw_ref_in(name.as_ref(), held).ok()? {
                None if reading => return None,
                None => return Some((name, kind.null())),
                Some(Target::Object(id)) => return Some((name, id)),
                Some(Target::Symbolic(target)) => name = target.0,
            }
        }
        None
    }

    /// `should_write_log()` (refs/reftable-backend.c:1443-1461).
    fn should_write_log(&self, refname: &BStr, log_refs_default: WriteReflog, held: Held<'_>) -> Result<bool, Error> {
        let config = self.write_config().log_all_ref_updates.unwrap_or(log_refs_default);
        if should_autocreate_reflog(config, refname) {
            return Ok(true);
        }
        self.reflog_exists_in(refname, held)
    }

    /// `prepare_transaction_update()` (refs/reftable-backend.c:968-1029): the
    /// index of the stack update `refname` belongs to, locking its stack when
    /// the transaction touches it first.
    fn stack_update_for(&self, data: &mut TransactionData, refname: &BStr) -> Result<usize, prepare::Error> {
        // Not reloaded: taking the lock reloads an outdated stack.
        let (stack, _) = self.backend_for(refname, false).map_err(|err| prepare_failure(&err))?;
        if let Some(pos) = data.args.iter().position(|arg| Arc::ptr_eq(&arg.stack, &stack)) {
            return Ok(pos);
        }
        let addition = lock(&stack)
            .addition_new(Some(&self.write_config().opts))
            .map_err(|err| match err {
                gix_reftable::Error::Lock => rejected(ErrorKind::Generic, "cannot lock references"),
                other => prepare_failure(&other.into()),
            })?;
        data.args.push(StackUpdate {
            stack,
            addition,
            updates: Vec::new(),
        });
        Ok(data.args.len() - 1)
    }

    /// `queue_transaction_update()` (refs/reftable-backend.c:1031-1061).
    fn queue_update(
        &self,
        data: &mut TransactionData,
        idx: usize,
        current_oid: ObjectId,
    ) -> Result<(), prepare::Error> {
        let refname = data.updates[idx].refname.clone();
        let arg = self.stack_update_for(data, refname.as_ref())?;
        data.args[arg].updates.push((idx, current_oid));
        Ok(())
    }

    /// `ref_update_original_update_refname()` (refs.c:3150-3156).
    fn original_refname(updates: &[Update], mut idx: usize) -> &BStr {
        while let Some(parent) = updates[idx].parent {
            idx = parent;
        }
        updates[idx].refname.as_ref()
    }

    /// `reftable_be_transaction_prepare()` (refs/reftable-backend.c:1314-1416),
    /// preceded by what `ref_transaction_update()` and `ref_transaction_prepare()`
    /// check before a backend sees the transaction (refs.c:1368-1455, 2681-2730).
    ///
    /// - `log_refs_default` is what `core.logAllRefUpdates` means while it is
    ///   unset: gix derives the store's [`WriteReflog`] from that same setting
    ///   with git's default, `Disable` in a bare repository and `Normal`
    ///   otherwise (`should_write_log()`, refs/reftable-backend.c:1443-1461).
    /// - `namespace` is prepended to every name.
    /// - `objects` peels annotated tags, whose peeled value git stores with the
    ///   reference (`REF_HAVE_PEELED`, refs.c:1433-1455); without it a reference
    ///   to a tag is written without one.
    /// - `options` carries `REF_TRANSACTION_ALLOW_FAILURE` and the edits that
    ///   only verify (`ref_transaction_verify()`).
    pub(crate) fn transaction_prepare(
        &self,
        edits: Vec<RefEdit>,
        object_hash: gix_hash::Kind,
        log_refs_default: WriteReflog,
        namespace: Option<&Namespace>,
        objects: Option<&dyn gix_object::Find>,
        options: &Options,
    ) -> Result<TransactionData, prepare::Error> {
        let kind = object_hash;
        let mut data = TransactionData {
            args: Vec::new(),
            updates: Vec::with_capacity(edits.len()),
            object_hash: kind,
            log_refs_default,
            allow_failure: options.allow_failure,
            rejections: Vec::new(),
        };

        for (idx, edit) in edits.iter().enumerate() {
            let log_only = match &edit.change {
                Change::Update { log, .. } => log.mode == RefLog::Only,
                Change::Delete { log, .. } => *log == RefLog::Only,
            };
            // `transaction_refname_valid()` (refs.c:1368-1394); everything else
            // it checks is guaranteed by `FullName`.
            if is_pseudo_ref(edit.name.as_bstr()) {
                let message = if log_only {
                    format!("refusing to update reflog for pseudoref '{}'", edit.name.as_bstr())
                } else {
                    format!("refusing to update pseudoref '{}'", edit.name.as_bstr())
                };
                return Err(rejected(ErrorKind::Generic, message));
            }
            data.updates.push(Self::update_from_edit(
                edit.clone(),
                log_only,
                options.verify_only.contains(&idx),
                kind,
                namespace,
                objects,
            )?);
        }

        // `ref_update_reject_duplicates()` (refs.c:2574-2596) over the names of
        // all updates that are not log-only, `transaction->refnames`.
        let mut refnames = StringList::default();
        for u in data.updates.iter().filter(|u| !u.log_only) {
            refnames.append(u.refname.clone());
        }
        refnames.sort();
        if let Some(pair) = refnames.items().windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(rejected(
                ErrorKind::Generic,
                format!("multiple updates for ref '{}' not allowed", pair[1]),
            ));
        }

        self.check().map_err(|err| prepare_failure(&err))?;

        // Lock every stack the updates go to.
        for idx in 0..data.updates.len() {
            let refname = data.updates[idx].refname.clone();
            self.stack_update_for(&mut data, refname.as_ref())?;
        }

        // `HEAD` of the current worktree, read without reloading its stack,
        // which a transaction not touching it has not locked
        // (refs/reftable-backend.c:1351-1366).
        let (head_stack, _) = self
            .backend_for(b"HEAD".as_bstr(), false)
            .map_err(|err| prepare_failure(&err))?;
        let head = self
            .read_ref(&lock(&head_stack), b"HEAD")
            .map_err(|err| prepare_failure(&err))?;
        let head_referent = match head {
            Some(Target::Symbolic(referent)) => Some(referent.0),
            _ => None,
        };

        let mut refnames_to_check = Vec::new();
        // Updates added by splits are prepared by this same loop.
        let mut idx = 0;
        while idx < data.updates.len() {
            if let Err(err) = self.prepare_single_update(
                &mut data,
                idx,
                &mut refnames,
                &mut refnames_to_check,
                head_referent.as_ref(),
            ) {
                if !data.maybe_set_rejected(idx, &err, &mut refnames) {
                    return Err(err);
                }
            }
            idx += 1;
        }

        let to_check: Vec<BString> = refnames_to_check.iter().map(|(name, _)| name.clone()).collect();
        let mut conflicts = Vec::new();
        let mut reject = |at: usize, message: &str| {
            let taken = data.allow_failure;
            if taken {
                conflicts.push((refnames_to_check[at].1, message.to_owned()));
            }
            taken
        };
        let available = self.verify_refnames_available(
            &to_check,
            Some(&mut refnames),
            &BTreeSet::new(),
            None,
            Some(&mut reject),
        );
        for (idx, message) in conflicts {
            let err = rejected(ErrorKind::NameConflict, message);
            data.maybe_set_rejected(idx, &err, &mut refnames);
        }
        available.map_err(|err| match err {
                Unavailable::Conflict(message) => rejected(ErrorKind::NameConflict, message),
                Unavailable::Backend(err) => prepare_failure(&err),
            })?;
        Ok(data)
    }

    /// `ref_transaction_update()` and `ref_transaction_add_update()`
    /// (refs.c:1307-1455): git's update for `edit`.
    fn update_from_edit(
        edit: RefEdit,
        log_only: bool,
        verify_only: bool,
        kind: gix_hash::Kind,
        namespace: Option<&Namespace>,
        objects: Option<&dyn gix_object::Find>,
    ) -> Result<Update, prepare::Error> {
        let refname = match namespace {
            Some(namespace) => namespace.clone().into_namespaced_name(edit.name.as_ref()).0,
            None => edit.name.0.clone(),
        };
        let mut update = Update {
            refname,
            new_oid: kind.null(),
            old_oid: kind.null(),
            new_target: None,
            old_target: None,
            peeled: None,
            msg: BString::default(),
            have_new: true,
            have_old: false,
            no_deref: !edit.deref,
            log_only,
            via_head: false,
            force_create_reflog: false,
            must_exist: false,
            old_may_be_missing: false,
            is_symref: false,
            parent: None,
            previous: None,
            rejected: false,
            edit: RefEdit {
                deref: false,
                ..edit.clone()
            },
        };
        let expected = match &edit.change {
            Change::Update { log, expected, new } => {
                match new {
                    Target::Object(id) => update.new_oid = *id,
                    Target::Symbolic(name) => update.new_target = Some(name.0.clone()),
                }
                update.force_create_reflog = log.force_create_reflog;
                update.msg = crate::log::normalize_message(log.message.as_ref());
                expected
            }
            Change::Delete { expected, message, .. } => {
                if matches!(expected, PreviousValue::MustNotExist) {
                    // `ref_transaction_delete()` (refs.c:1488-1489) treats it as a bug.
                    return Err(rejected(ErrorKind::Generic, "delete called with old_oid set to zeros"));
                }
                update.msg = crate::log::normalize_message(message.as_ref());
                expected
            }
        };
        match expected {
            PreviousValue::Any => {}
            PreviousValue::MustExist => update.must_exist = true,
            PreviousValue::MustNotExist => update.have_old = true,
            PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous) => {
                update.have_old = true;
                update.old_may_be_missing = matches!(expected, PreviousValue::ExistingMustMatch(_));
                match previous {
                    Target::Object(id) => update.old_oid = *id,
                    Target::Symbolic(name) => update.old_target = Some(name.0.clone()),
                }
            }
        }
        if verify_only {
            // `ref_transaction_verify()` passes no new value at all.
            update.have_new = false;
            update.new_oid = kind.null();
            update.new_target = None;
            update.force_create_reflog = false;
        }
        if let Some(objects) = objects {
            if update.have_new && update.new_target.is_none() && !update.new_oid.is_null() && !update.log_only {
                update.peeled = peel_tag(objects, &update.new_oid);
            }
        }
        Ok(update)
    }

    /// `ref_transaction_add_update()` (refs.c:1307-1366) for an update that
    /// `prepare_single_update()` derives from the update `from`, named
    /// `refname`, of the given `kind`; its index.
    fn add_split_update(
        data: &mut TransactionData,
        refnames: &mut StringList,
        from: usize,
        refname: BString,
        kind: Split,
    ) -> usize {
        let p = &data.updates[from];
        let (new_target, old_target, log_only, no_deref, via_head, parent) = match kind {
            // `u->flags | REF_LOG_ONLY | REF_NO_DEREF` with the values but not
            // the targets, and no parent (refs/reftable-backend.c:1110-1114).
            Split::HeadLog => (None, None, true, true, p.via_head, None),
            // `new_flags` with the targets, `u` as parent (:1206-1212).
            Split::Referent { via_head } => (
                p.new_target.clone(),
                p.old_target.clone(),
                p.log_only,
                p.no_deref,
                via_head,
                Some(from),
            ),
        };
        let mode = if log_only { RefLog::Only } else { RefLog::AndReference };
        let change = match &new_target {
            None if p.new_oid.is_null() => Change::Delete {
                expected: PreviousValue::Any,
                log: mode,
                message: p.msg.clone(),
            },
            new_target => Change::Update {
                log: LogChange {
                    mode,
                    force_create_reflog: p.force_create_reflog,
                    message: p.msg.clone(),
                },
                expected: PreviousValue::Any,
                new: match new_target {
                    Some(target) => Target::Symbolic(FullName(target.clone())),
                    None => Target::Object(p.new_oid),
                },
            },
        };
        let update = Update {
            refname: refname.clone(),
            new_oid: p.new_oid,
            old_oid: p.old_oid,
            new_target,
            old_target,
            peeled: p.peeled,
            msg: p.msg.clone(),
            have_new: p.have_new,
            have_old: p.have_old,
            no_deref,
            log_only,
            via_head,
            force_create_reflog: p.force_create_reflog,
            must_exist: p.must_exist,
            old_may_be_missing: p.old_may_be_missing,
            is_symref: false,
            parent,
            previous: None,
            rejected: false,
            edit: RefEdit {
                change,
                name: FullName(refname.clone()),
                deref: false,
            },
        };
        data.updates.push(update);
        if !log_only {
            refnames.append(refname);
        }
        data.updates.len() - 1
    }

    /// `prepare_single_update()` (refs/reftable-backend.c:1063-1312).
    fn prepare_single_update(
        &self,
        data: &mut TransactionData,
        idx: usize,
        refnames: &mut StringList,
        refnames_to_check: &mut Vec<(BString, usize)>,
        head_referent: Option<&BString>,
    ) -> Result<(), prepare::Error> {
        let kind = data.object_hash;
        let mut current_oid = kind.null();

        // No reload: the stack is locked, and was reloaded when it was.
        let (stack, rewritten) = {
            let (stack, rewritten) = self
                .backend_for(data.updates[idx].refname.as_ref(), false)
                .map_err(|_| generic_failure())?;
            (stack, rewritten.to_owned())
        };

        // When we update the reference that HEAD points to we enqueue a
        // second log-only update for HEAD so that its reflog is updated
        // accordingly.
        let u = &data.updates[idx];
        if head_referent.is_some_and(|referent| *referent == rewritten) && !u.log_only && !u.via_head {
            if refnames.has(b"HEAD") {
                return Err(rejected(
                    ErrorKind::NameConflict,
                    format!(
                        "multiple updates for 'HEAD' (including one via its referent '{}') are not allowed",
                        u.refname
                    ),
                ));
            }
            Self::add_split_update(data, refnames, idx, "HEAD".into(), Split::HeadLog);
        }

        let found = self
            .read_ref(&lock(&stack), &rewritten)
            .map_err(|_| generic_failure())?;
        let mut referent = BString::default();
        match &found {
            Some(Target::Object(id)) => current_oid = *id,
            Some(Target::Symbolic(target)) => {
                referent = target.0.clone();
                data.updates[idx].is_symref = true;
            }
            None => {}
        }

        let u = &data.updates[idx];
        if found.is_none() {
            if !u.expects_existing_old_ref() {
                // The reference does not exist and is not expected to: only
                // check that nothing conflicts with creating it.
                refnames_to_check.push((u.refname.clone(), idx));
                // There is no need to write the reference deletion when the
                // reference in question doesn't exist.
                if u.have_new && !u.has_null_new_value() {
                    self.queue_update(data, idx, current_oid)?;
                }
                return Ok(());
            }
            return Err(rejected(
                ErrorKind::NonexistentRef,
                format!(
                    "cannot lock ref '{}': unable to resolve reference '{}'",
                    Self::original_refname(&data.updates, idx),
                    u.refname
                ),
            ));
        }

        if data.updates[idx].is_symref {
            // The stack is locked, so resolving cannot race.
            let resolved = self.resolve(data.updates[idx].refname.as_ref(), false, None);
            if let Some((_, id)) = &resolved {
                current_oid = *id;
            }
            let u = &data.updates[idx];
            if u.no_deref {
                if u.have_old && resolved.is_none() {
                    return Err(rejected(
                        ErrorKind::Generic,
                        format!("cannot lock ref '{}': error reading reference", u.refname),
                    ));
                }
            } else {
                if refnames.has(&referent) {
                    return Err(rejected(
                        ErrorKind::NameConflict,
                        format!(
                            "multiple updates for '{referent}' (including one via symref '{}') are not allowed",
                            u.refname
                        ),
                    ));
                }
                // If we are updating a symref (eg. HEAD), we should also update
                // the branch that the symref points to.
                let via_head = u.via_head || rewritten == "HEAD";
                Self::add_split_update(data, refnames, idx, referent.clone(), Split::Referent { via_head });
                // Change the symbolic ref update to log only.
                let u = &mut data.updates[idx];
                u.log_only = true;
                u.no_deref = true;
            }
        }

        let u = &data.updates[idx];
        let original = || Self::original_refname(&data.updates, idx);
        if let Some(old_target) = &u.old_target {
            if !u.is_symref {
                return Err(rejected(
                    ErrorKind::ExpectedSymref,
                    format!(
                        "cannot lock ref '{}': expected symref with target '{old_target}': but is a regular ref",
                        original()
                    ),
                ));
            }
            // `ref_update_check_old_target()` (refs.c:3163-3184).
            if referent != *old_target {
                return Err(if referent.is_empty() {
                    rejected(
                        ErrorKind::NonexistentRef,
                        format!(
                            "verifying symref target: '{}': reference is missing but expected {old_target}",
                            original()
                        ),
                    )
                } else {
                    rejected(
                        ErrorKind::IncorrectOldValue,
                        format!(
                            "verifying symref target: '{}': is at {referent} but expected {old_target}",
                            original()
                        ),
                    )
                });
            }
        } else if u.have_old && !u.log_only {
            if current_oid == u.old_oid {
                // A dangling symref resolves to the null id, which a creation
                // under no-deref must not clobber.
                if u.no_deref && !referent.is_empty() && u.old_oid.is_null() {
                    return Err(rejected(
                        ErrorKind::CreateExists,
                        format!("cannot lock ref '{}': dangling symref already exists", original()),
                    ));
                }
            } else if u.old_oid.is_null() {
                return Err(rejected(
                    ErrorKind::CreateExists,
                    format!("cannot lock ref '{}': reference already exists", original()),
                ));
            } else if current_oid.is_null() {
                return Err(rejected(
                    ErrorKind::NonexistentRef,
                    format!(
                        "cannot lock ref '{}': reference is missing but expected {}",
                        original(),
                        u.old_oid
                    ),
                ));
            } else {
                return Err(rejected(
                    ErrorKind::IncorrectOldValue,
                    format!(
                        "cannot lock ref '{}': is at {current_oid} but expected {}",
                        original(),
                        u.old_oid
                    ),
                ));
            }
        }

        let previous = match found {
            Some(Target::Symbolic(target)) => Target::Symbolic(target),
            _ => Target::Object(current_oid),
        };
        let u = &mut data.updates[idx];
        u.previous = Some(previous);

        // Skip no-op updates: they would only add log entries.
        if u.is_symref || u.log_only || (u.have_new && current_oid != u.new_oid) {
            self.queue_update(data, idx, current_oid)?;
        }
        Ok(())
    }

    /// `reftable_be_transaction_finish()` (refs/reftable-backend.c:1666-1697):
    /// write one table per stack and commit it, auto-compacting as the
    /// addition does. On failure the remaining stacks are left untouched.
    pub(crate) fn transaction_finish(
        &self,
        data: TransactionData,
        committer: Option<gix_actor::SignatureRef<'_>>,
    ) -> Result<Vec<RefEdit>, commit::Error> {
        let TransactionData {
            args,
            updates,
            object_hash: _,
            log_refs_default,
            allow_failure: _,
            rejections: _,
        } = data;
        let missing_committer = Cell::new(false);
        let failure = |err: gix_reftable::Error| {
            if missing_committer.get() {
                commit::Error::CreateOrUpdateRefLog(crate::file::log::create_or_update::Error::MissingCommitter)
            } else {
                commit::Error::Reftable {
                    message: format!("reftable: transaction failure: {err}").into(),
                }
            }
        };
        for StackUpdate {
            stack,
            mut addition,
            updates: mut stack_updates,
        } in args
        {
            let mut st = lock(&stack);
            addition
                .add(&st, |writer, st| {
                    self.write_transaction_table(
                        writer,
                        (&stack, st),
                        &mut stack_updates,
                        &updates,
                        committer,
                        log_refs_default,
                        &missing_committer,
                    )
                })
                .map_err(failure)?;
            addition.commit(&mut st).map_err(failure)?;
        }
        Ok(updates.into_iter().filter(|u| !u.rejected).map(Update::into_edit).collect())
    }

    /// `write_transaction_table()` (refs/reftable-backend.c:1463-1664): the
    /// records of one stack's updates. `held` is that stack, locked.
    #[allow(clippy::too_many_arguments)]
    fn write_transaction_table(
        &self,
        writer: &mut Writer<TableFile>,
        held: (&StackRef, &Stack),
        stack_updates: &mut [(usize, ObjectId)],
        updates: &[Update],
        committer: Option<gix_actor::SignatureRef<'_>>,
        log_refs_default: WriteReflog,
        missing_committer: &Cell<bool>,
    ) -> gix_reftable::Result<()> {
        let to_reftable = |err: Error| match err {
            Error::Reftable(err) => err,
            Error::Io(_) => gix_reftable::Error::Io,
        };
        let st = held.1;
        let ts = st.next_update_index();
        let block_size = self.write_config().opts.block_size;

        // `transaction_update_cmp()`: no update has an index, so by name.
        stack_updates.sort_by(|a, b| updates[a.0].refname.cmp(&updates[b.0].refname));
        writer.set_limits(ts, ts)?;

        let mut logs = Vec::new();
        for (idx, current_oid) in stack_updates.iter() {
            let u = &updates[*idx];
            if u.rejected {
                continue;
            }
            if u.have_new && !u.is_symref && u.has_null_new_value() {
                // When deleting refs we also delete all reflog entries with
                // them, one tombstone per entry.
                let mut it = st.log_iterator()?;
                if it.seek_log(&u.refname)? {
                    let mut log = LogRecord::default();
                    while it.next_log(&mut log)? && log.refname == u.refname {
                        if log.is_deletion() {
                            continue;
                        }
                        logs.push(LogRecord {
                            refname: u.refname.clone(),
                            update_index: log.update_index,
                            value: LogValue::Deletion,
                        });
                    }
                }
            } else if u.have_new
                && (u.force_create_reflog
                    || self
                        .should_write_log(u.refname.as_ref(), log_refs_default, Some(held))
                        .map_err(to_reftable)?)
            {
                let mut new_oid = u.new_oid;
                // Dangling symref updates get no log entry.
                let resolved = match &u.new_target {
                    Some(target) => self
                        .resolve(target.as_ref(), true, Some(held))
                        .map(|(_, id)| new_oid = id)
                        .is_some(),
                    None => true,
                };
                if resolved {
                    let Some(committer) = committer else {
                        missing_committer.set(true);
                        return Err(gix_reftable::Error::Api);
                    };
                    // `xstrndup(u->msg, block_size / 2)`.
                    let message = strndup(&u.msg, block_size as usize / 2);
                    logs.push(LogRecord {
                        refname: u.refname.clone(),
                        update_index: ts,
                        value: LogValue::Update(LogUpdate {
                            new_hash: hash_of(&new_oid),
                            old_hash: hash_of(current_oid),
                            message: Some(message),
                            ..fill_log_record(&committer)
                        }),
                    });
                }
            }

            if u.log_only {
                continue;
            }
            let value = if let Some(target) = &u.new_target {
                RefValue::Symref(target.clone())
            } else if u.have_new && u.has_null_new_value() {
                RefValue::Deletion
            } else if u.have_new {
                match u.peeled {
                    Some(peeled) => RefValue::Val2 {
                        value: hash_of(&u.new_oid),
                        target_value: hash_of(&peeled),
                    },
                    None => RefValue::Val1(hash_of(&u.new_oid)),
                }
            } else {
                continue;
            };
            writer.add_ref(&RefRecord {
                refname: u.refname.clone(),
                update_index: ts,
                value,
            })?;
        }

        // Logs are written at the end so that we do not have intermixed ref
        // and log blocks.
        if !logs.is_empty() {
            writer.add_logs(&mut logs)?;
        }
        Ok(())
    }
}

