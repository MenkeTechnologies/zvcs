//! Maintenance of reflogs and stacks: reflog creation, deletion and expiry,
//! `optimize`, rename/copy and fsck (refs/reftable-backend.c:1699-2055,
//! 2368-2860), plus laying a reftable store down on disk and removing it
//! (refs/reftable-backend.c:497-534 with refs.c:2199-2290).
//!
//! These are public as the repository level calls them directly; the file
//! store has no equivalent operations to dispatch from.
//!
//! # Errors git prints itself
//!
//! Where git reports a problem with `error(…)` inside the backend and then
//! fails, the operation fails with an [`Error::Io`] of kind
//! [`Other`](std::io::ErrorKind::Other) whose message is exactly the text git
//! passes to `error()`; callers print it as `error: <message>`. An
//! [`Error::Reftable`] is a library error git returns without printing anything.
//!
//! The reflog existence marker and the placeholder written when expiry
//! leaves a reflog empty are records without a message, C's `NULL`, which
//! the writer stores as the empty string while any other message gets a
//! trailing newline (`reftable_writer_add_log()`, reftable/writer.c:441-497).

use std::{collections::BTreeSet, fmt::Write as _, path::Path};

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{
    LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, Writer, record::Hash, stack::TableFile,
};

use super::{
    Backend, Error, StackRef, lock,
    common::{Unavailable, fill_log_record, hash_of, strndup},
    is_root_ref, parse_worktree_ref,
};
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
    /// current value before any entry is looked at. The value is the null
    /// object ID for a symbolic or missing reference.
    fn prepare(&mut self, refname: &FullNameRef, oid: &gix_hash::oid);
    /// `reflog_expiry_should_prune_fn`: whether `entry` is to be expired.
    /// Entries come oldest first; the message has no trailing newline.
    fn should_prune(&mut self, entry: &crate::log::Line) -> bool;
    /// `reflog_expiry_cleanup_fn`: called once after the last entry.
    fn cleanup(&mut self);
    /// `peel_object()` as `write_reflog_expiry_table()`
    /// (refs/reftable-backend.c:2534-2543) applies it to the value
    /// [`ExpireFlags::update_ref`] writes: the object `oid` peels to if it is
    /// an annotated tag, which is then stored with the reference, and `None`
    /// for any other object.
    ///
    /// The backend has no object database; the default knows no objects and
    /// is right only for references that do not point at annotated tags.
    fn peel(&mut self, oid: &gix_hash::oid) -> Option<ObjectId> {
        let _ = oid;
        None
    }
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

/// A problem git reports with `error()` before failing, see the module docs.
fn message(msg: String) -> Error {
    Error::Io(std::io::Error::other(msg))
}

/// `strerror(errno)` of `err`: its text without Rust's ` (os error N)`.
fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(pos) if err.raw_os_error().is_some() => text[..pos].to_owned(),
        _ => text,
    }
}

/// The library's `Error` a `write_table` callback returns when it failed with
/// one of the backend's own errors, which is kept aside in the meantime.
fn callback_failed(slot: &mut Option<Error>, err: Error) -> gix_reftable::Error {
    let code = match &err {
        Error::Reftable(e) => *e,
        _ => gix_reftable::Error::General,
    };
    *slot = Some(err);
    code
}

/// The error of an addition whose callback may have failed with a backend error.
fn addition_error(slot: Option<Error>, err: gix_reftable::Error) -> Error {
    slot.unwrap_or(Error::Reftable(err))
}

/// The first `hash_len` bytes of `h` as object ID.
fn oid_of(h: &Hash, hash_len: usize) -> ObjectId {
    ObjectId::from_bytes_or_panic(&h[..hash_len])
}

/// `is_null_oid()` on the first `hash_len` bytes of `h`.
fn is_null(h: &Hash, hash_len: usize) -> bool {
    h[..hash_len].iter().all(|&b| b == 0)
}

/// The reflog entry `should_prune()` sees for the update `u`.
fn log_line(u: &LogUpdate, hash_len: usize) -> crate::log::Line {
    let tz = i32::from(u.tz_offset);
    let offset = tz.signum() * ((tz.abs() / 100) * 3600 + (tz.abs() % 100) * 60);
    let message = u.message_or_empty();
    let message = message.strip_suffix(b"\n").unwrap_or(message);
    crate::log::Line {
        previous_oid: oid_of(&u.old_hash, hash_len),
        new_oid: oid_of(&u.new_hash, hash_len),
        signature: gix_actor::Signature {
            name: u.name.clone(),
            email: u.email.clone(),
            time: gix_actor::date::Time {
                seconds: u.time as i64,
                offset,
            },
        },
        message: message.into(),
    }
}

