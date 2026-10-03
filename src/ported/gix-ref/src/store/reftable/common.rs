//! What transactions and maintenance both need: reading a reference or
//! reflog from a stack the caller may hold already, the refname-availability
//! check, and filling a log record from the committer.
//!
//! A caller holding a stack's mutex passes it as `held`; it is read as it is,
//! which is what git's reload amounts to for a stack an addition has locked,
//! and locking it again would deadlock. Every other stack is locked and
//! reloaded for the read.

use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use gix_hash::oid;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{LogRecord, LogUpdate, RefRecord, Stack, record::Hash};

use super::{Backend, Error, StackRef, WorktreeType, lock, parse_worktree_ref, worktree::is_pseudo_ref};
use crate::{FullName, Target};

/// A stack whose mutex the caller holds, with its guarded value.
pub(super) type Held<'a> = Option<(&'a StackRef, &'a Stack)>;

/// `atol()` / `atoi()`: leading white space, an optional sign, then digits.
pub(super) fn atol(s: &[u8]) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let val = digits
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .fold(0i64, |acc, &c| acc.wrapping_mul(10).wrapping_add(i64::from(c - b'0')));
    if neg { -val } else { val }
}

/// `xstrndup(msg, n)`: at most `n` bytes of `msg`, ending early at a NUL.
pub(super) fn strndup(msg: &[u8], n: usize) -> BString {
    let msg = msg.find_byte(0).map_or(msg, |nul| &msg[..nul]);
    msg[..msg.len().min(n)].into()
}

/// `oid` in a record's hash buffer.
pub(super) fn hash_of(oid: &oid) -> Hash {
    let mut h = Hash::default();
    h[..oid.as_bytes().len()].copy_from_slice(oid.as_bytes());
    h
}

/// `fill_reftable_log_record()` (refs/reftable-backend.c:297-321): the
/// committer part of a log record, from git's raw `<seconds> <+|-HHMM>`
/// time as `split_ident_line()` (ident.c:275-358) splits it. The name and
/// email are taken without surrounding white space.
pub(super) fn fill_log_record(committer: &gix_actor::SignatureRef<'_>) -> LogUpdate {
    let committer = committer.trim();
    let time = committer.time.as_bytes().trim_start();
    let date_end = time.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut tz = time[date_end..].trim_start();
    let mut sign = 1;
    if tz.first() == Some(&b'-') {
        sign = -1;
        tz = &tz[1..];
    }
    if tz.first() == Some(&b'+') {
        sign = 1;
        tz = &tz[1..];
    }
    LogUpdate {
        name: committer.name.to_owned(),
        email: committer.email.to_owned(),
        time: atol(&time[..date_end]) as u64,
        tz_offset: (sign * atol(tz)) as i16,
        ..LogUpdate::default()
    }
}

/// git's `struct string_list` as `transaction->refnames` uses it: sorted once
/// (`string_list_sort()`), appended to unsorted afterwards
/// (`string_list_append()`, refs.c:1361), and searched by bisection all the
/// same (`get_entry_index()`, string-list.c:18-41). A name appended after
/// the sort can therefore be missed by a lookup, exactly as git misses it.
#[derive(Debug, Default, Clone)]
pub(super) struct StringList(Vec<BString>);

impl StringList {
    /// `string_list_append()`.
    pub(super) fn append(&mut self, string: BString) {
        self.0.push(string);
    }

    /// `string_list_sort()`.
    pub(super) fn sort(&mut self) {
        self.0.sort();
    }

    /// The items in list order.
    pub(super) fn items(&self) -> &[BString] {
        &self.0
    }

    /// `get_entry_index()`: where `string` is or would be inserted, and
    /// whether it was found there.
    fn entry_index(&self, string: &[u8]) -> (usize, bool) {
        let (mut left, mut right) = (0, self.0.len());
        while left < right {
            let middle = left + (right - left) / 2;
            match string.cmp(self.0[middle].as_slice()) {
                std::cmp::Ordering::Less => right = middle,
                std::cmp::Ordering::Greater => left = middle + 1,
                std::cmp::Ordering::Equal => return (middle, true),
            }
        }
        (right, false)
    }

    /// `string_list_has_string()`.
    pub(super) fn has(&self, string: &[u8]) -> bool {
        self.entry_index(string).1
    }

    /// `string_list_find_insert_index()`.
    pub(super) fn find_insert_index(&self, string: &[u8]) -> usize {
        self.entry_index(string).0
    }

