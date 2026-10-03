//! Iterating references: `reftable_be_iterator_begin()` and the
//! `reftable_ref_iterator` (refs/reftable-backend.c:539-878). In a linked
//! worktree the worktree's stack and the main stack are merged, the former
//! contributing its per-worktree references and the latter everything else
//! (`ref_iterator_select()`, refs/iterator.c:97-130).

use gix_object::bstr::{BStr, BString};

use super::{Backend, Error};

/// References in name order, as the backend's reference iteration yields them.
pub struct RefIter {
    refs: std::vec::IntoIter<Result<crate::Reference, Error>>,
}

impl Iterator for RefIter {
    type Item = Result<crate::Reference, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.refs.next()
    }
}

impl Backend {
    /// `reftable_be_iterator_begin()` (refs/reftable-backend.c:846-878): the
    /// references starting with `prefix`, skipping those matching one of
    /// `exclude_patterns` where the table layout allows it.
    #[expect(dead_code, reason = "the file store's read path dispatches here once ported")]
    pub(crate) fn iter_refs(&self, prefix: &BStr, exclude_patterns: &[BString]) -> Result<RefIter, Error> {
        let _ = (prefix, exclude_patterns, RefIter { refs: Vec::new().into_iter() });
        Err(Error::unsupported("iterator_begin"))
    }
}