/// `refname_disposition` (refs.c:65-74): what each byte means in a refname.
const REFNAME_DISPOSITION: [u8; 128] = [
    1, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, //
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, //
    4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 2, 1, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 4, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 4, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 4, 4, //
];

/// `check_refname_component()` (refs.c:191-268) without sanitizing or refspec
/// patterns: the length of the component at the start of `refname`, `0` if it
/// is empty, `None` if it is invalid.
fn check_refname_component(refname: &[u8]) -> Option<usize> {
    let mut last = 0u8;
    let mut end = 0;
    loop {
        // C walks a NUL-terminated string.
        let ch = refname.get(end).copied().unwrap_or(0);
        match REFNAME_DISPOSITION.get(usize::from(ch)).copied().unwrap_or(0) {
            1 => break,
            2 if last == b'.' => return None,
            3 if last == b'@' => return None,
            4 | 5 => return None,
            _ => {}
        }
        last = ch;
        end += 1;
    }
    if end == 0 {
        return Some(0);
    }
    if refname[0] == b'.' || refname[..end].ends_with(b".lock") {
        return None;
    }
    Some(end)
}

/// `check_refname_format(refname, 0)` (refs.c:275-322): whether `refname` is
/// a well-formed name of at least two components.
fn check_refname_format(refname: &[u8]) -> bool {
    if refname == b"@" {
        return false;
    }
    let mut rest = refname;
    let mut components = 0;
    loop {
        let len = match check_refname_component(rest) {
            Some(len) if len > 0 => len,
            _ => return false,
        };
        components += 1;
        if rest.get(len).is_none_or(|&c| c == 0) {
            if rest[len - 1] == b'.' {
                return false;
            }
            break;
        }
        rest = &rest[len + 1..];
    }
    components >= 2
}

impl Backend {
    /// `reftable_be_create_on_disk()` (refs/reftable-backend.c:497-511) with
    /// what `ref_store_create_on_disk()` (refs.c:2223-2241) adds for a
    /// repository, `refs_create_refdir_stubs()` (refs.c:2199-2220): lay down
    /// an empty reftable store in `git_dir`, a `HEAD` naming the invalid
    /// branch `.invalid` so older gits recognize the repository, and a
    /// `refs/heads` *file* naming the format.
    ///
    /// Existing directories are fine and existing files are overwritten, as
    /// with git. `core.sharedRepository` permissions (`adjust_shared_perm()`)
    /// are for the caller to apply, the backend has no configuration here.
    /// git dies on any failure; the error carries `perror()`'s text,
    /// `<path>: <strerror>`, for `safe_create_dir()`.
    pub fn create_on_disk(git_dir: &Path) -> Result<(), Error> {
        // `safe_create_dir()` (path.c:791-801).
        fn safe_create_dir(dir: &Path) -> Result<(), Error> {
            match std::fs::create_dir(dir) {
                Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => {
                    Err(message(format!("{}: {}", dir.display(), strerror(&err))))
                }
                _ => Ok(()),
            }
        }
        safe_create_dir(&git_dir.join("reftable"))?;

        // `write_file()` completes the line.
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/.invalid\n")?;
        safe_create_dir(&git_dir.join("refs"))?;
        std::fs::write(git_dir.join("refs/heads"), "this repository uses the reftable format\n")?;
        Ok(())
    }

