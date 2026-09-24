//! `merged.c`: a unified view of a sequence of tables, later ones shadowing
//! earlier ones.

use std::sync::Arc;

use crate::{
    BLOCK_TYPE_LOG, BLOCK_TYPE_REF, Error, HashId, Result,
    iter::{Iterator, RecordIter},
    pq::MergedIterPqueue,
    record::Record,
    table::Table,
};

/// `struct reftable_merged_table`.
#[derive(Clone)]
pub struct MergedTable {
    pub(crate) tables: Vec<Arc<Table>>,
    hash_id: HashId,
    /// If set, hide deletions; the full stack does, compaction does not.
    pub(crate) suppress_deletions: bool,
    min: u64,
    max: u64,
}

impl MergedTable {
    /// `reftable_merged_table_new()` (`merged.c:194-228`): all tables must use
    /// `hash_id`.
    pub fn new(tables: Vec<Arc<Table>>, hash_id: HashId) -> Result<Self> {
        let mut first_min = 0;
        let mut last_max = 0;
        for (i, t) in tables.iter().enumerate() {
            if t.hash_id() != hash_id {
                return Err(Error::Format);
            }
            if i == 0 || t.min_update_index() < first_min {
                first_min = t.min_update_index();
            }
            if i == 0 || t.max_update_index() > last_max {
                last_max = t.max_update_index();
            }
        }
        Ok(MergedTable {
            tables,
            hash_id,
            suppress_deletions: false,
            min: first_min,
            max: last_max,
        })
    }

    /// `reftable_merged_table_max_update_index()`.
    pub fn max_update_index(&self) -> u64 {
        self.max
    }

    /// `reftable_merged_table_min_update_index()`.
    pub fn min_update_index(&self) -> u64 {
        self.min
    }

    /// `reftable_merged_table_hash_id()`.
    pub fn hash_id(&self) -> HashId {
        self.hash_id
    }

    /// The tables, oldest first.
    pub fn tables(&self) -> &[Arc<Table>] {
        &self.tables
    }

    /// `merged_table_init_iter()` (`merged.c:249-299`).
    pub(crate) fn init_iter(&self, typ: u8) -> Result<Iterator> {
        let mut subiters = Vec::with_capacity(self.tables.len());
        let mut recs = Vec::with_capacity(self.tables.len());
        for t in &self.tables {
            recs.push(Record::new(typ)?);
            subiters.push(t.init_iter(typ));
        }
        Ok(Iterator::new(MergedIter {
            subiters,
            recs,
            pq: MergedIterPqueue::default(),
            suppress_deletions: self.suppress_deletions,
            advance_index: None,
        }))
    }

    /// `reftable_merged_table_init_ref_iterator()`.
    pub fn ref_iterator(&self) -> Result<Iterator> {
        self.init_iter(BLOCK_TYPE_REF)
    }

    /// `reftable_merged_table_init_log_iterator()`.
    pub fn log_iterator(&self) -> Result<Iterator> {
        self.init_iter(BLOCK_TYPE_LOG)
    }
}

/// `struct merged_iter`. `subiters[i]` and `recs[i]` together are C's
/// `struct merged_subiter`.
struct MergedIter {
    subiters: Vec<Box<dyn RecordIter>>,
    recs: Vec<Record>,
    pq: MergedIterPqueue,
    suppress_deletions: bool,
    /// The sub-iterator whose record was yielded last and must be advanced
    /// before the next entry is chosen.
    advance_index: Option<usize>,
}

impl MergedIter {
    /// `merged_iter_advance_subiter()` (`merged.c:45-62`): `Ok(false)` if the
    /// sub-iterator is exhausted.
    fn advance_subiter(&mut self, idx: usize) -> Result<bool> {
        if !self.subiters[idx].next(&mut self.recs[idx])? {
            return Ok(false);
        }
        self.pq.add(idx, &self.recs)?;
        Ok(true)
    }

    /// `merged_iter_next_entry()` (`merged.c:90-160`).
    fn next_entry(&mut self, rec: &mut Record) -> Result<bool> {
        let mut empty = self.pq.is_empty();

        if let Some(idx) = self.advance_index {
            // With no queued entries only one sub-iterator is left, which
            // yields its records in order already. This is common: most
            // repositories have one large base table holding most refs.
            if empty {
                return self.subiters[idx].next(rec);
            }
            if self.advance_subiter(idx)? {
                empty = false;
            }
            self.advance_index = None;
        }

        if empty {
            return Ok(false);
        }

        let entry = self.pq.remove(&self.recs)?;

        // Drop the entries of older tables with the same key.
        while !self.pq.is_empty() {
            let top = self.pq.top();
            if self.recs[top].cmp_key(&self.recs[entry])? == std::cmp::Ordering::Greater {
                break;
            }
            self.pq.remove(&self.recs)?;
            self.advance_subiter(top)?;
        }

        self.advance_index = Some(entry);
        std::mem::swap(rec, &mut self.recs[entry]);
        Ok(true)
    }
}

impl RecordIter for MergedIter {
    /// `merged_iter_seek()` (`merged.c:64-88`).
    fn seek(&mut self, want: &Record) -> Result<bool> {
        self.advance_index = None;
        self.pq.clear();
        for i in 0..self.subiters.len() {
            if !self.subiters[i].seek(want)? {
                continue;
            }
            self.advance_subiter(i)?;
        }
        Ok(true)
    }

    /// `merged_iter_next_void()` (`merged.c:167-179`).
    fn next(&mut self, rec: &mut Record) -> Result<bool> {
        loop {
            if !self.next_entry(rec)? {
                return Ok(false);
            }
            if self.suppress_deletions && rec.is_deletion() {
                continue;
            }
            return Ok(true);
        }
    }
}
