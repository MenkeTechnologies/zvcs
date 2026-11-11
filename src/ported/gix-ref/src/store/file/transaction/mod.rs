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

/// Why a transaction of a reftable store was refused, `enum ref_transaction_error`
/// (refs.h:20-37, v2.56.0); `ref_transaction_error_msg()` (refs.c) words each for
/// `update-ref --batch-updates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `REF_TRANSACTION_ERROR_GENERIC`: not caused by the values of an update.
    Generic,
    /// `REF_TRANSACTION_ERROR_NAME_CONFLICT`: a reference with a conflicting name exists or is updated as well.
    NameConflict,
    /// `REF_TRANSACTION_ERROR_CREATE_EXISTS`: the reference to create exists.
    CreateExists,
    /// `REF_TRANSACTION_ERROR_NONEXISTENT_REF`: the reference expected to exist does not.
    NonexistentRef,
    /// `REF_TRANSACTION_ERROR_INCORRECT_OLD_VALUE`: the reference does not have the expected value.
    IncorrectOldValue,
    /// `REF_TRANSACTION_ERROR_INVALID_NEW_VALUE`: the new value cannot be written.
    InvalidNewValue,
    /// `REF_TRANSACTION_ERROR_EXPECTED_SYMREF`: the reference expected to be symbolic is not.
    ExpectedSymref,
    /// `REF_TRANSACTION_ERROR_CASE_CONFLICT`: the name differs from an existing one only in case,
    /// which only the files backend on a case-insensitive file system reports.
    CaseConflict,
}

impl ErrorKind {
    /// `ref_transaction_error_msg()` (refs.c:3542-3562, v2.56.0): how
    /// `update-ref --batch-updates` names the error in a `rejected` line.
    pub fn message(self) -> &'static str {
        match self {
            ErrorKind::NameConflict => "refname conflict",
            ErrorKind::CreateExists => "reference already exists",
            ErrorKind::NonexistentRef => "reference does not exist",
            ErrorKind::IncorrectOldValue => "incorrect old value provided",
            ErrorKind::InvalidNewValue => "invalid new value provided",
            ErrorKind::ExpectedSymref => "expected symref but found regular ref",
            ErrorKind::CaseConflict => "reference conflict due to case-insensitive filesystem",
            ErrorKind::Generic => "unknown failure",
        }
    }
}

/// An update a transaction that may fail partially refused, as
/// `ref_transaction_for_each_rejected_update()` (refs.c:3048-3069, v2.56.0)
/// hands it to its callback: the values are those of git's `struct ref_update`,
/// an oid only where the update has one (`REF_HAVE_NEW`, `REF_HAVE_OLD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The name of the refused update; one a symbolic reference was split into
    /// is named for the referent.
    pub refname: BString,
    /// `new_oid` if `REF_HAVE_NEW`; a symbolic update has the null id here.
    pub new_oid: Option<ObjectId>,
    /// `old_oid` if `REF_HAVE_OLD`.
    pub old_oid: Option<ObjectId>,
    /// `new_target`.
    pub new_target: Option<BString>,
    /// `old_target`.
    pub old_target: Option<BString>,
    /// `rejection_err`.
    pub kind: ErrorKind,
    /// `rejection_details`, git's error text.
    pub message: BString,
}

/// git's transaction flags that a [`RefEdit`] cannot express.
#[derive(Debug, Default, Clone)]
pub(crate) struct Options {
    /// `REF_TRANSACTION_ALLOW_FAILURE`.
    pub allow_failure: bool,
    /// Indices of the edits that only verify their expected value.
    pub verify_only: std::collections::BTreeSet<usize>,
    /// Indices of the edits that write no reflog entry (`REF_SKIP_CREATE_REFLOG`).
    pub skip_create_reflog: std::collections::BTreeSet<usize>,
    /// Reflog entries written as they are given.
    pub reflog_updates: Vec<ReflogUpdate>,
    /// Log an update to a symbolic value as the files backend does; see
    /// [`Transaction::log_symref_updates()`].
    pub log_symref_updates: bool,
    /// Refuse an existing reference for [`PreviousValue::MustNotExist`] whatever it holds; see
    /// [`Transaction::must_not_exist_strictly()`].
    ///
    /// [`PreviousValue::MustNotExist`]: crate::transaction::PreviousValue::MustNotExist
    pub must_not_exist_strictly: bool,
}