    /// `string_list_remove()`.
    pub(super) fn remove(&mut self, string: &[u8]) {
        if let (index, true) = self.entry_index(string) {
            self.0.remove(index);
        }
    }
}

/// Why `refs_verify_refnames_available()` refused.
pub(super) enum Unavailable {
    /// `REF_TRANSACTION_ERROR_NAME_CONFLICT` with git's message.
    Conflict(String),
    /// Reading a stack failed.
    Backend(Error),
}

impl Backend {
    /// The object hash of this backend's stacks.
    pub(super) fn object_hash(&self) -> gix_hash::Kind {
        gix_hash::Kind::from_hex_len(self.stack_options().hash_id.size() * 2)
            .expect("the stack hash is one gix-hash supports")
    }

    /// Run `f` on the stack `refname` lives in and the name within that stack:
    /// `held` as it is if that is the one, any other locked and reloaded first.
    pub(super) fn with_stack_for<T>(
        &self,
        refname: &BStr,
        held: Held<'_>,
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

    /// `refs_read_raw_ref()` (refs.c:2094-2105): the pseudo references
    /// `FETCH_HEAD` and `MERGE_HEAD` from their files
    /// (`refs_read_special_head()`, refs.c:2070-2092, a file that cannot be
    /// read or parsed being absent), every other one from its stack. A
    /// symbolic target is returned as stored.
    pub(super) fn read_raw_ref_in(&self, refname: &BStr, held: Held<'_>) -> Result<Option<Target>, Error> {
        if is_pseudo_ref(refname) {
            let Ok(contents) = std::fs::read(self.git_dir().join(gix_path::from_bstr(refname))) else {
                return Ok(None);
            };
            let name = FullName(refname.to_owned());
            return Ok(crate::file::loose::Reference::try_from_path(name, &contents, self.object_hash())
                .ok()
                .map(|r| r.target));
        }
        self.with_stack_for(refname, held, |stack, name| self.read_ref(stack, name))
    }

    /// `reftable_be_reflog_exists()` (refs/reftable-backend.c:2306-2366):
    /// whether `refname` has a log record that is not a deletion.
    pub(super) fn reflog_exists_in(&self, refname: &BStr, held: Held<'_>) -> Result<bool, Error> {
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

    /// The first reference whose name starts with `prefix` and is not in
    /// `skip`, as `refs_ref_iterator_begin(refs, prefix, NULL, 0,
    /// REFS_FOR_EACH_INCLUDE_BROKEN)` yields them (`reftable_be_iterator_begin()`,
    /// refs/reftable-backend.c:846-878): only names below `refs/`, and in a
    /// linked worktree the worktree's references merged with the shared ones
    /// of the main stack (`ref_iterator_select()`, refs/iterator.c:97-130).
    pub(super) fn first_ref_with_prefix(
        &self,
        prefix: &[u8],
        skip: &BTreeSet<BString>,
        held: Held<'_>,
    ) -> Result<Option<BString>, Error> {
        let first_in = |st: &Stack, shared_only: bool| -> Result<Option<BString>, Error> {
            let mut it = st.ref_iterator()?;
            if !it.seek_ref(prefix)? {
                return Ok(None);
            }
            let mut r = RefRecord::default();
            while it.next_ref(&mut r)? {
                if !r.refname.starts_with(b"refs/") {
                    continue;
                }
                if !r.refname.starts_with(prefix) {
                    break;
                }
                if r.is_deletion() || skip.contains(&r.refname) {
                    continue;
                }
                if shared_only && parse_worktree_ref(r.refname.as_bstr()).0 != WorktreeType::Shared {
                    continue;
                }
                return Ok(Some(r.refname));
            }
            Ok(None)
        };
        let search = |stack: StackRef, shared_only: bool| -> Result<Option<BString>, Error> {
            if let Some((held_ref, held_stack)) = held {
                if Arc::ptr_eq(&stack, held_ref) {
                    return first_in(held_stack, shared_only);
                }
            }
            let mut st = lock(&stack);
            st.reload()?;
            first_in(&st, shared_only)
        };
        let worktree = self.worktree_stack();
        let main = search(self.main_stack()?, worktree.is_some())?;
        let Some(worktree) = worktree else {
            return Ok(main);
        };
        let private = search(worktree, false)?;
        Ok(match (private, main) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// `refs_verify_refnames_available()` (refs.c:2789-2951) for a transaction
    /// that is not the initial one: none of `refnames` may have a leading
    /// directory that is a reference or one of `extras`, nor be the directory
    /// of an existing reference or of one of `extras`; names in `skip` do not
    /// count. git's single-name `refs_verify_refname_available()`
    /// (:2953-2968) is this with one name.
    ///
    /// `reject` is `ref_transaction_maybe_set_rejected()` for a transaction
    /// that may fail partially: it is handed the position in `refnames` and
    /// the message of a conflict, and when it takes the conflict the check
    /// moves on to the next name, which is removed from `extras`, the
    /// transaction's names (`string_list_remove(&transaction->refnames, …)`,
    /// refs.c:1296-1300).
    pub(super) fn verify_refnames_available(
        &self,
        refnames: &[BString],
        mut extras: Option<&mut StringList>,
        skip: &BTreeSet<BString>,
        held: Held<'_>,
        mut reject: Option<&mut dyn FnMut(usize, &str) -> bool>,
    ) -> Result<(), Unavailable> {
        let mut dirnames = HashSet::<BString>::new();
        let mut conflicting_dirnames = HashSet::<BString>::new();
        'next_ref: for (idx, refname) in refnames.iter().enumerate() {
            let mut take = |message: &str, extras: &mut Option<&mut StringList>| {
                let taken = reject.as_mut().is_some_and(|reject| reject(idx, message));
                if taken {
                    if let Some(extras) = extras {
                        extras.remove(refname);
                    }
                }
                taken
            };
            for slash in refname.find_iter(b"/") {
                let dirname: BString = refname[..slash].into();
                if skip.contains(&dirname) {
                    continue;
                }
                if !dirnames.insert(dirname.clone()) {
                    continue;
                }
                // Any error reading it counts as absent.
                if conflicting_dirnames.contains(&dirname)
                    || matches!(self.read_raw_ref_in(dirname.as_ref(), held), Ok(Some(_)))
                {
                    let message = format!("'{dirname}' exists; cannot create '{refname}'");
                    if take(&message, &mut extras) {
                        dirnames.remove(&dirname);
                        conflicting_dirnames.insert(dirname);
                        continue 'next_ref;
                    }
                    return Err(Unavailable::Conflict(message));
                }
                if extras.as_ref().is_some_and(|extras| extras.has(&dirname)) {
                    let message = format!("cannot process '{refname}' and '{dirname}' at the same time");
                    if take(&message, &mut extras) {
                        dirnames.remove(&dirname);
                        continue 'next_ref;
                    }
                    return Err(Unavailable::Conflict(message));
                }
            }

            let mut prefix = refname.clone();
            prefix.push(b'/');
            let existing = self
                .first_ref_with_prefix(&prefix, skip, held)
                .map_err(Unavailable::Backend)?;
            if let Some(existing) = existing {
                let message = format!("'{existing}' exists; cannot create '{refname}'");
                if take(&message, &mut extras) {
                    continue 'next_ref;
                }
                return Err(Unavailable::Conflict(message));
            }
            // `find_descendant_ref()` (refs.c:1787-1811).
            if let Some(extra) = extras.as_ref().and_then(|extras| {
                extras.items()[extras.find_insert_index(&prefix)..]
                    .iter()
                    .take_while(|e| e.starts_with(&prefix))
                    .find(|e| !skip.contains(*e))
                    .cloned()
            }) {
                let message = format!("cannot process '{refname}' and '{extra}' at the same time");
                if take(&message, &mut extras) {
                    continue 'next_ref;
                }
                return Err(Unavailable::Conflict(message));
            }
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
        let update = fill_log_record(&sig);
        assert_eq!(update.name, "A U Thor", "split_ident_line() trims the name");
        assert_eq!(update.time, 1_700_000_000);
        assert_eq!(update.tz_offset, -130, "the offset stays HHMM, signed");
        let sig = gix_actor::SignatureRef { time: "5 +0200", ..sig };
        assert_eq!(fill_log_record(&sig).tz_offset, 200);
        let sig = gix_actor::SignatureRef {
            time: "1112911993 -0700",
            ..sig
        };
        assert_eq!(
            (fill_log_record(&sig).time, fill_log_record(&sig).tz_offset),
            (1112911993, -700)
        );
    }

    #[test]
    fn strndup_stops_at_nul_and_length() {
        assert_eq!(strndup(b"abc\0def", 10), "abc");
        assert_eq!(strndup(b"abcdef", 3), "abc");
    }
}
