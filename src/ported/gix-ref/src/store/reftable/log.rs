//! Reading reflogs: `reftable_be_reflog_exists()`, the reflog iterator and
//! `for_each_reflog_ent[_reverse]()` (refs/reftable-backend.c:2048-2366).
//!
//! The file store's reflog API reads the files format; the entries of a
//! reftable reflog are rendered into that format so the same parser serves
//! both (see [`ReflogSource`](crate::file::log::iter::ReflogSource)).

use std::{cmp::Ordering, io::Write};

use gix_object::bstr::{BString, ByteSlice};
use gix_reftable::{LogRecord, LogUpdate};

use super::{Backend, Error, StackRef, WorktreeType, lock, parse_worktree_ref};
use crate::{FullName, FullNameRef};

/// `crud()` (ident.c:203-214): characters trimmed from the ends of an
/// identity's name and email.
fn crud(c: u8) -> bool {
    c <= 32 || matches!(c, b',' | b':' | b';' | b'<' | b'>' | b'"' | b'\\' | b'\'')
}

/// `strbuf_addstr_without_crud()` (ident.c:229-266): `src` without crud at
/// either end and without the delimiters `\n`, `<` and `>`.
fn push_without_crud(out: &mut Vec<u8>, src: &[u8]) {
    let src = src.split(|&c| c == 0).next().unwrap_or_default();
    let start = src.iter().position(|&c| !crud(c)).unwrap_or(src.len());
    let end = src.iter().rposition(|&c| !crud(c)).map_or(start, |i| i + 1);
    out.extend(src[start..end].iter().filter(|c| !matches!(c, b'\n' | b'<' | b'>')));
}

/// One reflog entry as `each_reflog_ent_fn` (refs.h) receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// The value before the update.
    pub old_oid: gix_hash::ObjectId,
    /// The value after the update.
    pub new_oid: gix_hash::ObjectId,
    /// `Name <email>`, as `fmt_ident()` formats the stored identity.
    pub committer: BString,
    /// Seconds since the epoch.
    pub timestamp: u64,
    /// The time zone as the decimal number `HHMM`, signed (`-0700` is `-700`).
    pub tz: i32,
    /// The message as stored, with its trailing newline.
    pub message: BString,
}

/// `reftable_reflog_iterator` (refs/reftable-backend.c:2048-2149): the names
/// of the references with a reflog in one stack.
struct NameIter {
    iter: Option<gix_reftable::Iterator>,
    err: Option<Error>,
    done: bool,
    last_name: BString,
}

impl NameIter {
    /// `reflog_iterator_for_stack()` (refs/reftable-backend.c:2119-2149).
    fn new(backend: &Backend, stack: Result<StackRef, Error>) -> Self {
        let res = stack.and_then(|stack| {
            backend.check()?;
            let mut stack = lock(&stack);
            stack.reload()?;
            let mut iter = stack.log_iterator()?;
            let found = iter.seek_log(b"")?;
            Ok((iter, found))
        });
        let (iter, err, done) = match res {
            Ok((iter, found)) => (Some(iter), None, !found),
            Err(err) => (None, Some(err), true),
        };
        NameIter {
            iter,
            err,
            done,
            last_name: BString::default(),
        }
    }

    /// `reftable_reflog_iterator_advance()` (refs/reftable-backend.c:2057-2093).
    fn advance(&mut self) -> Option<Result<FullName, Error>> {
        if let Some(err) = self.err.take() {
            self.done = true;
            return Some(Err(err));
        }
        while !self.done {
            let mut log = LogRecord::default();
            match self.iter.as_mut().expect("set unless failed").next_log(&mut log) {
                Ok(true) => {}
                Ok(false) => break,
                Err(err) => {
                    self.done = true;
                    return Some(Err(err.into()));
                }
            }
            // Entries of one reference are adjacent; each name is yielded once.
            if log.is_deletion() || log.refname == self.last_name {
                continue;
            }
            // `check_refname_format(…, REFNAME_ALLOW_ONELEVEL)`, as the file
            // store checks the names of reflog files.
            if gix_validate::reference::name_partial(log.refname.as_bstr()).is_err() {
                continue;
            }
            self.last_name = log.refname.clone();
            return Some(Ok(FullName(log.refname)));
        }
        self.done = true;
        None
    }
}