/// One reflog entry written as given, git's `ref_transaction_update_reflog()`
/// (refs.c:1463-1496, v2.56.0): it does not look at or change the reference,
/// and carries its own committer and its place among the transaction's
/// entries, so a reflog can be copied entry by entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogUpdate {
    /// The reference whose reflog gains the entry.
    pub name: crate::FullName,
    /// The entry's old value.
    pub old_oid: ObjectId,
    /// The entry's new value.
    pub new_oid: ObjectId,
    /// `committer_info`: `Name <email> <seconds> <+|-HHMM>`, as `fmt_ident()` writes it.
    pub committer_info: BString,
    /// The message, normalized like every reflog message.
    pub message: BString,
    /// `index`: entries are written in this order, each under the update index
    /// of the transaction's table plus this.
    pub index: u64,
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
    /// The update is refused by a transaction that may fail partially: git's `rejection_err`, set by
    /// `ref_transaction_maybe_set_rejected()` (refs.c:1276-1312). `files_transaction_finish()` passes
    /// over it (refs/files-backend.c:3352, :3400, :3434), but the `reference-transaction` hook
    /// still lists it (`transaction_hook_feed_stdin()`, refs.c:2602-2640).
    rejected: bool,
    /// The update only verifies its expected value, git's `ref_transaction_verify()` (refs.c:1537-1554,
    /// v2.56.0): it carries no new value (`REF_HAVE_NEW` unset), so `lock_ref_for_update()` locks the
    /// reference and checks its old value but writes nothing (refs/files-backend.c:2857-2860), and
    /// `files_transaction_finish()` neither commits nor logs it (:3355-3360). An update split off one
    /// inherits it, as `split_symref_update()` and `split_head_update()` copy the flags.
    verify_only: bool,
    /// The update was split off one that was refused before `lock_ref_for_update()` split it, so it
    /// is not part of git's transaction at all: `split_symref_update()` and `split_head_update()`
    /// run for an update only once it is locked (refs/files-backend.c:2707-2792). gix splits every
    /// edit up front, so such a half is dropped instead.
    dropped: bool,
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
    ///
    /// In a store with the reftable backend the transaction follows git's reftable backend instead: each
    /// stack it writes to is locked for the whole transaction, with `reftable.lockTimeout` rather than the
    /// lock modes passed to [`prepare()`](Transaction::prepare()), and committing writes one table per
    /// stack. There is no `packed-refs` then; an object database handed to [`packed_refs()`](Transaction::packed_refs())
    /// only serves to peel annotated tags, whose peeled value a table stores with the reference.
    pub fn transaction(&self) -> Transaction<'_, '_> {
        file::first_use();
        Transaction {
            store: self,
            packed_transaction: None,
            packed_buffer: None,
            updates: None,
            packed_refs: PackedRefs::default(),
            reftable: None,
            options: Options::default(),
            hook: None,
            rejections: Vec::new(),
        }
    }
}

