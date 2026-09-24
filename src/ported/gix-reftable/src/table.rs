//! `table.c`: reading a single reftable.

use std::sync::Arc;

use crate::{
    BLOCK_TYPE_INDEX, BLOCK_TYPE_LOG, BLOCK_TYPE_OBJ, BLOCK_TYPE_REF, Error, HashId, Result,
    basics::{FORMAT_ID_SHA1, FORMAT_ID_SHA256, get_be24, get_be32, get_be64},
    block::{Block, BlockIter, footer_size, header_size},
    blocksource::BlockSource,
    iter::{Empty, FilteringRefIter, IndexedTableRefIter, Iterator, RecordIter},
    record::{IndexRecord, ObjRecord, Record},
};

/// `struct reftable_table_offsets`: where a section starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableOffsets {
    /// Whether the table has this section.
    pub is_present: bool,
    /// Offset of the first block of the section.
    pub offset: u64,
    /// Offset of the section's top-level index block, or 0.
    pub index_offset: u64,
}

/// `struct reftable_table`: an open reftable file.
///
/// Shared by [`Arc`], which stands in for C's `refcount`: iterators keep the
/// table alive across a reload of the stack that opened it.
pub struct Table {
    name: String,
    source: Arc<BlockSource>,
    /// Size of the file, excluding the footer.
    pub(crate) size: u64,
    hash_id: HashId,
    block_size: u32,
    min_update_index: u64,
    max_update_index: u64,
    /// Length of the object ID keys in the `o` section.
    object_id_len: usize,
    version: u8,
    ref_offsets: TableOffsets,
    obj_offsets: TableOffsets,
    log_offsets: TableOffsets,
}

impl Table {
    /// `reftable_table_new()` (`table.c:520-596`): open the table in `source`;
    /// `name` is its file name within the stack.
    pub fn new(source: Arc<BlockSource>, name: &str) -> Result<Arc<Table>> {
        let file_size = source.size();
        // One extra byte for the type of the first block; the v2 header is the
        // larger one.
        let read_size = header_size(2) as u64 + 1;
        if read_size > file_size {
            return Err(Error::Format);
        }
        let bytes = source.bytes();
        let header = &bytes[..read_size as usize];
        if &header[..4] != b"REFT" {
            return Err(Error::Format);
        }
        let version = header[4];
        if version != 1 && version != 2 {
            return Err(Error::Format);
        }
        let size = file_size
            .checked_sub(footer_size(version) as u64)
            .ok_or(Error::Format)?;
        let footer = &bytes[size as usize..size as usize + footer_size(version)];

        let mut t = Table {
            name: name.to_owned(),
            source: Arc::clone(&source),
            size,
            hash_id: HashId::Sha1,
            block_size: 0,
            min_update_index: 0,
            max_update_index: 0,
            object_id_len: 0,
            version,
            ref_offsets: TableOffsets::default(),
            obj_offsets: TableOffsets::default(),
            log_offsets: TableOffsets::default(),
        };
        t.parse_footer(footer, header)?;
        Ok(Arc::new(t))
    }

    /// `parse_footer()` (`table.c:43-137`).
    fn parse_footer(&mut self, footer: &[u8], header: &[u8]) -> Result<()> {
        let hs = header_size(self.version);
        if &footer[..4] != b"REFT" || footer[..hs] != header[..hs] {
            return Err(Error::Format);
        }
        let mut f = 5;
        self.block_size = get_be24(&footer[f..]);
        f += 3;
        self.min_update_index = get_be64(&footer[f..]);
        f += 8;
        self.max_update_index = get_be64(&footer[f..]);
        f += 8;

        if self.version == 1 {
            self.hash_id = HashId::Sha1;
        } else {
            self.hash_id = match get_be32(&footer[f..]) {
                FORMAT_ID_SHA1 => HashId::Sha1,
                FORMAT_ID_SHA256 => HashId::Sha256,
                _ => return Err(Error::Format),
            };
            f += 4;
        }

        self.ref_offsets.index_offset = get_be64(&footer[f..]);
        f += 8;
        let obj = get_be64(&footer[f..]);
        f += 8;
        self.object_id_len = (obj & ((1 << 5) - 1)) as usize;
        self.obj_offsets.offset = obj >> 5;
        self.obj_offsets.index_offset = get_be64(&footer[f..]);
        f += 8;
        self.log_offsets.offset = get_be64(&footer[f..]);
        f += 8;
        self.log_offsets.index_offset = get_be64(&footer[f..]);
        f += 8;

        let computed_crc = gix_zlib::deflate::crc32(0, &footer[..f]);
        if computed_crc != get_be32(&footer[f..]) {
            return Err(Error::Format);
        }

        let first_block_typ = header[hs];
        self.ref_offsets.is_present = first_block_typ == BLOCK_TYPE_REF;
        self.ref_offsets.offset = 0;
        self.log_offsets.is_present = first_block_typ == BLOCK_TYPE_LOG || self.log_offsets.offset > 0;
        self.obj_offsets.is_present = self.obj_offsets.offset > 0;
        if self.obj_offsets.is_present && self.object_id_len == 0 {
            return Err(Error::Format);
        }
        Ok(())
    }