impl Backend {
    /// `reftable_be_reflog_exists()` (refs/reftable-backend.c:2306-2350):
    /// whether `name` has at least one reflog entry that is not a deletion.
    /// As in git, failing to read the stack means there is none.
    pub fn reflog_exists(&self, name: &FullNameRef) -> Result<bool, Error> {
        let exists = || -> Result<bool, Error> {
            self.check()?;
            let (stack, refname) = self.backend_for(name.as_bstr(), true)?;
            let mut iter = lock(&stack).log_iterator()?;
            if !iter.seek_log(refname)? {
                return Ok(false);
            }
            let mut log = LogRecord::default();
            while iter.next_log(&mut log)? {
                if log.refname != refname {
                    return Ok(false);
                }
                if !log.is_deletion() {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        Ok(exists().unwrap_or(false))
    }

    /// The reflog records of `name`, newest first, without deletions; `None`
    /// if there are none. As in git, the stack is not reloaded first
    /// (refs/reftable-backend.c:2207-2211).
    fn reflog_records(&self, name: &FullNameRef) -> Result<Option<Vec<LogUpdate>>, Error> {
        self.check()?;
        let (stack, refname) = self.backend_for(name.as_bstr(), false)?;
        let mut iter = lock(&stack).log_iterator()?;
        let mut records = None;
        if iter.seek_log(refname)? {
            let mut log = LogRecord::default();
            while iter.next_log(&mut log)? && log.refname == refname {
                if let Some(update) = log.update() {
                    records.get_or_insert_with(Vec::new).push(update.clone());
                }
            }
        }
        Ok(records)
    }

    /// `yield_log_record()` (refs/reftable-backend.c:2167-2191): what the
    /// callback of `for_each_reflog_ent()` receives for `update`, `None` for
    /// the existence marker, an entry from the null object id to the null
    /// object id, which callers must not see.
    fn reflog_entry(&self, update: &LogUpdate) -> Option<ReflogEntry> {
        let old_oid = self.oid_from_hash(&update.old_hash);
        let new_oid = self.oid_from_hash(&update.new_hash);
        if old_oid.is_null() && new_oid.is_null() {
            return None;
        }
        // `fmt_ident(name, email, WANT_COMMITTER_IDENT, NULL, IDENT_NO_DATE)`.
        let mut committer = Vec::new();
        push_without_crud(&mut committer, &update.name);
        committer.extend_from_slice(b" <");
        push_without_crud(&mut committer, &update.email);
        committer.push(b'>');
        Some(ReflogEntry {
            old_oid,
            new_oid,
            committer: committer.into(),
            timestamp: update.time,
            tz: i32::from(update.tz_offset),
            // A C string: whatever follows a NUL is not seen.
            message: update.message_or_empty().split(|&c| c == 0).next().unwrap_or_default().into(),
        })
    }

    /// `entry` rendered as a line of the files format (`old SP new SP name
    /// <email> SP time SP tz TAB msg LF`), which is what the callback receives
    /// from the files backend.
    fn write_log_line(out: &mut Vec<u8>, entry: &ReflogEntry) {
        write!(
            out,
            "{} {} {} {} {:+05}",
            entry.old_oid, entry.new_oid, entry.committer, entry.timestamp, entry.tz
        )
        .expect("writing to a Vec cannot fail");
        // Messages are stored with one trailing newline (`reftable_writer_add_log()`,
        // reftable/writer.c), like the files backend hands them on. An empty
        // message is written without the tab, as the files backend does.
        let message = entry.message.strip_suffix(b"\n").unwrap_or(&entry.message);
        if !message.is_empty() {
            out.push(b'\t');
            out.extend_from_slice(message);
        }
        out.push(b'\n');
    }

    /// `reftable_be_for_each_reflog_ent()` (refs/reftable-backend.c:2243-2304)
    /// and `reftable_be_for_each_reflog_ent_reverse()` (:2193-2241): the
    /// entries of the reflog of `name`, oldest first, or newest first with
    /// `reverse`. A reference without a reflog has no entries, which git does
    /// not tell apart from an empty reflog for this backend.
    pub fn reflog_entries(&self, name: &FullNameRef, reverse: bool) -> Result<Vec<ReflogEntry>, Error> {
        let records = self.reflog_records(name)?.unwrap_or_default();
        let entries = records.iter().filter_map(|update| self.reflog_entry(update));
        Ok(if reverse {
            entries.collect()
        } else {
            let mut entries: Vec<_> = entries.collect();
            entries.reverse();
            entries
        })
    }

    /// `reftable_be_for_each_reflog_ent()` (refs/reftable-backend.c:2243-2304):
    /// the reflog of `name`, oldest entry first, as lines of the files format
    /// written into `buf`. `Ok(false)` if there is none.
    pub(crate) fn reflog_into(&self, name: &FullNameRef, buf: &mut Vec<u8>) -> Result<bool, Error> {
        buf.clear();
        let Some(records) = self.reflog_records(name)? else {
            return Ok(false);
        };
        for update in records.iter().rev() {
            if let Some(entry) = self.reflog_entry(update) {
                Self::write_log_line(buf, &entry);
            }
        }
        Ok(true)
    }

    /// `reftable_be_reflog_iterator_begin()` (refs/reftable-backend.c:2151-2165):
    /// the names of all references that have a reflog, in name order. In a
    /// linked worktree those of its own stack and the shared ones of the main
    /// stack are merged (`ref_iterator_select()`, refs/iterator.c:97-130).
    pub fn reflog_names(&self) -> Result<Vec<FullName>, Error> {
        let mut common = NameIter::new(self, self.main_stack());
        let Some(worktree) = self.worktree_stack() else {
            return std::iter::from_fn(|| common.advance()).collect();
        };
        let mut worktree = NameIter::new(self, Ok(worktree));

        let mut names = Vec::new();
        let mut wt_next = worktree.advance().transpose()?;
        let mut common_next = common.advance().transpose()?;
        loop {
            match (&wt_next, &common_next) {
                (None, None) => break,
                (Some(_), None) => {
                    names.extend(wt_next.take());
                    wt_next = worktree.advance().transpose()?;
                }
                (wt, Some(common_name)) => {
                    if let Some(wt) = wt {
                        match wt.cmp(common_name) {
                            Ordering::Less => {
                                names.extend(wt_next.take());
                                wt_next = worktree.advance().transpose()?;
                                continue;
                            }
                            Ordering::Equal => {
                                names.extend(wt_next.take());
                                wt_next = worktree.advance().transpose()?;
                                common_next = common.advance().transpose()?;
                                continue;
                            }
                            Ordering::Greater => {}
                        }
                    }
                    let shared = parse_worktree_ref(common_name.as_bstr()).0 == WorktreeType::Shared;
                    let name = common_next.take();
                    if shared {
                        names.extend(name);
                    }
                    common_next = common.advance().transpose()?;
                }
            }
        }
        Ok(names)
    }
}