    /// `reftable_be_remove_on_disk()` (refs/reftable-backend.c:513-534) with
    /// what `ref_store_remove_on_disk()` (refs.c:2246-2287) adds: delete the
    /// reftable store of `git_dir`, then the stubs. Each failure is added to
    /// the returned message, git's `err`, and the rest still attempted; the
    /// stubs are only removed once the stack is gone.
    ///
    /// Any [`Backend`] open on `git_dir` must be dropped first, which is git's
    /// `reftable_be_release()`.
    pub fn remove_on_disk(git_dir: &Path) -> Result<(), String> {
        match std::fs::remove_dir_all(git_dir.join("reftable")) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("could not delete reftables: {}", strerror(&err)));
            }
            _ => {}
        }

        let mut err = String::new();
        if let Err(e) = std::fs::remove_file(git_dir.join("HEAD")) {
            write!(err, "could not delete stub HEAD: {}", strerror(&e)).expect("writing to a String");
        }
        if let Err(e) = std::fs::remove_file(git_dir.join("refs/heads")) {
            write!(err, "could not delete stub heads: {}", strerror(&e)).expect("writing to a String");
        }
        if let Err(e) = std::fs::remove_dir(git_dir.join("refs")) {
            write!(err, "could not delete refs directory: {}", strerror(&e)).expect("writing to a String");
        }
        if err.is_empty() { Ok(()) } else { Err(err) }
    }

    /// The object hash of this backend's stacks.
    fn hash_len(&self) -> usize {
        self.stack_options().hash_id.size()
    }

    /// `reftable_be_create_reflog()` (refs/reftable-backend.c:2400-2432) with
    /// `write_reflog_existence_table()` (:2368-2398): unless `name` has a
    /// reflog entry already, write the existence marker, an entry whose old
    /// and new values are both null, which reflog readers skip.
    pub fn create_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        self.check()?;
        let (stack, refname) = self.backend_for(name.as_bstr(), true)?;
        let opts = self.write_config().opts.clone();
        let mut st = lock(&stack);

        let mut add = st.addition_new(Some(&opts))?;
        add.add(&st, |wr, st| {
            let ts = st.next_update_index();
            if st.read_log(refname)?.is_some() {
                return Ok(());
            }
            wr.set_limits(ts, ts)?;
            wr.add_log(&LogRecord {
                refname: refname.into(),
                update_index: ts,
                value: LogValue::Update(LogUpdate::default()),
            })?;
            Ok(())
        })?;
        // Without a table, which an existing reflog leaves, there is nothing to
        // commit or compact.
        add.commit(&mut st)?;
        Ok(())
    }

    /// `reftable_be_delete_reflog()` (refs/reftable-backend.c:2480-2510) with
    /// `write_reflog_delete_table()` (:2439-2478): a tombstone for each entry
    /// of the reflog of `name`.
    ///
    /// git fills `arg.refname` with the name it was given *before*
    /// `backend_for()` strips a `worktrees/<id>/` or `main-worktree/` prefix
    /// (:2486-2491), and seeks, compares and writes tombstones under that full
    /// name in the stack the stripped name routes to. That stack keeps the
    /// worktree's reflog under the stripped name, so a prefixed name finds no
    /// entry and the reflog stays.
    pub fn delete_reflog(&self, name: &FullNameRef) -> Result<(), Error> {
        let refname = name.as_bstr();
        let (stack, _) = self.backend_for(refname, true)?;
        let opts = self.write_config().opts.clone();
        let mut st = lock(&stack);
        st.add(
            |wr, st| {
                let ts = st.next_update_index();
                wr.set_limits(ts, ts)?;
                let mut it = st.log_iterator()?;
                // A positive return of the seek ends the loop before it starts.
                if !it.seek_log(refname)? {
                    return Ok(());
                }
                let mut log = LogRecord::default();
                while it.next_log(&mut log)? && log.refname == refname {
                    if log.is_deletion() {
                        continue;
                    }
                    wr.add_log(&LogRecord {
                        refname: refname.into(),
                        update_index: log.update_index,
                        value: LogValue::Deletion,
                    })?;
                }
                Ok(())
            },
            Some(&opts),
        )?;
        Ok(())
    }

    /// `reftable_be_reflog_expire()` (refs/reftable-backend.c:2575-2737) with
    /// `write_reflog_expiry_table()` (:2512-2573): rewrite the reflog of `name`
    /// in a new table, turning each entry `policy` prunes into a tombstone and,
    /// with [`ExpireFlags::rewrite`], chaining the kept ones. A reflog left
    /// without entries keeps an existence marker. With
    /// [`ExpireFlags::update_ref`], a reference that is not symbolic is set to
    /// the newest kept entry. With [`ExpireFlags::dry_run`] the table is
    /// written and discarded.
    pub fn reflog_expire(
        &self,
        name: &FullNameRef,
        flags: ExpireFlags,
        policy: &mut dyn ExpirePolicy,
    ) -> Result<(), Error> {
        self.check()?;
        let (stack, refname) = self.backend_for(name.as_bstr(), true)?;
        let opts = self.write_config().opts.clone();
        let hash_len = self.hash_len();
        let mut st = lock(&stack);

        let mut add = st.addition_new(Some(&opts))?;
        let res = (|| -> Result<bool, Error> {
            let mut it = st.log_iterator()?;
            let positioned = it.seek_log(refname)?;

            // `reftable_backend_read_ref()`: a symbolic or missing reference
            // leaves the value null.
            let oid = match st.read_ref(refname)? {
                Some(RefRecord {
                    value: RefValue::Val1(h) | RefValue::Val2 { value: h, .. },
                    ..
                }) => oid_of(&h, hash_len),
                _ => ObjectId::null(self.object_hash()),
            };
            policy.prepare(FullNameRef::new_unchecked(refname), &oid);

            // Newest first; existence markers are dropped and re-added below
            // if no entry survives.
            let mut logs = Vec::new();
            let mut log = LogRecord::default();
            while positioned && it.next_log(&mut log)? && log.refname == refname {
                match &log.value {
                    LogValue::Deletion => continue,
                    LogValue::Update(u) if is_null(&u.old_hash, hash_len) && is_null(&u.new_hash, hash_len) => {
                        continue;
                    }
                    LogValue::Update(_) => logs.push(std::mem::take(&mut log)),
                }
            }

            let mut rewritten = logs.clone();
            let mut last_hash: Option<Hash> = None;
            for (dest, log) in rewritten.iter_mut().zip(&logs).rev() {
                let u = log.update().expect("only updates were collected");
                if policy.should_prune(&log_line(u, hash_len)) {
                    dest.value = LogValue::Deletion;
                } else {
                    if flags.rewrite {
                        if let (Some(last), LogValue::Update(d)) = (&last_hash, &mut dest.value) {
                            d.old_hash = *last;
                        }
                    }
                    last_hash = Some(u.new_hash);
                }
            }

            let update_oid = match last_hash {
                Some(h) if flags.update_ref && !oid.is_null() => oid_of(&h, hash_len),
                _ => ObjectId::null(self.object_hash()),
            };
            let update_value = (!update_oid.is_null()).then(|| match policy.peel(&update_oid) {
                Some(peeled) => RefValue::Val2 {
                    value: hash_of(&update_oid),
                    target_value: hash_of(&peeled),
                },
                None => RefValue::Val1(hash_of(&update_oid)),
            });

            let mut failure = None;
            add.add(&st, |wr, st| {
                write_reflog_expiry_table(wr, st, refname, update_value, rewritten)
                    .map_err(|err| callback_failed(&mut failure, err))
            })
            .map_err(|err| addition_error(failure, err))?;
            Ok(true)
        })();

        // git calls the cleanup whenever the addition was created.
        policy.cleanup();
        res?;
        if !flags.dry_run {
            add.commit(&mut st)?;
        }
        Ok(())
    }

    /// The stack `refs_compact()` and friends work on: the worktree's own in a
    /// linked worktree, the main stack otherwise.
    fn optimize_stack(&self) -> Result<StackRef, Error> {
        self.check()?;
        match self.worktree_stack() {
            Some(stack) => Ok(stack),
            None => self.main_stack(),
        }
    }

    /// `reftable_be_optimize()` (refs/reftable-backend.c:1699-1730): compact the
    /// stack, only as far as needed with `auto` (`REFS_OPTIMIZE_AUTO`), then
    /// delete the tables no longer used.
    pub fn optimize(&self, auto: bool) -> Result<(), Error> {
        let stack = self.optimize_stack()?;
        let opts = &self.write_config().opts;
        let mut st = lock(&stack);
        let res = if auto {
            st.auto_compact(Some(opts))
        } else {
            st.compact_all(Some(opts), None)
        };
        if let Err(err) = res {
            return Err(message(format!("unable to compact stack: {err}")));
        }
        st.clean()?;
        Ok(())
    }

    /// `reftable_be_optimize_required()` (refs/reftable-backend.c:1732-1754):
    /// whether [`optimize()`](Self::optimize) would compact, by the geometric
    /// heuristic with `auto`, or whenever there are two tables or more.
    pub fn optimize_required(&self, auto: bool) -> Result<bool, Error> {
        let stack = self.optimize_stack()?;
        let opts = &self.write_config().opts;
        Ok(lock(&stack).compaction_required(Some(opts), auto))
    }

    /// `reftable_be_rename_ref()` (refs/reftable-backend.c:1987-2016): move
    /// `old` with its reflog to `new`, logging `logmsg` as `committer`.
    ///
    /// `logmsg` is normalized like `refs_rename_ref()` (refs.c:3126-3136) does.
    pub fn rename_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        self.copy_or_rename(old, new, &committer, logmsg, true)
    }

    /// `reftable_be_copy_ref()` (refs/reftable-backend.c:2018-2047): copy `old`
    /// with its reflog to `new`, logging `logmsg` as `committer`.
    ///
    /// `logmsg` is normalized like `refs_copy_existing_ref()` (refs.c:3138-3148) does.
    pub fn copy_ref(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
    ) -> Result<(), Error> {
        self.copy_or_rename(old, new, &committer, logmsg, false)
    }

    /// Both of [`rename_ref()`](Self::rename_ref) and
    /// [`copy_ref()`](Self::copy_ref): one table written by
    /// `write_copy_table()` to the stack of `new`.
    fn copy_or_rename(
        &self,
        old: &FullNameRef,
        new: &FullNameRef,
        committer: &gix_actor::SignatureRef<'_>,
        logmsg: &BStr,
        delete_old: bool,
    ) -> Result<(), Error> {
        self.check()?;
        let logmsg = crate::log::normalize_message(logmsg);
        let (stack, newname) = self.backend_for(new.as_bstr(), true)?;
        let opts = self.write_config().opts.clone();
        let mut st = lock(&stack);

        let mut failure = None;
        let arg = CopyArg {
            backend: self,
            stack: &stack,
            oldname: old.as_bstr(),
            newname,
            logmsg: strndup(logmsg.as_bstr(), opts.block_size as usize / 2),
            committer: fill_log_record(committer),
            delete_old,
        };
        st.add(
            |wr, st| {
                arg.write_copy_table(wr, st)
                    .map_err(|err| callback_failed(&mut failure, err))
            },
            Some(&opts),
        )
        .map_err(|err| addition_error(failure, err))
    }

    /// `refs_verify_refname_available()` (refs.c:2953-2968) for `refname`
    /// with no extra names, ignoring `skip`; `held` is the stack the caller
    /// locked. The error is git's message.
    fn verify_refname_available(
        &self,
        refname: &BStr,
        skip: Option<&BStr>,
        held: (&StackRef, &Stack),
    ) -> Result<(), Error> {
        let skip: BTreeSet<BString> = skip.into_iter().map(ToOwned::to_owned).collect();
        self.verify_refnames_available(&[refname.to_owned()], None, &skip, Some(held), None)
            .map_err(|err| match err {
                Unavailable::Conflict(msg) => message(msg),
                Unavailable::Backend(err) => err,
            })
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
        let stack = match worktree {
            None => self.main_stack()?,
            Some(id) => {
                // `backend_for_worktree()`, reached through a per-worktree ref.
                let mut name = BString::from("worktrees/");
                name.extend_from_slice(id);
                name.extend_from_slice(b"/HEAD");
                match self.backend_for(name.as_bstr(), false) {
                    Ok((stack, _)) => stack,
                    Err(_) => return Err(message(format!("reftable stack for worktree '{id}' is broken"))),
                }
            }
        };
        let hash_len = self.hash_len();
        let mut st = lock(&stack);
        let mut errors = false;

        // `reftable_fsck_error_handler()`: every library problem maps to a
        // message id of git's.
        errors |= gix_reftable::fsck::check(
            &st,
            |info| {
                let msg_id = match info.error {
                    gix_reftable::fsck::FsckError::TableName => "badReftableTableName",
                };
                report(FsckReport {
                    path: info.path.as_bytes().as_bstr(),
                    msg_id,
                    message: info.msg.to_owned(),
                }) != 0
            },
            |msg| verbose(msg),
        );

        // `ref_iterator_for_stack()` reloads; its raw iterator sees every record.
        let worktree_name = worktree.map_or_else(String::new, ToString::to_string);
        let records = (|| -> Result<Vec<RefRecord>, gix_reftable::Error> {
            st.reload()?;
            let mut it = st.ref_iterator()?;
            let mut records = Vec::new();
            if it.seek_ref(b"")? {
                let mut r = RefRecord::default();
                while it.next_ref(&mut r)? {
                    records.push(std::mem::take(&mut r));
                }
            }
            Ok(records)
        })()
        .map_err(|_| message(format!("could not read record for worktree '{worktree_name}'")))?;
        drop(st);

        for r in records {
            let mut path = BString::default();
            if let Some(id) = worktree {
                path.extend_from_slice(b"worktrees/");
                path.extend_from_slice(id);
                path.push(b'/');
            }
            path.extend_from_slice(&r.refname);
            let path = path.as_bstr();
            match &r.value {
                RefValue::Deletion => {}
                RefValue::Val1(h) | RefValue::Val2 { value: h, .. } => {
                    // `refs_fsck_ref()` (refs.c:324-334).
                    if is_null(h, hash_len) {
                        errors |= report(FsckReport {
                            path,
                            msg_id: "badRefOid",
                            message: format!("points to invalid object ID '{}'", oid_of(h, hash_len)),
                        }) != 0;
                    }
                }
                RefValue::Symref(target) => {
                    errors |= fsck_symref(r.refname.as_bstr(), target.as_bstr(), path, report);
                }
            }
        }
        Ok(errors)
    }
}

