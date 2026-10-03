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

use std::{
    cell::Cell,
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{Addition, LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, Writer, stack::TableFile};

use super::{Backend, Error, StackRef, WorktreeType, lock, parse_worktree_ref};
use crate::{
    FullName, Namespace, Target,
    store::WriteReflog,
    store_impl::file::transaction::{ErrorKind, commit, prepare},
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefEditsExt, RefLog},
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
}

/// A reference's value as its stack stores it.
enum Raw {
    Object(ObjectId),
    Symbolic(BString),
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
        Error::Unsupported { .. } => err.to_string(),
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

/// `is_pseudo_ref()` (refs.c:887-900): the references that stay files.
fn is_pseudo_ref(name: &[u8]) -> bool {
    name == b"FETCH_HEAD" || name == b"MERGE_HEAD"
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
fn parse_c_int(s: &str) -> i64 {
    let s = s.trim_start();
    let (sign, digits) = match s.as_bytes().first() {
        Some(b'-') => (-1, &s[1..]),
        Some(b'+') => (1, &s[1..]),
        _ => (1, s),
    };
    let end = digits.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(digits.len());
    sign * digits[..end].parse::<i64>().unwrap_or(0)
}

/// `fill_reftable_log_record()` (refs/reftable-backend.c:297-321): the
/// committer part of a log entry. `time` is `<seconds> <+|-HHMM>`.
fn log_update(committer: gix_actor::SignatureRef<'_>) -> LogUpdate {
    let committer = committer.trim();
    let mut parts = committer.time.split_ascii_whitespace();
    let time = parse_c_int(parts.next().unwrap_or("")) as u64;
    let tz = parts.next().unwrap_or("");
    let (sign, tz) = match tz.as_bytes().first() {
        Some(b'-') => (-1, &tz[1..]),
        Some(b'+') => (1, &tz[1..]),
        _ => (1, tz),
    };
    LogUpdate {
        name: committer.name.to_owned(),
        email: committer.email.to_owned(),
        time,
        tz_offset: (sign * parse_c_int(tz)) as i16,
        ..LogUpdate::default()
    }
}

/// An object id as a record's hash.
fn hash_of(id: &ObjectId) -> gix_reftable::record::Hash {
    let mut hash = gix_reftable::record::Hash::default();
    hash[..id.as_slice().len()].copy_from_slice(id.as_slice());
    hash
}

impl Backend {
    /// An object id from a record's hash.
    fn oid_of(&self, hash: &gix_reftable::record::Hash, kind: gix_hash::Kind) -> ObjectId {
        ObjectId::from_bytes_or_panic(&hash[..kind.len_in_bytes()])
    }

    /// `reftable_backend_read_ref()` (refs/reftable-backend.c:62-118): the
    /// value of `refname` in `stack`, `None` if it does not exist there.
    fn read_ref_in(&self, stack: &Stack, refname: &[u8], kind: gix_hash::Kind) -> Result<Option<Raw>, Error> {
        Ok(stack.read_ref(refname)?.map(|record| match record.value {
            RefValue::Symref(target) => Raw::Symbolic(target),
            RefValue::Val1(hash) | RefValue::Val2 { value: hash, .. } => Raw::Object(self.oid_of(&hash, kind)),
            RefValue::Deletion => unreachable!("read_ref() skips deletions"),
        }))
    }

    /// Run `f` on the stack `refname` lives in and the name within that stack,
    /// reloaded first as every reading operation does. `held` is a stack whose
    /// mutex the caller holds already: it is used as it is, which is what a
    /// reload amounts to for the stack an addition has locked.
    fn with_stack_for<T>(
        &self,
        refname: &BStr,
        held: Option<(&StackRef, &Stack)>,
        f: impl FnOnce(&Stack, &BStr) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let (stack, rewritten) = self.backend_for(refname, false)?;
        match held {
            Some((held_ref, held_stack)) if Arc::ptr_eq(held_ref, &stack) => f(held_stack, rewritten),
            _ => {
                let mut guard = lock(&stack);
                guard.reload()?;
                f(&guard, rewritten)
            }
        }
    }

    /// `refs_read_raw_ref()` (refs.c:2094-2105) with `reftable_be_read_raw_ref()`
    /// (refs/reftable-backend.c:880-908): pseudo references are read from their
    /// files, all others from their stack.
    fn read_raw(
        &self,
        refname: &BStr,
        kind: gix_hash::Kind,
        held: Option<(&StackRef, &Stack)>,
    ) -> Result<Option<Raw>, Error> {
        if is_pseudo_ref(refname) {
            // `refs_read_special_head()` (refs.c:2070-2092).
            let contents = match std::fs::read(self.git_dir().join(gix_path::from_bstr(refname))) {
                Ok(contents) => contents,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let name = FullName(refname.to_owned());
            return Ok(
                match crate::file::loose::Reference::try_from_path(name, &contents, kind).map(|r| r.target) {
                    Ok(Target::Object(id)) => Some(Raw::Object(id)),
                    Ok(Target::Symbolic(target)) => Some(Raw::Symbolic(target.0)),
                    Err(_) => None,
                },
            );
        }
        self.with_stack_for(refname, held, |stack, name| self.read_ref_in(stack, name, kind))
    }

    /// `refs_resolve_ref_unsafe()` (refs.c:2113-2200) without
    /// `RESOLVE_REF_ALLOW_BAD_NAME`: the name `refname` ends at and its object,
    /// `None` where git returns `NULL`. With `reading` the reference must
    /// exist; without, a missing one resolves to the null id.
    fn resolve(
        &self,
        refname: &BStr,
        reading: bool,
        kind: gix_hash::Kind,
        held: Option<(&StackRef, &Stack)>,
    ) -> Option<(BString, ObjectId)> {
        let mut name = refname.to_owned();
        for _ in 0..SYMREF_MAXDEPTH {
            if gix_validate::reference::name_partial(name.as_ref()).is_err() {
                return None;
            }
            match self.read_raw(name.as_ref(), kind, held).ok()? {
                None if reading => return None,
                None => return Some((name, kind.null())),
                Some(Raw::Object(id)) => return Some((name, id)),
                Some(Raw::Symbolic(target)) => name = target,
            }
        }
        None
    }

    /// `reftable_be_reflog_exists()` (refs/reftable-backend.c:2306-2366):
    /// whether `refname` has a log entry that is not a deletion.
    fn reflog_exists_in(&self, refname: &BStr, held: Option<(&StackRef, &Stack)>) -> Result<bool, Error> {
        self.check()?;
        self.with_stack_for(refname, held, |stack, name| {
            let mut it = stack.log_iterator()?;
            if !it.seek_log(name)? {
                return Ok(false);
            }
            let mut log = LogRecord::default();
            while it.next_log(&mut log)? {
                if log.refname != name {
                    return Ok(false);
                }
                if !log.is_deletion() {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    /// `should_write_log()` (refs/reftable-backend.c:1443-1461).
    fn should_write_log(
        &self,
        refname: &BStr,
        log_refs_default: WriteReflog,
        held: Option<(&StackRef, &Stack)>,
    ) -> Result<bool, Error> {
        let config = self.write_config().log_all_ref_updates.unwrap_or(log_refs_default);
        if should_autocreate_reflog(config, refname) {
            return Ok(true);
        }
        self.reflog_exists_in(refname, held)
    }

    /// The first reference whose name starts with `prefix` (which ends in a
    /// slash), as `refs_ref_iterator_begin(refs, prefix, NULL, 0,
    /// REFS_FOR_EACH_INCLUDE_BROKEN)` yields it: in a linked worktree, the
    /// worktree's stack merged with the shared references of the main one
    /// (`reftable_be_iterator_begin()`, refs/reftable-backend.c:846-878, and
    /// `ref_iterator_select()`, refs/iterator.c:97-130).
    fn first_ref_below(&self, prefix: &[u8]) -> Result<Option<BString>, Error> {
        // `reftable_ref_iterator_advance()` (refs/reftable-backend.c:625-660)
        // for one stack, skipping what `shared_only` filters out.
        fn first_in(stack: &StackRef, prefix: &[u8], shared_only: bool) -> Result<Option<BString>, Error> {
            let mut stack = lock(stack);
            stack.reload()?;
            let mut it = stack.ref_iterator()?;
            if !it.seek_ref(prefix)? {
                return Ok(None);
            }
            let mut record = RefRecord::default();
            while it.next_ref(&mut record)? {
                if !record.refname.starts_with(b"refs/") {
                    continue;
                }
                if !record.refname.starts_with(prefix) {
                    break;
                }
                if record.is_deletion() {
                    continue;
                }
                if shared_only && parse_worktree_ref(record.refname.as_ref()).0 != WorktreeType::Shared {
                    continue;
                }
                return Ok(Some(record.refname));
            }
            Ok(None)
        }

        let worktree = self.worktree_stack();
        let main = first_in(&self.main_stack()?, prefix, worktree.is_some())?;
        let Some(worktree) = worktree else { return Ok(main) };
        let private = first_in(&worktree, prefix, false)?;
        Ok(match (private, main) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// `refs_verify_refnames_available()` (refs.c:2789-2955) for a transaction
    /// that is not the initial one, skipping nothing: none of `refnames` may
    /// be a directory of an existing reference or of one in `extras`, nor have
    /// one as its directory.
    fn verify_refnames_available(
        &self,
        refnames: &[BString],
        extras: &BTreeSet<BString>,
        kind: gix_hash::Kind,
    ) -> Result<(), prepare::Error> {
        let conflict = |message: String| rejected(ErrorKind::NameConflict, message);
        let mut dirnames = HashSet::<BString>::new();
        for refname in refnames {
            for slash in refname.find_iter(b"/") {
                let dirname: BString = refname[..slash].into();
                if !dirnames.insert(dirname.clone()) {
                    continue;
                }
                // Any error reading it counts as absent, like the files backend's ENOENT.
                if let Ok(Some(_)) = self.read_raw(dirname.as_ref(), kind, None) {
                    return Err(conflict(format!("'{dirname}' exists; cannot create '{refname}'")));
                }
                if extras.contains(&dirname) {
                    return Err(conflict(format!(
                        "cannot process '{refname}' and '{dirname}' at the same time"
                    )));
                }
            }

            let mut prefix = refname.clone();
            prefix.push(b'/');
            let existing = self.first_ref_below(&prefix).map_err(|err| prepare_failure(&err))?;
            if let Some(existing) = existing {
                return Err(conflict(format!("'{existing}' exists; cannot create '{refname}'")));
            }
            // `find_descendant_ref()` (refs.c:1787-1811).
            if let Some(extra) = extras.range(prefix.clone()..).next().filter(|e| e.starts_with(&prefix)) {
                return Err(conflict(format!(
                    "cannot process '{refname}' and '{extra}' at the same time"
                )));
            }
        }
        Ok(())
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
    pub(crate) fn transaction_prepare(
        &self,
        edits: Vec<RefEdit>,
        object_hash: gix_hash::Kind,
        log_refs_default: WriteReflog,
        namespace: Option<&Namespace>,
        objects: Option<&dyn gix_object::Find>,
    ) -> Result<TransactionData, prepare::Error> {
        let kind = object_hash;
        let mut data = TransactionData {
            args: Vec::new(),
            updates: Vec::with_capacity(edits.len()),
            object_hash: kind,
            log_refs_default,
        };

        for edit in &edits {
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
                kind,
                namespace,
                objects,
            )?);
        }

        // `ref_update_reject_duplicates()` (refs.c:2574-2596) over the names of
        // all updates that are not log-only, `transaction->refnames`.
        let named: Vec<RefEdit> = data
            .updates
            .iter()
            .filter(|u| !u.log_only)
            .map(|u| RefEdit {
                change: u.edit.change.clone(),
                name: FullName(u.refname.clone()),
                deref: false,
            })
            .collect();
        named.assure_one_name_has_one_edit().map_err(|name| {
            rejected(
                ErrorKind::Generic,
                format!("multiple updates for ref '{name}' not allowed"),
            )
        })?;
        let mut refnames: BTreeSet<BString> = named.into_iter().map(|e| e.name.0).collect();

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
            .read_ref_in(&lock(&head_stack), b"HEAD", kind)
            .map_err(|err| prepare_failure(&err))?;
        let head_referent = match head {
            Some(Raw::Symbolic(referent)) => Some(referent),
            _ => None,
        };

        let mut refnames_to_check = Vec::new();
        // Updates added by splits are prepared by this same loop.
        let mut idx = 0;
        while idx < data.updates.len() {
            self.prepare_single_update(
                &mut data,
                idx,
                &mut refnames,
                &mut refnames_to_check,
                head_referent.as_ref(),
            )?;
            idx += 1;
        }

        self.verify_refnames_available(&refnames_to_check, &refnames, kind)?;
        Ok(data)
    }

    /// `ref_transaction_update()` and `ref_transaction_add_update()`
    /// (refs.c:1307-1455): git's update for `edit`.
    fn update_from_edit(
        edit: RefEdit,
        log_only: bool,
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
        if let Some(objects) = objects {
            if update.new_target.is_none() && !update.new_oid.is_null() && !update.log_only {
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
        refnames: &mut BTreeSet<BString>,
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
            edit: RefEdit {
                change,
                name: FullName(refname.clone()),
                deref: false,
            },
        };
        data.updates.push(update);
        if !log_only {
            refnames.insert(refname);
        }
        data.updates.len() - 1
    }

    /// `prepare_single_update()` (refs/reftable-backend.c:1063-1312).
    fn prepare_single_update(
        &self,
        data: &mut TransactionData,
        idx: usize,
        refnames: &mut BTreeSet<BString>,
        refnames_to_check: &mut Vec<BString>,
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
            if refnames.contains(b"HEAD".as_bstr()) {
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
            .read_ref_in(&lock(&stack), &rewritten, kind)
            .map_err(|_| generic_failure())?;
        let mut referent = BString::default();
        match &found {
            Some(Raw::Object(id)) => current_oid = *id,
            Some(Raw::Symbolic(target)) => {
                referent = target.clone();
                data.updates[idx].is_symref = true;
            }
            None => {}
        }

        let u = &data.updates[idx];
        if found.is_none() {
            if !u.expects_existing_old_ref() {
                // The reference does not exist and is not expected to: only
                // check that nothing conflicts with creating it.
                refnames_to_check.push(u.refname.clone());
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
            let resolved = self.resolve(data.updates[idx].refname.as_ref(), false, kind, None);
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
                if refnames.contains(&referent) {
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
            Some(Raw::Symbolic(target)) => Target::Symbolic(FullName(target)),
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
        Ok(updates.into_iter().map(Update::into_edit).collect())
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
            Error::Io(_) | Error::Unsupported { .. } => gix_reftable::Error::Io,
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
                        .resolve(target.as_ref(), true, new_oid.kind(), Some(held))
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
                    let message = u.msg[..u.msg.len().min(block_size as usize / 2)].to_owned();
                    logs.push(LogRecord {
                        refname: u.refname.clone(),
                        update_index: ts,
                        value: LogValue::Update(LogUpdate {
                            new_hash: hash_of(&new_oid),
                            old_hash: hash_of(current_oid),
                            message: Some(message.into()),
                            ..log_update(committer)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committer_time_parses_like_atol_and_atoi() {
        let sig = gix_actor::SignatureRef {
            name: " A U Thor ".into(),
            email: "a@example.com".into(),
            time: "1700000000 -0130",
        };
        let update = log_update(sig);
        assert_eq!(update.name, "A U Thor", "split_ident_line() trims the name");
        assert_eq!(update.time, 1_700_000_000);
        assert_eq!(update.tz_offset, -130, "the offset stays HHMM, signed");
        let sig = gix_actor::SignatureRef { time: "5 +0200", ..sig };
        assert_eq!(log_update(sig).tz_offset, 200);
    }
}
