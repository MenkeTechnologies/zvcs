//! Reading reflogs: `reftable_be_reflog_exists()`, the reflog iterator and
//! `for_each_reflog_ent[_reverse]()` (refs/reftable-backend.c:2057-2366).
//!
//! The file store's reflog API reads the files format; the entries of a
//! reftable reflog are rendered into that format so the same parser serves
//! both (see [`ReflogSource`](crate::file::log::iter::ReflogSource)).

use super::{Backend, Error};
use crate::{FullName, FullNameRef};

impl Backend {
    /// `reftable_be_reflog_exists()` (refs/reftable-backend.c:2306-2366):
    /// whether `name` has at least one reflog entry that is not a deletion.
    pub fn reflog_exists(&self, name: &FullNameRef) -> Result<bool, Error> {
        let _ = name;
        Err(Error::unsupported("reflog_exists"))
    }

    /// `reftable_be_for_each_reflog_ent()` (refs/reftable-backend.c:2243-2304):
    /// the reflog of `name`, oldest entry first, as lines of the files format
    /// written into `buf`. `Ok(false)` if there is none.
    #[expect(dead_code, reason = "the file store's read path dispatches here once ported")]
    pub(crate) fn reflog_into(&self, name: &FullNameRef, buf: &mut Vec<u8>) -> Result<bool, Error> {
        let _ = (name, buf);
        Err(Error::unsupported("for_each_reflog_ent"))
    }

    /// `reftable_be_reflog_iterator_begin()` (refs/reftable-backend.c:2151-2165):
    /// the names of all references that have a reflog, in name order.
    pub fn reflog_names(&self) -> Result<Vec<FullName>, Error> {
        Err(Error::unsupported("reflog_iterator_begin"))
    }
}