impl<'p> Transaction<'_, 'p> {
    /// Configure the way packed refs are handled during the transaction
    pub fn packed_refs(mut self, packed_refs: PackedRefs<'p>) -> Self {
        self.packed_refs = packed_refs;
        self
    }

    /// Let single updates fail without failing the transaction, git's
    /// `REF_TRANSACTION_ALLOW_FAILURE` (`update-ref --batch-updates`): an update
    /// refused for any reason but [`ErrorKind::Generic`] is dropped and listed by
    /// [`rejections()`](Self::rejections()), while the others are committed.
    ///
    /// Both backends honour it: the files backend records a refusal of
    /// `lock_ref_for_update()` (refs/files-backend.c:3021-3033) and of the
    /// `packed-refs` name check that follows the locking (:3064-3082).
    pub fn allow_failure(mut self) -> Self {
        self.options.allow_failure = true;
        self
    }

    /// Make the edits at `indices` (positions in what [`prepare()`](Self::prepare())
    /// is given) verify their expected value only, git's `ref_transaction_verify()`
    /// (refs.c:1537-1554, v2.56.0): the update carries no new value
    /// (`REF_HAVE_NEW` is unset), so its `new` is ignored, nothing is written and
    /// no reflog entry is made for it.
    ///
    /// Both backends honour it. In the files backend the update locks its reference and
    /// checks the old value, and the log-only updates split off it still log, with the null
    /// id as their new value (`lock_ref_for_update()`, refs/files-backend.c:2687-2905;
    /// `parse_and_write_reflog()`, :3134-3185).
    pub fn verify_only(mut self, indices: impl IntoIterator<Item = usize>) -> Self {
        self.options.verify_only.extend(indices);
        self
    }

    /// Make the edits at `indices` (positions in what [`prepare()`](Self::prepare())
    /// is given) write no reflog entry, git's `REF_SKIP_CREATE_REFLOG`.
    ///
    /// Only a store with the reftable backend honours it.
    pub fn skip_create_reflog(mut self, indices: impl IntoIterator<Item = usize>) -> Self {
        self.options.skip_create_reflog.extend(indices);
        self
    }

    /// Log each update that makes a reference symbolic as `parse_and_write_reflog()` does
    /// (refs/files-backend.c:3154-3170): under the value its new target resolves to, and not at all
    /// when that target is dangling. Without it a symbolic update writes no reflog entry, which the
    /// callers that predate this write themselves.
    ///
    /// Only a store with the files backend reads it.
    pub fn log_symref_updates(mut self) -> Self {
        self.options.log_symref_updates = true;
        self
    }

    /// Make [`PreviousValue::MustNotExist`] refuse a reference that exists whatever it holds,
    /// as `check_old_oid()` refuses one for an update whose old value is the null id
    /// (refs/files-backend.c:2647-2652, v2.56.0). Without it the files backend accepts a
    /// reference that already holds the new value.
    ///
    /// The reftable backend always refuses it.
    ///
    /// [`PreviousValue::MustNotExist`]: crate::transaction::PreviousValue::MustNotExist
    pub fn must_not_exist_strictly(mut self) -> Self {
        self.options.must_not_exist_strictly = true;
        self
    }

    /// Add reflog entries to write as they are given, see [`ReflogUpdate`].
    ///
    /// Only a store with the reftable backend honours it.
    pub fn update_reflogs(mut self, entries: impl IntoIterator<Item = ReflogUpdate>) -> Self {
        self.options.reflog_updates.extend(entries);
        self
    }

    /// The lines a prepared transaction feeds the `reference-transaction` hook at `prepared`
    /// (`transaction_hook_feed_stdin()`, refs.c:2602-2640), for a writer that runs the hook
    /// itself (see [`without_transaction_hook()`](crate::file::without_transaction_hook())).
    pub fn hook_stdin(&self) -> Option<&[u8]> {
        self.hook.as_ref().map(|feed| feed.lines.as_slice())
    }

    /// The updates a prepared transaction that [may fail partially](Self::allow_failure())
    /// refused, in the order they were refused.
    pub fn rejections(&self) -> &[Rejection] {
        match self.reftable.as_ref() {
            Some(data) => data.rejections(),
            None => &self.rejections,
        }
    }
}

/// What the `reference-transaction` hook is fed for a prepared transaction.
#[derive(Debug, Default)]
pub(crate) struct HookFeed {
    /// The lines of the transaction's updates as `prepared` saw them.
    pub lines: Vec<u8>,
    /// The lines of the files backend's nested `packed-refs` transaction, when
    /// `files_transaction_prepare()` prepared one (refs/files-backend.c:3084-3103);
    /// it is committed or aborted along with this one.
    pub packed: Option<Vec<u8>>,
}

