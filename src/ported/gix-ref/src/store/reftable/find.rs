//! Reading a single reference: `reftable_be_read_raw_ref()` and
//! `reftable_be_read_symbolic_ref()` (refs/reftable-backend.c:880-933).

use gix_hash::ObjectId;
use gix_reftable::{RefRecord, RefValue};

use super::{Backend, Error, lock};
use crate::{FullName, FullNameRef, Target};

impl Backend {
    /// `oidread()` of one of a record's hashes, which hold `HASH_SIZE_MAX` bytes.
    pub(crate) fn oid_from_hash(&self, hash: &gix_reftable::record::Hash) -> ObjectId {
        ObjectId::from_bytes_or_panic(&hash[..self.stack_options().hash_id.size()])
    }

    /// `reftable_backend_read_ref()` (refs/reftable-backend.c:64-118): seek
    /// `refname` in `stack` and return the value of its record, `None` if the
    /// first record at or after it has another name or is a deletion. Since
    /// 2.56 the tombstone is checked here (:87-88); before, a deletion record
    /// was reported as a bug.
    ///
    /// The target of a symbolic reference is the referent as stored, not
    /// validated: whoever follows it decides what an invalid one means.
    fn read_ref(&self, stack: &gix_reftable::Stack, refname: &[u8]) -> Result<Option<Target>, Error> {
        let mut it = stack.ref_iterator()?;
        if !it.seek_ref(refname)? {
            return Ok(None);
        }
        let mut record = RefRecord::default();
        if !it.next_ref(&mut record)? || record.refname != refname || record.is_deletion() {
            return Ok(None);
        }
        Ok(Some(match record.value {
            RefValue::Symref(target) => Target::Symbolic(FullName(target)),
            // `reftable_ref_record_val1()` is the first hash of either kind;
            // the peeled value of a `VAL2` is not reported by this function.
            RefValue::Val1(hash) | RefValue::Val2 { value: hash, .. } => Target::Object(self.oid_from_hash(&hash)),
            RefValue::Deletion => unreachable!("deletions were filtered above"),
        }))
    }

    /// `reftable_be_read_raw_ref()` (refs/reftable-backend.c:880-908): the
    /// value of the reference `name` as stored, a symbolic one unresolved and
    /// its target not validated; `Ok(None)` if there is none.
    ///
    /// `FETCH_HEAD` and `MERGE_HEAD` never get here: they stay files for every
    /// backend (`refs_read_raw_ref()`, refs.c:2095-2105).
    pub fn read_raw_ref(&self, name: &FullNameRef) -> Result<Option<Target>, Error> {
        self.check()?;
        let (stack, refname) = self.backend_for(name.as_bstr(), true)?;
        self.read_ref(&lock(&stack), refname)
    }

    /// `reftable_be_read_symbolic_ref()` (refs/reftable-backend.c:910-933):
    /// `Ok(None)` if `name` does not exist (git's `-1`), the object it points
    /// to if it is not symbolic (git's `NOT_A_SYMREF`), and the reference it
    /// points to otherwise.
    ///
    /// Unlike [`read_raw_ref()`](Self::read_raw_ref), git does not consult the
    /// error of the last stack initialization here. A target that is not a
    /// valid reference name fails with an [`Error::Io`] of kind `InvalidData`.
    pub fn read_symbolic_ref(&self, name: &FullNameRef) -> Result<Option<Target>, Error> {
        let (stack, refname) = self.backend_for(name.as_bstr(), true)?;
        let target = self.read_ref(&lock(&stack), refname)?;
        if let Some(Target::Symbolic(target)) = &target {
            if let Err(err) = gix_validate::reference::name(target.as_bstr()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "symbolic reference {} points to invalid name {target:?}: {err}",
                        name.as_bstr()
                    ),
                )
                .into());
            }
        }
        Ok(target)
    }
}
