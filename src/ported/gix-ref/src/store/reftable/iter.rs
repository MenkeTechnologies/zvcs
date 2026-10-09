//! Iterating references: `reftable_be_iterator_begin()` and the
//! `reftable_ref_iterator` (refs/reftable-backend.c:539-878). In a linked
//! worktree the worktree's stack and the main stack are merged, the former
//! contributing its per-worktree references and the latter everything else
//! (`ref_iterator_select()`, refs/iterator.c:97-130).
//!
//! References are yielded as stored, like the file store's iteration yields
//! them: a symbolic reference is not resolved and no reference is checked to
//! point to an existing object, which git's iterator does for its callers
//! (`refs_resolve_ref_unsafe()` and `ref_resolves_to_object()`,
//! refs/reftable-backend.c:671-707) and which needs the object database; a null
//! object id is not filtered either.
//! What can be decided from the names alone is decided here as git does.

use std::cmp::Ordering;

use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{RefRecord, RefValue};

use super::{Backend, Error, StackRef, WorktreeType, lock, parse_worktree_ref, worktree::is_root_ref};
use crate::{FullName, Reference, Target};

/// `refname_is_safe()` (refs.c:382-412): a name that cannot escape `refs/`,
/// or one of only uppercase letters and `_`.
fn refname_is_safe(name: &[u8]) -> bool {
    match name.strip_prefix(b"refs/") {
        // `normalize_path_copy()` must leave the rest unchanged, so it may
        // not be empty, start or end with `/`, or have empty, `.` or `..`
        // components.
        Some(rest) => !rest.is_empty() && rest.split_str("/").all(|c| !c.is_empty() && c != b"." && c != b".."),
        None => !name.is_empty() && name.iter().all(|&c| c.is_ascii_uppercase() || c == b'_'),
    }
}

/// `filter_exclude_patterns()` (refs/reftable-backend.c:772-806): the
/// patterns without glob characters (`is_glob_special()`), sorted.
fn filter_exclude_patterns(patterns: &[BString]) -> Vec<BString> {
    let mut filtered: Vec<BString> = patterns
        .iter()
        .filter(|p| !p.iter().any(|c| matches!(c, b'*' | b'?' | b'[' | b'\\')))
        .cloned()
        .collect();
    filtered.sort();
    filtered
}

/// `strncmp(a, b, n)` for names without NUL bytes.
fn strncmp(a: &[u8], b: &[u8], n: usize) -> Ordering {
    a[..a.len().min(n)].cmp(&b[..b.len().min(n)])
}

/// `struct reftable_ref_iterator` (refs/reftable-backend.c:539-553): the
/// references of one stack.
struct StackIter {
    iter: Option<gix_reftable::Iterator>,
    /// `iter->err`: an error to report on the next advance, after which the
    /// iterator is done.
    err: Option<Error>,
    /// Set once `iter->err` is positive or an error was reported.
    done: bool,
    prefix: BString,
    exclude_patterns: Vec<BString>,
    exclude_patterns_index: usize,
    flags: u32,
    hash_len: usize,
}

impl StackIter {
    /// `ref_iterator_for_stack()` (refs/reftable-backend.c:808-844).
    fn new(
        backend: &Backend,
        stack: Result<StackRef, Error>,
        prefix: &BStr,
        exclude_patterns: &[BString],
        flags: u32,
    ) -> Self {
        let mut this = StackIter {
            iter: None,
            err: None,
            done: false,
            prefix: prefix.to_owned(),
            exclude_patterns: filter_exclude_patterns(exclude_patterns),
            exclude_patterns_index: 0,
            flags,
            hash_len: backend.stack_options().hash_id.size(),
        };
        // `ret = refs->err; if (ret) goto done;` leaves `iter->err` negative, so the iterator
        // ends in `ITER_ERROR` before it yields anything. The walkers that list references
        // (`for-each-ref`, `rev-list --all`, `log --all`) drop that status, which is an empty
        // listing; it is kept off the item stream so none of them reports it.
        if backend.check().is_err() {
            this.done = true;
            return this;
        }
        let res = stack.and_then(|stack| {
            backend.check()?;
            let mut stack = lock(&stack);
            stack.reload()?;
            let mut iter = stack.ref_iterator()?;
            // `reftable_ref_iterator_seek()` with `REF_ITERATOR_SEEK_SET_PREFIX`.
            let found = iter.seek_ref(prefix)?;
            Ok((iter, found))
        });
        match res {
            Ok((iter, found)) => {
                this.iter = Some(iter);
                this.done = !found;
            }
            Err(err) => this.err = Some(err),
        }
        this
    }