/// One line of the hook's stdin, `transaction_hook_feed_stdin()` (refs.c:2602-2640):
/// `<old> <new> <refname>`, each value the null id when the update has none
/// (no `REF_HAVE_OLD`, no `REF_HAVE_NEW`), and a symbolic value as `ref:<target>`.
pub fn hook_line(
    out: &mut Vec<u8>,
    kind: gix_hash::Kind,
    old: Option<HookValue<'_>>,
    new: Option<HookValue<'_>>,
    refname: &gix_object::bstr::BStr,
) {
    for value in [old, new] {
        match value {
            Some(HookValue::Symbolic(target)) => {
                out.extend_from_slice(b"ref:");
                out.extend_from_slice(target);
            }
            Some(HookValue::Object(oid)) => out.extend_from_slice(oid.to_hex().to_string().as_bytes()),
            None => out.extend_from_slice(kind.null().to_hex().to_string().as_bytes()),
        }
        out.push(b' ');
    }
    out.extend_from_slice(refname);
    out.push(b'\n');
}

/// A value on a [`hook_line()`]: `old_oid`/`new_oid`, or `old_target`/`new_target`.
#[derive(Debug, Clone, Copy)]
pub enum HookValue<'a> {
    /// An object id.
    Object(&'a gix_hash::oid),
    /// The name a symbolic reference points to.
    Symbolic(&'a gix_object::bstr::BStr),
}

impl<'a> From<crate::TargetRef<'a>> for HookValue<'a> {
    fn from(target: crate::TargetRef<'a>) -> Self {
        match target {
            crate::TargetRef::Object(oid) => HookValue::Object(oid),
            crate::TargetRef::Symbolic(name) => HookValue::Symbolic(name.as_bstr()),
        }
    }
}

/// The hook's stdin for `edits`, one [`edit_hook_line()`] each, for a writer
/// that queues git's updates without a [`Transaction`].
pub fn hook_stdin(kind: gix_hash::Kind, edits: &[RefEdit]) -> Vec<u8> {
    let mut lines = Vec::new();
    for edit in edits {
        edit_hook_line(&mut lines, kind, edit, false);
    }
    lines
}

/// The hook line of `edit`, or nothing for a `REF_LOG_ONLY` update, which the
/// feed skips (refs.c:2617-2618).
///
/// An expected value is git's `REF_HAVE_OLD`; [`PreviousValue::MustNotExist`]
/// is `REF_HAVE_OLD` with the null id, which reads the same as none. A deletion
/// has `REF_HAVE_NEW` with the null id, and a `verify` has no new value at all.
///
/// [`PreviousValue::MustNotExist`]: crate::transaction::PreviousValue::MustNotExist
pub fn edit_hook_line(out: &mut Vec<u8>, kind: gix_hash::Kind, edit: &RefEdit, verify_only: bool) {
    use crate::transaction::{Change, PreviousValue, RefLog};
    let (log, expected, new) = match &edit.change {
        Change::Update { log, expected, new } => (log.mode, expected, (!verify_only).then_some(new)),
        Change::Delete { log, expected, .. } => (*log, expected, None),
    };
    if log == RefLog::Only {
        return;
    }
    let old = match expected {
        PreviousValue::MustExistAndMatch(t) => Some(t.to_ref().into()),
        // "If it exists it must match" has no counterpart among git's flags: its callers
        // create a reference git creates with no old value (fetch.c:676-678).
        PreviousValue::ExistingMustMatch(_)
        | PreviousValue::Any
        | PreviousValue::MustExist
        | PreviousValue::MustNotExist => None,
    };
    hook_line(out, kind, old, new.map(|t| t.to_ref().into()), edit.name.as_bstr());
}

impl<'s> Transaction<'s, '_> {
    /// `ref_transaction_abort()` of a prepared transaction (refs.c:2730-2756): roll
    /// it back — releasing the files backend's nested `packed-refs` transaction,
    /// whose hook runs at `aborted` first — and run the hook at `aborted`.
    ///
    /// Dropping a transaction rolls it back too, but silently, as
    /// `ref_transaction_free()` does.
    pub fn abort(mut self) {
        let store: &'s file::Store = self.store;
        let feed = self.hook.take();
        drop(self);
        if let Some(feed) = feed {
            if let Some(packed) = &feed.packed {
                file::transaction_hook(store, "aborted", packed);
            }
            file::transaction_hook(store, "aborted", &feed.lines);
        }
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

///
pub mod initial;