/// `refs_fsck_symref()` (refs.c:336-366): whether a report about the symbolic
/// reference `refname` pointing at `target` was an error.
fn fsck_symref(refname: &BStr, target: &BStr, path: &BStr, report: &mut dyn FnMut(FsckReport<'_>) -> i32) -> bool {
    let (_, _, stripped) = parse_worktree_ref(refname);
    if stripped == "HEAD"
        && !target.starts_with(b"refs/heads/")
        && report(FsckReport {
            path,
            msg_id: "badHeadTarget",
            message: format!("HEAD points to non-branch '{target}'"),
        }) != 0
    {
        return true;
    }
    if is_root_ref(target) {
        return false;
    }
    if !check_refname_format(target)
        && report(FsckReport {
            path,
            msg_id: "badReferentName",
            message: format!("points to invalid refname '{target}'"),
        }) != 0
    {
        return true;
    }
    !target.starts_with(b"refs/")
        && !target.starts_with(b"worktrees/")
        && report(FsckReport {
            path,
            msg_id: "symrefTargetIsNotARef",
            message: format!("points to non-ref target '{target}'"),
        }) != 0
}

/// `write_reflog_expiry_table()` (refs/reftable-backend.c:2512-2573): the
/// reference's new value if any, the existence marker if no entry is left,
/// then all `records`, newest first, kept or turned into tombstones.
fn write_reflog_expiry_table(
    wr: &mut Writer<TableFile>,
    st: &Stack,
    refname: &BStr,
    update_value: Option<RefValue>,
    records: Vec<LogRecord>,
) -> Result<(), Error> {
    let ts = st.next_update_index();
    let live_records = records.iter().filter(|r| !r.is_deletion()).count();
    wr.set_limits(ts, ts)?;

    if let Some(value) = update_value {
        wr.add_ref(&RefRecord {
            refname: refname.into(),
            update_index: ts,
            value,
        })?;
    }

    // An emptied reflog keeps a placeholder saying it still exists.
    if live_records == 0 {
        wr.add_log(&LogRecord {
            refname: refname.into(),
            update_index: ts,
            value: LogValue::Update(LogUpdate::default()),
        })?;
    }

    for record in records {
        wr.add_log(&record)?;
    }
    Ok(())
}

/// `struct write_copy_arg` (refs/reftable-backend.c:1765-1772).
struct CopyArg<'a> {
    backend: &'a Backend,
    /// The stack the table goes to, the one of `newname`.
    stack: &'a StackRef,
    oldname: &'a BStr,
    newname: &'a BStr,
    /// Already trimmed to half the block size.
    logmsg: BString,
    /// Name, email and time of the committer.
    committer: LogUpdate,
    delete_old: bool,
}