    /// `should_exclude_current_ref()` (refs/reftable-backend.c:561-622):
    /// whether `refname` matches an exclude pattern, in which case the
    /// iterator is moved past all references matching it.
    fn should_exclude_current_ref(&mut self, refname: &[u8]) -> bool {
        while let Some(pattern) = self.exclude_patterns.get(self.exclude_patterns_index) {
            match strncmp(refname, pattern, pattern.len()) {
                Ordering::Greater => {
                    self.exclude_patterns_index += 1;
                    continue;
                }
                Ordering::Less => return false,
                Ordering::Equal => {}
            }
            // Seek past every name that has the pattern as prefix, by
            // appending the highest possible byte.
            let mut ref_after_pattern = pattern.clone();
            ref_after_pattern.push(0xff);
            self.exclude_patterns_index += 1;
            let iter = self.iter.as_mut().expect("set while advancing");
            match iter.seek_ref(&ref_after_pattern) {
                Ok(true) => {}
                Ok(false) => self.done = true,
                Err(err) => self.err = Some(err.into()),
            }
            return true;
        }
        false
    }

    /// The reference of `record`, `None` if git's iterator would not show it
    /// for a name that is not a valid reference name (`REF_BAD_NAME`, shown
    /// only with `REFS_FOR_EACH_INCLUDE_BROKEN`).
    fn reference(&self, record: RefRecord) -> Option<Result<Reference, Error>> {
        // `check_refname_format(…, REFNAME_ALLOW_ONELEVEL)`, which the file
        // store's iteration checks with the same validation.
        if gix_validate::reference::name_partial(record.refname.as_bstr()).is_err() {
            if !refname_is_safe(&record.refname) {
                return Some(Err(Error::Io(std::io::Error::other(format!(
                    "refname is dangerous: {}",
                    record.refname
                )))));
            }
            return None;
        }
        let name = FullName(record.refname);
        let oid = |hash: &gix_reftable::record::Hash| gix_hash::ObjectId::from_bytes_or_panic(&hash[..self.hash_len]);
        let (target, peeled) = match record.value {
            RefValue::Val1(value) => (Target::Object(oid(&value)), None),
            RefValue::Val2 { value, target_value } => (Target::Object(oid(&value)), Some(oid(&target_value))),
            RefValue::Symref(target) => match FullName::try_from(target) {
                Ok(target) => (Target::Symbolic(target), None),
                // `refs_resolve_ref_unsafe()` fails on such a target, which
                // leaves the reference broken.
                Err(_) => return None,
            },
            RefValue::Deletion => unreachable!("deletions were skipped"),
        };
        Some(Ok(Reference { name, target, peeled }))
    }

    /// `reftable_ref_iterator_advance()` (refs/reftable-backend.c:624-724).
    fn advance(&mut self) -> Option<Result<Reference, Error>> {
        loop {
            if let Some(err) = self.err.take() {
                self.done = true;
                return Some(Err(err));
            }
            if self.done {
                return None;
            }
            let mut record = RefRecord::default();
            match self.iter.as_mut().expect("set unless failed").next_ref(&mut record) {
                Ok(true) => {}
                Ok(false) => {
                    self.done = true;
                    return None;
                }
                Err(err) => {
                    self.err = Some(err.into());
                    continue;
                }
            }

            // Like the files backend, only references under `refs/`, unless
            // root references are asked for.
            let wanted_root_ref = self.flags & RefIter::INCLUDE_ROOT_REFS != 0 && is_root_ref(&record.refname);
            if !record.refname.starts_with(b"refs/") && !wanted_root_ref {
                continue;
            }
            if !self.prefix.is_empty() && !record.refname.starts_with(&self.prefix) {
                self.done = true;
                return None;
            }
            if record.is_deletion() {
                continue;
            }
            if !self.exclude_patterns.is_empty() && self.should_exclude_current_ref(&record.refname) {
                continue;
            }
            if self.flags & RefIter::PER_WORKTREE_ONLY != 0
                && parse_worktree_ref(record.refname.as_bstr()).0 != WorktreeType::Current
            {
                continue;
            }
            if let Some(res) = self.reference(record) {
                return Some(res);
            }
        }
    }
}

