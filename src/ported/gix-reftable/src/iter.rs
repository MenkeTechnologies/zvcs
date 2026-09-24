//! `iter.c`: the generic record iterator and the iterators for `refs_for()`.

use std::sync::Arc;

use crate::{
    BLOCK_TYPE_REF, Error, Result,
    block::{Block, BlockIter},
    record::{LogRecord, Record, RefRecord, RefValue},
    table::Table,
};

/// `struct reftable_iterator_vtable`: what every iterator implements.
///
/// Both methods keep C's three-way protocol: an [`Error`] for a negative
/// return, `Ok(false)` for the positive "nothing (more) here", `Ok(true)` for 0.
pub(crate) trait RecordIter {
    /// Position the iterator so that [`next()`](Self::next) yields `want` if it exists.
    fn seek(&mut self, want: &Record) -> Result<bool>;
    /// Read the next record into `rec`.
    fn next(&mut self, rec: &mut Record) -> Result<bool>;
}

/// `empty_vtable` (`iter.c:29-54`): an iterator without entries.
pub(crate) struct Empty;

impl RecordIter for Empty {
    fn seek(&mut self, _want: &Record) -> Result<bool> {
        Ok(true)
    }
    fn next(&mut self, _rec: &mut Record) -> Result<bool> {
        Ok(false)
    }
}

/// `struct reftable_iterator`: an iterator over refs or logs.
pub struct Iterator {
    pub(crate) inner: Box<dyn RecordIter>,
}

impl Iterator {
    pub(crate) fn new(inner: impl RecordIter + 'static) -> Self {
        Iterator { inner: Box::new(inner) }
    }

    /// `reftable_iterator_seek_ref()` (`iter.c:245-255`): position at the ref
    /// `name`, or the first one after it.
    pub fn seek_ref(&mut self, name: &[u8]) -> Result<bool> {
        let want = Record::Ref(RefRecord {
            refname: name.into(),
            ..Default::default()
        });
        self.inner.seek(&want)
    }

    /// `reftable_iterator_next_ref()` (`iter.c:257-269`): `Ok(false)` at the end.
    pub fn next_ref(&mut self, r: &mut RefRecord) -> Result<bool> {
        let mut rec = Record::Ref(std::mem::take(r));
        let res = self.inner.next(&mut rec);
        match rec {
            Record::Ref(got) => *r = got,
            _ => return Err(Error::Api),
        }
        res
    }

    /// `reftable_iterator_seek_log_at()` (`iter.c:271-282`): position at the log
    /// entry of `name` at `update_index`, or the first one after it — entries of
    /// one ref are ordered newest first.
    pub fn seek_log_at(&mut self, name: &[u8], update_index: u64) -> Result<bool> {
        let want = Record::Log(LogRecord {
            refname: name.into(),
            update_index,
            ..Default::default()
        });
        self.inner.seek(&want)
    }

    /// `reftable_iterator_seek_log()` (`iter.c:284-288`): position at the
    /// newest log entry of `name`.
    pub fn seek_log(&mut self, name: &[u8]) -> Result<bool> {
        self.seek_log_at(name, u64::MAX)
    }

    /// `reftable_iterator_next_log()` (`iter.c:290-302`): `Ok(false)` at the end.
    pub fn next_log(&mut self, l: &mut LogRecord) -> Result<bool> {
        let mut rec = Record::Log(std::mem::take(l));
        let res = self.inner.next(&mut rec);
        match rec {
            Record::Log(got) => *l = got,
            _ => return Err(Error::Api),
        }
        res
    }
}

/// `struct filtering_ref_iterator` (`iter.c:56-111`): only the refs of an
/// underlying iterator that point to `oid`.
pub(crate) struct FilteringRefIter {
    pub(crate) oid: Vec<u8>,
    pub(crate) it: Box<dyn RecordIter>,
}

/// Whether `r` names `oid` as its value or peeled value.
fn ref_points_to(r: &RefRecord, oid: &[u8]) -> bool {
    let n = oid.len();
    match &r.value {
        RefValue::Val1(v) => &v[..n] == oid,
        RefValue::Val2 { value, target_value } => &target_value[..n] == oid || &value[..n] == oid,
        _ => false,
    }
}

impl RecordIter for FilteringRefIter {
    fn seek(&mut self, want: &Record) -> Result<bool> {
        self.it.seek(want)
    }

    fn next(&mut self, rec: &mut Record) -> Result<bool> {
        loop {
            if !self.it.next(rec)? {
                *rec = Record::new(BLOCK_TYPE_REF)?;
                return Ok(false);
            }
            if let Record::Ref(r) = rec {
                if ref_points_to(r, &self.oid) {
                    return Ok(true);
                }
            }
        }
    }
}

/// `struct indexed_table_ref_iter` (`iter.c:113-226`): the refs of the blocks
/// an object index record lists that point to `oid`.
pub(crate) struct IndexedTableRefIter {
    table: Arc<Table>,
    oid: Vec<u8>,
    offsets: Vec<u64>,
    /// The next offset to read.
    offset_idx: usize,
    block: Block,
    cur: BlockIter,
    is_finished: bool,
}

impl IndexedTableRefIter {
    /// `indexed_table_ref_iter_new()` (`iter.c:183-226`).
    pub(crate) fn new(table: Arc<Table>, oid: &[u8], offsets: Vec<u64>) -> Result<Self> {
        let mut it = IndexedTableRefIter {
            table,
            oid: oid.to_vec(),
            offsets,
            offset_idx: 0,
            block: Block::default(),
            cur: BlockIter::default(),
            is_finished: false,
        };
        it.next_block()?;
        Ok(it)
    }

    /// `indexed_table_ref_iter_next_block()` (`iter.c:122-144`).
    fn next_block(&mut self) -> Result<()> {
        if self.offset_idx == self.offsets.len() {
            self.is_finished = true;
            return Ok(());
        }
        let off = self.offsets[self.offset_idx];
        self.offset_idx += 1;
        // An indexed block that does not exist is corruption.
        self.block = self.table.init_block(off, BLOCK_TYPE_REF)?.ok_or(Error::Format)?;
        self.cur.seek_start(&self.block);
        Ok(())
    }
}

impl RecordIter for IndexedTableRefIter {
    fn seek(&mut self, _want: &Record) -> Result<bool> {
        Err(Error::Api)
    }

    fn next(&mut self, rec: &mut Record) -> Result<bool> {
        loop {
            if self.is_finished {
                return Ok(false);
            }
            if !self.cur.next(&self.block, rec)? {
                self.next_block()?;
                continue;
            }
            if let Record::Ref(r) = rec {
                if ref_points_to(r, &self.oid) {
                    return Ok(true);
                }
            }
        }
    }
}
