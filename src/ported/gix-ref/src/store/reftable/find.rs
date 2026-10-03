//! Reading a single reference: `reftable_be_read_raw_ref()` and
//! `reftable_be_read_symbolic_ref()` (refs/reftable-backend.c:880-954).

use super::{Backend, Error};
use crate::{FullName, FullNameRef};

impl Backend {
    /// `reftable_be_read_raw_ref()` (refs/reftable-backend.c:880-908): the
    /// reference `name` as stored, a symbolic one unresolved; `Ok(None)` if
    /// there is none. An annotated tag's peeled value is in
    /// [`peeled`](crate::Reference::peeled).
    #[expect(dead_code, reason = "the file store's read path dispatches here once ported")]
    pub(crate) fn read_raw_ref(&self, name: &FullNameRef) -> Result<Option<crate::Reference>, Error> {
        let _ = name;
        Err(Error::unsupported("read_raw_ref"))
    }

    /// `reftable_be_read_symbolic_ref()` (refs/reftable-backend.c:910-954): the
    /// target of the symbolic reference `name`, `Ok(None)` if `name` does not
    /// exist or is not symbolic.
    #[expect(dead_code, reason = "the file store's read path dispatches here once ported")]
    pub(crate) fn read_symbolic_ref(&self, name: &FullNameRef) -> Result<Option<FullName>, Error> {
        let _ = name;
        Err(Error::unsupported("read_symbolic_ref"))
    }
}