    /// `reftable_table_name()`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `reftable_table_hash_id()`.
    pub fn hash_id(&self) -> HashId {
        self.hash_id
    }

    /// `reftable_table_min_update_index()`.
    pub fn min_update_index(&self) -> u64 {
        self.min_update_index
    }

    /// `reftable_table_max_update_index()`.
    pub fn max_update_index(&self) -> u64 {
        self.max_update_index
    }

    /// The size of the table file without its footer.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// `table_offsets_for()` (`table.c:20-31`).
    fn offsets_for(&self, typ: u8) -> TableOffsets {
        match typ {
            BLOCK_TYPE_REF => self.ref_offsets,
            BLOCK_TYPE_LOG => self.log_offsets,
            _ => self.obj_offsets,
        }
    }

    /// `table_init_block()` (`table.c:166-180`): the block at `next_off`, or
    /// `Ok(None)` past the last block or when it is not of `want_typ`.
    pub(crate) fn init_block(&self, next_off: u64, want_typ: u8) -> Result<Option<Block>> {
        if next_off >= self.size {
            return Ok(None);
        }
        let header_off = if next_off != 0 { 0 } else { header_size(self.version) as u32 };
        Block::init(
            &self.source,
            next_off,
            header_off,
            self.block_size,
            self.hash_id.size(),
            want_typ,
        )
    }

    /// `table_init_iter()` (`table.c:487-506`).
    pub(crate) fn init_iter(self: &Arc<Self>, typ: u8) -> Box<dyn RecordIter> {
        if self.offsets_for(typ).is_present {
            Box::new(TableIter::new(Arc::clone(self)))
        } else {
            Box::new(Empty)
        }
    }

    /// `reftable_table_init_ref_iterator()`.
    pub fn ref_iterator(self: &Arc<Self>) -> Iterator {
        Iterator {
            inner: self.init_iter(BLOCK_TYPE_REF),
        }
    }

    /// `reftable_table_init_log_iterator()`.
    pub fn log_iterator(self: &Arc<Self>) -> Iterator {
        Iterator {
            inner: self.init_iter(BLOCK_TYPE_LOG),
        }
    }

    /// `reftable_table_refs_for()` (`table.c:716-722`): the refs pointing to
    /// `oid` (the full object ID), through the object index if there is one.
    pub fn refs_for(self: &Arc<Self>, oid: &[u8]) -> Result<Iterator> {
        if self.obj_offsets.is_present {
            self.refs_for_indexed(oid)
        } else {
            self.refs_for_unindexed(oid)
        }
    }

    /// `reftable_table_refs_for_indexed()` (`table.c:614-667`).
    fn refs_for_indexed(self: &Arc<Self>, oid: &[u8]) -> Result<Iterator> {
        let prefix_len = self.object_id_len.min(oid.len());
        let want = Record::Obj(ObjRecord {
            hash_prefix: oid[..prefix_len].to_vec(),
            offsets: Vec::new(),
        });
        let mut oit = self.init_iter(BLOCK_TYPE_OBJ);
        let mut got = Record::new(BLOCK_TYPE_OBJ)?;
        if !oit.seek(&want)? || !oit.next(&mut got)? {
            return Ok(Iterator::new(Empty));
        }
        let Record::Obj(got) = got else {
            return Err(Error::Api);
        };
        if got.hash_prefix.len() < prefix_len || got.hash_prefix[..prefix_len] != oid[..prefix_len] {
            return Ok(Iterator::new(Empty));
        }
        let hash_size = self.hash_id.size().min(oid.len());
        Ok(Iterator::new(IndexedTableRefIter::new(
            Arc::clone(self),
            &oid[..hash_size],
            got.offsets,
        )?))
    }

    /// `reftable_table_refs_for_unindexed()` (`table.c:669-714`).
    fn refs_for_unindexed(self: &Arc<Self>, oid: &[u8]) -> Result<Iterator> {
        let mut ti = TableIter::new(Arc::clone(self));
        ti.seek_start(BLOCK_TYPE_REF, false)?;
        let hash_size = self.hash_id.size().min(oid.len());
        Ok(Iterator::new(FilteringRefIter {
            oid: oid[..hash_size].to_vec(),
            it: Box::new(ti),
        }))
    }
}

/// `struct table_iter`: a cursor over the blocks of one section of a table.
pub(crate) struct TableIter {
    table: Arc<Table>,
    typ: u8,
    block_off: u64,
    block: Block,
    bi: BlockIter,
    is_finished: bool,
}

impl TableIter {
    /// `table_iter_init()` (`table.c:139-147`).
    fn new(table: Arc<Table>) -> Self {
        TableIter {
            table,
            typ: 0,
            block_off: 0,
            block: Block::default(),
            bi: BlockIter::default(),
            is_finished: false,
        }
    }