/// One side of the merge, with its next reference already read.
struct Peeked {
    iter: StackIter,
    next: Option<Result<Reference, Error>>,
}

impl Peeked {
    fn new(mut iter: StackIter) -> Self {
        let next = iter.advance();
        Peeked { iter, next }
    }

    fn take(&mut self) -> Option<Result<Reference, Error>> {
        let next = self.iter.advance();
        std::mem::replace(&mut self.next, next)
    }
}

/// References in name order, as the backend's reference iteration yields them.
pub struct RefIter {
    /// The stack of the linked worktree, if the backend is for one.
    worktree: Option<Peeked>,
    /// The main stack.
    common: Peeked,
}

impl RefIter {
    /// `REFS_FOR_EACH_PER_WORKTREE_ONLY` (refs.h:438): yield only the references
    /// private to the current worktree.
    pub const PER_WORKTREE_ONLY: u32 = 1 << 1;
    /// `REFS_FOR_EACH_INCLUDE_ROOT_REFS` (refs.h:450): also yield root references
    /// like `HEAD`, which sort before `refs/`.
    pub const INCLUDE_ROOT_REFS: u32 = 1 << 3;
}

impl Iterator for RefIter {
    type Item = Result<crate::Reference, Error>;

    /// `merge_ref_iterator_advance()` (refs/iterator.c:140-206) choosing with
    /// `ref_iterator_select()` (refs/iterator.c:97-130), or just the main
    /// stack's iterator outside of a linked worktree.
    fn next(&mut self) -> Option<Self::Item> {
        let Some(worktree) = self.worktree.as_mut() else {
            return self.common.take();
        };
        loop {
            // An error of either side ends the iteration with that error.
            if let Some(Err(_)) = &worktree.next {
                return worktree.take();
            }
            if let Some(Err(_)) = &self.common.next {
                return self.common.take();
            }
            match (&worktree.next, &self.common.next) {
                (None, None) => return None,
                (Some(_), None) => return worktree.take(),
                (wt, Some(Ok(common))) => {
                    if let Some(Ok(wt)) = wt {
                        match wt.name.as_bstr().cmp(common.name.as_bstr()) {
                            Ordering::Less => return worktree.take(),
                            Ordering::Equal => {
                                // Worktree references shadow common ones of the same name.
                                self.common.take();
                                return worktree.take();
                            }
                            Ordering::Greater => {}
                        }
                    }
                    if parse_worktree_ref(common.name.as_bstr()).0 == WorktreeType::Shared {
                        return self.common.take();
                    }
                    // A per-worktree reference of the main stack is the main
                    // worktree's, not ours.
                    self.common.take();
                }
                (_, Some(Err(_))) => unreachable!("handled above"),
            }
        }
    }
}

impl Backend {
    /// `reftable_be_iterator_begin()` (refs/reftable-backend.c:846-878): the
    /// references starting with `prefix`, skipping those matching one of
    /// `exclude_patterns` where the table layout allows it. A failure to set
    /// up the iteration is its first item.
    pub fn iter_refs(&self, prefix: &BStr, exclude_patterns: &[BString], flags: u32) -> RefIter {
        let iter_for =
            |stack: Result<StackRef, Error>| Peeked::new(StackIter::new(self, stack, prefix, exclude_patterns, flags));
        RefIter {
            worktree: self.worktree_stack().map(|stack| iter_for(Ok(stack))),
            common: iter_for(self.main_stack()),
        }
    }
}