impl CopyArg<'_> {
    /// `write_copy_table()` (refs/reftable-backend.c:1774-1985).
    fn write_copy_table(&self, wr: &mut Writer<TableFile>, st: &Stack) -> Result<(), Error> {
        let Some(old_ref) = st.read_ref(self.oldname)? else {
            return Err(message(format!("refname {} not found", self.oldname)));
        };
        let old_val = match &old_ref.value {
            RefValue::Symref(_) => {
                return Err(message(format!(
                    "refname {} is a symbolic ref, copying it is not supported",
                    self.oldname
                )));
            }
            RefValue::Val1(h) | RefValue::Val2 { value: h, .. } => *h,
            RefValue::Deletion => unreachable!("read_ref() skips deletions"),
        };

        // Nothing to do when the names are the same.
        if self.oldname == self.newname {
            return Ok(());
        }

        let skip = self.delete_old.then_some(self.oldname);
        self.backend
            .verify_refname_available(self.newname, skip, (self.stack, st))?;

        // A rename needs two update indices: the new reflog records both the
        // deletion of the old branch and the creation of the new one, and a
        // reflog cannot change twice in one update.
        let deletion_ts = st.next_update_index();
        let creation_ts = deletion_ts + u64::from(self.delete_old);
        wr.set_limits(deletion_ts, creation_ts)?;

        let mut refs = vec![RefRecord {
            refname: self.newname.into(),
            update_index: creation_ts,
            value: old_ref.value.clone(),
        }];
        if self.delete_old {
            refs.push(RefRecord {
                refname: self.oldname.into(),
                update_index: deletion_ts,
                value: RefValue::Deletion,
            });
        }
        wr.add_refs(&mut refs)?;

        let entry = |refname: &BStr, update_index: u64, old_hash: Hash, new_hash: Hash| LogRecord {
            refname: refname.into(),
            update_index,
            value: LogValue::Update(LogUpdate {
                old_hash,
                new_hash,
                message: Some(self.logmsg.clone()),
                ..self.committer.clone()
            }),
        };

        // A rename deletes and recreates the branch in its reflog, as the files
        // backend does, and HEAD logs the deletion if it points at the branch.
        let mut logs = Vec::new();
        if self.delete_old {
            logs.push(entry(self.newname, deletion_ts, old_val, Hash::default()));
            let head_points_here = matches!(st.read_ref(b"HEAD")?, Some(RefRecord { value: RefValue::Symref(target), .. }) if target == self.oldname);
            if head_points_here {
                logs.push(entry(b"HEAD".as_bstr(), deletion_ts, old_val, Hash::default()));
            }
        }
        logs.push(entry(self.newname, creation_ts, Hash::default(), old_val));

        // Copy the old reflog over, deleting it when renaming.
        let mut it = st.log_iterator()?;
        if it.seek_log(self.oldname)? {
            let mut old_log = LogRecord::default();
            while it.next_log(&mut old_log)? && old_log.refname == self.oldname {
                if old_log.is_deletion() {
                    continue;
                }
                let update_index = old_log.update_index;
                let mut copied = std::mem::take(&mut old_log);
                copied.refname = self.newname.into();
                logs.push(copied);
                if self.delete_old {
                    logs.push(LogRecord {
                        refname: self.oldname.into(),
                        update_index,
                        value: LogValue::Deletion,
                    });
                }
            }
        }
        wr.add_logs(&mut logs)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refname_format() {
        for valid in ["refs/heads/main", "refs/heads/a.b", "worktrees/wt/HEAD", "a/b"] {
            assert!(check_refname_format(valid.as_bytes()), "{valid}");
        }
        for invalid in [
            "HEAD",
            "refs/heads/",
            "refs//x",
            "refs/heads/a..b",
            "refs/heads/x.lock",
            "refs/heads/.x",
            "refs/heads/x.",
            "refs/heads/a@{b",
            "refs/heads/a b",
            "refs/heads/a*",
            "@",
        ] {
            assert!(!check_refname_format(invalid.as_bytes()), "{invalid}");
        }
    }

    #[test]
    fn root_refs() {
        assert!(is_root_ref(b"HEAD"));
        assert!(is_root_ref(b"ORIG_HEAD"));
        assert!(is_root_ref(b"MERGE_AUTOSTASH"));
        assert!(!is_root_ref(b"FETCH_HEAD"), "pseudo refs are not root refs");
        assert!(!is_root_ref(b"FOO"));
        assert!(!is_root_ref(b"refs/heads/main"));
    }

    #[test]
    fn committer_time_and_zone() {
        let sig = gix_actor::SignatureRef {
            name: "C O Mitter".into(),
            email: "committer@example.com".into(),
            time: "1112911993 -0700",
        };
        let u = fill_log_record(&sig);
        assert_eq!((u.time, u.tz_offset), (1112911993, -700));
        let line = log_line(&u, 20);
        assert_eq!(line.signature.time.offset, -7 * 3600);
    }
}