    /// `table_iter_next_in_block()` (`table.c:149-158`): ref update indices are
    /// stored relative to the table's minimum.
    fn next_in_block(&mut self, rec: &mut Record) -> Result<bool> {
        let res = self.bi.next(&self.block, rec)?;
        if res {
            if let Record::Ref(r) = rec {
                r.update_index += self.table.min_update_index;
            }
        }
        Ok(res)
    }

    /// The block following the current one, if it is of the current type.
    fn peek_next_block(&self) -> Result<Option<(u64, Block)>> {
        let next_block_off = self.block_off + u64::from(self.block.full_block_size);
        Ok(self
            .table
            .init_block(next_block_off, self.typ)?
            .map(|block| (next_block_off, block)))
    }

    /// `table_iter_next_block()` (`table.c:189-205`).
    fn next_block(&mut self) -> Result<bool> {
        match self.peek_next_block()? {
            Some((off, block)) => {
                self.block = block;
                self.block_off = off;
                self.is_finished = false;
                self.bi.seek_start(&self.block);
                Ok(true)
            }
            None => {
                self.is_finished = true;
                Ok(false)
            }
        }
    }

    /// `table_iter_seek_to()` (`table.c:240-253`); `typ` 0 accepts any block.
    fn seek_to(&mut self, off: u64, typ: u8) -> Result<bool> {
        let Some(block) = self.table.init_block(off, typ)? else {
            return Ok(false);
        };
        self.typ = block.block_type();
        self.block = block;
        self.block_off = off;
        self.bi.seek_start(&self.block);
        self.is_finished = false;
        Ok(true)
    }

    /// `table_iter_seek_start()` (`table.c:255-268`): the first block of the
    /// section of `typ`, or of its index.
    fn seek_start(&mut self, typ: u8, index: bool) -> Result<bool> {
        let offs = self.table.offsets_for(typ);
        if index {
            if offs.index_offset == 0 {
                return Ok(false);
            }
            return self.seek_to(offs.index_offset, BLOCK_TYPE_INDEX);
        }
        self.seek_to(offs.offset, typ)
    }

    /// `table_iter_seek_linear()` (`table.c:270-354`): scan blocks until the
    /// first one whose first key is past `want`; the record, if it exists, is in
    /// the block before it.
    fn seek_linear(&mut self, want: &Record) -> Result<()> {
        let mut want_key = Vec::new();
        want.key(&mut want_key);
        let mut got_key = Vec::new();

        while let Some((off, block)) = self.peek_next_block()? {
            block.first_key(&mut got_key)?;
            if got_key > want_key {
                break;
            }
            self.bi.reset();
            self.block = block;
            self.block_off = off;
            self.is_finished = false;
        }

        self.bi.seek_start(&self.block);
        self.bi.seek_key(&self.block, &want_key)
    }

    /// `table_iter_seek_indexed()` (`table.c:356-433`): search the highest index
    /// level linearly, then descend level by level.
    fn seek_indexed(&mut self, rec: &Record) -> Result<bool> {
        let mut want_key = Vec::new();
        rec.key(&mut want_key);
        let want_index = Record::Index(IndexRecord {
            offset: 0,
            last_key: want_key.clone(),
        });
        self.seek_linear(&want_index)?;

        let mut index_result = Record::Index(IndexRecord::default());
        loop {
            // An exhausted index means the key is past the last indexed one.
            if !self.next(&mut index_result)? {
                return Ok(false);
            }
            let Record::Index(idx) = &index_result else {
                return Err(Error::Api);
            };
            if !self.seek_to(idx.offset, 0)? {
                return Ok(false);
            }
            self.bi.seek_key(&self.block, &want_key)?;
            if self.typ == rec.typ() {
                return Ok(true);
            }
            if self.typ != BLOCK_TYPE_INDEX {
                return Err(Error::Format);
            }
        }
    }
}

impl RecordIter for TableIter {
    /// `table_iter_seek()` (`table.c:435-456`).
    fn seek(&mut self, want: &Record) -> Result<bool> {
        let offs = self.table.offsets_for(want.typ());
        let indexed = offs.index_offset != 0;
        // Only a negative result stops the seek here, as in C.
        self.seek_start(want.typ(), indexed)?;
        if indexed {
            self.seek_indexed(want)
        } else {
            self.seek_linear(want).map(|()| true)
        }
    }

    /// `table_iter_next()` (`table.c:207-238`).
    fn next(&mut self, rec: &mut Record) -> Result<bool> {
        if rec.typ() != self.typ {
            return Err(Error::Api);
        }
        loop {
            if self.is_finished {
                return Ok(false);
            }
            if self.next_in_block(rec)? {
                return Ok(true);
            }
            // The block is exhausted: continue with the next one, if any.
            match self.next_block() {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(err) => {
                    self.is_finished = true;
                    return Err(err);
                }
            }
        }
    }
}
