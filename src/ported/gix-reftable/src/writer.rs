//! `writer.c` (and `tree.c`): writing a single reftable.
//!
//! A table is written in sections: ref blocks (padded to the block size),
//! an optional object index mapping object IDs to ref blocks, then log blocks.
//! Every section with more than a few blocks gets a multi-level index. The
//! footer repeats the header and records where each section starts.

use std::collections::BTreeMap;

use crate::{
    BLOCK_TYPE_INDEX, BLOCK_TYPE_LOG, BLOCK_TYPE_OBJ, BLOCK_TYPE_REF, DEFAULT_BLOCK_SIZE, Error, HashId, Result,
    block::{BlockWriter, footer_size, header_size},
    record::{IndexRecord, LogRecord, LogValue, ObjRecord, Record, RefRecord},
};

/// `struct reftable_write_options` (`reftable-writer.h:18-72`).
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Do not pad blocks to the block size.
    pub unpadded: bool,
    /// The block size, less than 2^24. `0` means [`DEFAULT_BLOCK_SIZE`].
    pub block_size: u32,
    /// Do not write an object-to-ref index.
    pub skip_index_objects: bool,
    /// How often to write complete keys. `0` means 16.
    pub restart_interval: u16,
    /// The hash of the object IDs in the table.
    pub hash_id: HashId,
    /// Mode for new files; `None` uses 0666 minus the umask.
    pub default_permissions: Option<u32>,
    /// Copy log messages exactly instead of requiring one line and appending `\n`.
    pub exact_log_message: bool,
    /// Prevent auto-compaction of the stack after an addition.
    pub disable_auto_compact: bool,
    /// The geometric factor auto-compaction maintains; `0` means 2.
    pub auto_compaction_factor: u8,
    /// Milliseconds to wait for the `tables.list` lock: `0` fails at once,
    /// a negative value waits indefinitely.
    pub lock_timeout_ms: i64,
    /// Whether to `fsync()` written tables and `tables.list`, git's
    /// `fsync_component(FSYNC_COMPONENT_REFERENCE, fd)` (`system.c`), which
    /// `core.fsync` controls and leaves off by default.
    pub fsync: bool,
}

impl WriteOptions {
    /// `options_set_defaults()` (`writer.c:77-89`).
    fn with_defaults(mut self) -> Self {
        if self.restart_interval == 0 {
            self.restart_interval = 16;
        }
        if self.block_size == 0 {
            self.block_size = DEFAULT_BLOCK_SIZE;
        }
        self
    }
}

/// `struct reftable_block_stats`: statistics for one block type.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockStats {
    /// Total number of entries written.
    pub entries: usize,
    /// Total number of key restarts.
    pub restarts: usize,
    /// Total number of blocks.
    pub blocks: usize,
    /// Total number of index blocks.
    pub index_blocks: usize,
    /// Depth of the index.
    pub max_index_level: usize,
    /// Offset of the first block of this type.
    pub offset: u64,
    /// Offset of the top-level index block, or 0.
    pub index_offset: u64,
}

/// `struct reftable_stats`: statistics for a written table.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Total number of blocks written.
    pub blocks: usize,
    /// Ref blocks.
    pub ref_stats: BlockStats,
    /// Object index blocks.
    pub obj_stats: BlockStats,
    /// Index blocks.
    pub idx_stats: BlockStats,
    /// Log blocks.
    pub log_stats: BlockStats,
    /// Disambiguation length of shortened object IDs.
    pub object_id_len: usize,
}

/// Where a [`Writer`] sends its bytes: C's `write` and `flush` callbacks.
pub trait Sink {
    /// Write all of `data`.
    fn write(&mut self, data: &[u8]) -> Result<()>;
    /// Called once before the footer is written; the stack `fsync()`s here.
    fn flush(&mut self) -> Result<()>;
}

impl Sink for Vec<u8> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.extend_from_slice(data);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// `struct reftable_writer`: writes one table to a [`Sink`].
pub struct Writer<S: Sink> {
    sink: S,
    pending_padding: usize,
    last_key: Vec<u8>,
    scratch: Vec<u8>,
    /// Offset of the next block to write.
    next: u64,
    min_update_index: u64,
    max_update_index: u64,
    opts: WriteOptions,
    /// The writer for the current section, if one is open.
    block_writer: Option<BlockWriter>,
    /// Pending index records for the current section.
    index: Vec<IndexRecord>,
    /// Object ID → offsets of the ref blocks naming it, for the `o` section.
    /// C keeps an unbalanced binary tree (`tree.c`) and walks it in order; a
    /// sorted map yields the same order.
    obj_index: BTreeMap<Vec<u8>, Vec<u64>>,
    stats: Stats,
}

impl<S: Sink> Writer<S> {
    /// `reftable_writer_new()` (`writer.c:147-182`).
    pub fn new(sink: S, opts: &WriteOptions) -> Result<Self> {
        let opts = opts.clone().with_defaults();
        if opts.block_size >= (1 << 24) {
            return Err(Error::Api);
        }
        let mut w = Writer {
            sink,
            pending_padding: 0,
            last_key: Vec::new(),
            scratch: Vec::new(),
            next: 0,
            min_update_index: 0,
            max_update_index: 0,
            opts,
            block_writer: None,
            index: Vec::new(),
            obj_index: BTreeMap::new(),
            stats: Stats::default(),
        };
        w.reinit_block_writer(BLOCK_TYPE_REF);
        Ok(w)
    }

    /// `reftable_writer_set_limits()` (`writer.c:184-202`): the update index
    /// range of the records to come. Must be called before adding any.
    pub fn set_limits(&mut self, min: u64, max: u64) -> Result<()> {
        if self.next != 0 || !self.last_key.is_empty() {
            return Err(Error::Api);
        }
        self.min_update_index = min;
        self.max_update_index = max;
        Ok(())
    }

    /// The lower update index limit.
    pub fn min_update_index(&self) -> u64 {
        self.min_update_index
    }

    /// The upper update index limit.
    pub fn max_update_index(&self) -> u64 {
        self.max_update_index
    }

    /// `reftable_writer_stats()`.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The sink, to finish writing to it after [`close()`](Self::close).
    pub fn into_sink(self) -> S {
        self.sink
    }

    /// `writer_version()` (`writer.c:91-96`).
    fn version(&self) -> u8 {
        if self.opts.hash_id == HashId::Sha1 { 1 } else { 2 }
    }

    /// `writer_write_header()` (`writer.c:98-125`): write the header into
    /// `dest`, returning its length.
    fn write_header(&self, dest: &mut [u8]) -> usize {
        dest[..4].copy_from_slice(b"REFT");
        dest[4] = self.version();
        crate::basics::put_be24(&mut dest[5..], self.opts.block_size);
        dest[8..16].copy_from_slice(&self.min_update_index.to_be_bytes());
        dest[16..24].copy_from_slice(&self.max_update_index.to_be_bytes());
        if self.version() == 2 {
            dest[24..28].copy_from_slice(&self.opts.hash_id.format_id().to_be_bytes());
        }
        header_size(self.version())
    }

    /// `writer_reinit_block_writer()` (`writer.c:127-145`).
    fn reinit_block_writer(&mut self, typ: u8) {
        let block_start = if self.next == 0 { header_size(self.version()) as u32 } else { 0 };
        self.last_key.clear();
        let mut bw = BlockWriter::new(typ, self.opts.block_size, block_start, self.opts.hash_id.size());
        bw.restart_interval = self.opts.restart_interval;
        self.block_writer = Some(bw);
    }

    fn block_stats(&mut self, typ: u8) -> &mut BlockStats {
        match typ {
            BLOCK_TYPE_REF => &mut self.stats.ref_stats,
            BLOCK_TYPE_OBJ => &mut self.stats.obj_stats,
            BLOCK_TYPE_INDEX => &mut self.stats.idx_stats,
            _ => &mut self.stats.log_stats,
        }
    }

    /// `padded_write()` (`writer.c:47-75`): write `data`, first emitting the
    /// padding queued by the previous write, and queue `padding` zeroes.
    fn padded_write(&mut self, data: &[u8], padding: usize) -> Result<()> {
        if self.pending_padding > 0 {
            let zeroed = vec![0u8; self.pending_padding];
            self.sink.write(&zeroed)?;
            self.pending_padding = 0;
        }
        self.pending_padding = padding;
        self.sink.write(data)
    }

    /// `writer_index_hash()` (`writer.c:241-281`): note that the ref block
    /// about to be written mentions `hash`.
    fn index_hash(&mut self, hash: &[u8]) {
        let off = self.next;
        let offsets = self.obj_index.entry(hash.to_vec()).or_default();
        if offsets.last() == Some(&off) {
            return;
        }
        offsets.push(off);
    }

    /// `writer_add_record()` (`writer.c:283-343`).
    fn add_record(&mut self, rec: &Record) -> Result<()> {
        rec.key(&mut self.scratch);
        if self.last_key >= self.scratch {
            return Err(Error::Api);
        }
        self.last_key.clear();
        self.last_key.extend_from_slice(&self.scratch);

        if self.block_writer.is_none() {
            self.reinit_block_writer(rec.typ());
        }
        let bw = self.block_writer.as_mut().expect("just set");
        if bw.typ() != rec.typ() {
            return Err(Error::Api);
        }

        // If the record does not fit, flush the block and retry in a fresh one;
        // failing again means it does not fit into the block size at all.
        match bw.add(rec) {
            Err(Error::EntryTooBig) => {}
            other => return other,
        }
        self.flush_block()?;
        self.reinit_block_writer(rec.typ());
        self.block_writer.as_mut().expect("just set").add(rec)
    }

    /// `reftable_writer_add_ref()` (`writer.c:345-395`). Records must be added
    /// in name order and within the limits of [`set_limits()`](Self::set_limits).
    pub fn add_ref(&mut self, r: &RefRecord) -> Result<()> {
        if r.update_index < self.min_update_index || r.update_index > self.max_update_index {
            return Err(Error::Api);
        }
        let mut stored = r.clone();
        stored.update_index -= self.min_update_index;
        self.add_record(&Record::Ref(stored))?;

        let hash_size = self.opts.hash_id.size();
        if !self.opts.skip_index_objects {
            if let Some(h) = r.val1() {
                self.index_hash(&h[..hash_size]);
            }
            if let Some(h) = r.val2() {
                self.index_hash(&h[..hash_size]);
            }
        }
        Ok(())
    }

    /// `reftable_writer_add_refs()` (`writer.c:397-409`): sort by name, then add.
    pub fn add_refs(&mut self, refs: &mut [RefRecord]) -> Result<()> {
        refs.sort_by(|a, b| a.refname.cmp(&b.refname));
        refs.iter().try_for_each(|r| self.add_ref(r))
    }

    /// `reftable_writer_add_log_verbatim()` (`writer.c:411-430`).
    fn add_log_verbatim(&mut self, log: &LogRecord) -> Result<()> {
        if self.block_writer.as_ref().is_some_and(|bw| bw.typ() == BLOCK_TYPE_REF) {
            self.finish_public_section()?;
        }
        // Log blocks are not padded: drop the padding queued after the last
        // ref block.
        self.next -= self.pending_padding as u64;
        self.pending_padding = 0;
        self.add_record(&Record::Log(log.clone()))
    }

    /// `reftable_writer_add_log()` (`writer.c:432-488`). Unless
    /// [`WriteOptions::exact_log_message`] is set, the message must be a single
    /// line; trailing newlines are normalized to exactly one.
    pub fn add_log(&mut self, log: &LogRecord) -> Result<()> {
        let LogValue::Update(update) = &log.value else {
            return self.add_log_verbatim(log);
        };

        // Only the upper limit is checked: an entry can be replaced by writing a
        // new one with the same (older) update index.
        if log.update_index > self.max_update_index {
            return Err(Error::Api);
        }

        if self.opts.exact_log_message {
            return self.add_log_verbatim(log);
        }
        let mut msg = update.message.to_vec();
        while msg.last() == Some(&b'\n') {
            msg.pop();
        }
        // `strchr()` stops at a NUL, so a newline after one goes unnoticed.
        let visible = msg.iter().position(|&b| b == 0).map_or(&msg[..], |n| &msg[..n]);
        if visible.contains(&b'\n') {
            return Err(Error::Api);
        }
        msg.push(b'\n');

        let mut cleaned = log.clone();
        if let LogValue::Update(u) = &mut cleaned.value {
            u.message = msg.into();
        }
        self.add_log_verbatim(&cleaned)
    }

    /// `reftable_writer_add_logs()` (`writer.c:490-502`): sort by key, then add.
    pub fn add_logs(&mut self, logs: &mut [LogRecord]) -> Result<()> {
        logs.sort_by(LogRecord::compare_key);
        logs.iter().try_for_each(|l| self.add_log(l))
    }

    /// `writer_finish_section()` (`writer.c:504-595`): flush the last block and
    /// write as many index levels as the section needs.
    fn finish_section(&mut self) -> Result<()> {
        let typ = self.block_writer.as_ref().expect("section open").typ();
        let mut index_start = 0;
        let mut max_level = 0;
        let threshold = if self.opts.unpadded { 1 } else { 3 };
        let before_blocks = self.stats.idx_stats.blocks;

        self.flush_block()?;

        // Each level indexes the blocks of the one below; the highest level is
        // written last, so readers start at the end of the index section.
        while self.index.len() > threshold {
            max_level += 1;
            index_start = self.next;
            self.reinit_block_writer(BLOCK_TYPE_INDEX);

            let idx = std::mem::take(&mut self.index);
            for rec in idx {
                self.add_record(&Record::Index(rec))?;
            }
            self.flush_block()?;
        }

        // Fewer index records than the threshold remain; they must not leak into
        // the next section.
        self.index.clear();

        let idx_blocks = self.stats.idx_stats.blocks - before_blocks;
        let bstats = self.block_stats(typ);
        bstats.index_blocks = idx_blocks;
        bstats.index_offset = index_start;
        bstats.max_index_level = max_level;

        // The next section can start with any key.
        self.last_key.clear();
        Ok(())
    }

    /// `writer_dump_object_index()` (`writer.c:682-704`) with
    /// `update_common()` and `write_object_record()`.
    fn dump_object_index(&mut self) -> Result<()> {
        // The shortest prefix that tells all object IDs apart, at least 2.
        let mut max = 1;
        let mut last: Option<&Vec<u8>> = None;
        for hash in self.obj_index.keys() {
            if let Some(last) = last {
                max = max.max(crate::basics::common_prefix_size(hash, last));
            }
            last = Some(hash);
        }
        self.stats.object_id_len = max + 1;
        let object_id_len = self.stats.object_id_len;

        self.reinit_block_writer(BLOCK_TYPE_OBJ);
        let entries = std::mem::take(&mut self.obj_index);
        for (hash, offsets) in entries {
            let mut rec = ObjRecord {
                hash_prefix: hash[..object_id_len.min(hash.len())].to_vec(),
                offsets,
            };
            let bw = self.block_writer.as_mut().expect("section open");
            match bw.add(&Record::Obj(rec.clone())) {
                Err(Error::EntryTooBig) => {}
                other => {
                    other?;
                    continue;
                }
            }
            self.flush_block()?;
            self.reinit_block_writer(BLOCK_TYPE_OBJ);
            let bw = self.block_writer.as_mut().expect("just set");
            match bw.add(&Record::Obj(rec.clone())) {
                Err(Error::EntryTooBig) => {}
                other => {
                    other?;
                    continue;
                }
            }
            // Too many offsets for a fresh block: drop them, which readers
            // treat as "scan the ref section".
            rec.offsets.clear();
            bw.add(&Record::Obj(rec))?;
        }
        self.finish_section()
    }

    /// `writer_finish_public_section()` (`writer.c:706-733`).
    fn finish_public_section(&mut self) -> Result<()> {
        let Some(bw) = &self.block_writer else {
            return Ok(());
        };
        let typ = bw.typ();
        self.finish_section()?;
        if typ == BLOCK_TYPE_REF && !self.opts.skip_index_objects && self.stats.ref_stats.index_blocks > 0 {
            self.dump_object_index()?;
        }
        self.obj_index.clear();
        self.block_writer = None;
        Ok(())
    }

    /// `reftable_writer_close()` (`writer.c:735-787`): finish the last section
    /// and write the footer. An empty table is still written, header and
    /// footer, and reported as [`Error::EmptyTable`].
    pub fn close(&mut self) -> Result<()> {
        self.finish_public_section()?;
        let empty_table = self.next == 0;
        self.pending_padding = 0;
        if empty_table {
            let mut header = [0u8; 28];
            let n = self.write_header(&mut header);
            self.padded_write(&header[..n], 0)?;
        }

        let mut footer = [0u8; 72];
        let mut p = self.write_header(&mut footer);
        let s = &self.stats;
        for v in [
            s.ref_stats.index_offset,
            (s.obj_stats.offset << 5) | s.object_id_len as u64,
            s.obj_stats.index_offset,
            s.log_stats.offset,
            s.log_stats.index_offset,
        ] {
            footer[p..p + 8].copy_from_slice(&v.to_be_bytes());
            p += 8;
        }
        let crc = gix_zlib::deflate::crc32(0, &footer[..p]);
        footer[p..p + 4].copy_from_slice(&crc.to_be_bytes());

        self.sink.flush().map_err(|_| Error::Io)?;
        let n = footer_size(self.version());
        self.padded_write(&footer[..n], 0)?;

        if empty_table {
            return Err(Error::EmptyTable);
        }
        Ok(())
    }

    /// `writer_flush_nonempty_block()` (`writer.c:798-873`).
    fn flush_nonempty_block(&mut self) -> Result<()> {
        let mut bw = self.block_writer.take().expect("block open");
        let typ = bw.typ();

        // Finish the block in memory: restart points, and compression for logs.
        let raw_bytes = bw.finish()?;

        // All but log blocks are padded to the block size.
        let padding = if !self.opts.unpadded && typ != BLOCK_TYPE_LOG {
            (self.opts.block_size as usize).saturating_sub(raw_bytes)
        } else {
            0
        };

        let next = self.next;
        let entries = bw.entries;
        let restarts = bw.restart_len();
        let bstats = self.block_stats(typ);
        let block_typ_off = if bstats.blocks == 0 { next } else { 0 };
        if block_typ_off > 0 {
            bstats.offset = block_typ_off;
        }
        bstats.entries += entries;
        bstats.restarts += restarts;
        bstats.blocks += 1;
        self.stats.blocks += 1;

        // The first block of the table carries the file header.
        if self.next == 0 {
            let n = header_size(self.version());
            let mut header = [0u8; 28];
            self.write_header(&mut header);
            bw.block[..n].copy_from_slice(&header[..n]);
        }

        self.padded_write(&bw.block[..raw_bytes], padding)?;

        // One index record per block: its last key and its offset. More than a
        // threshold of these makes `finish_section()` write an index.
        self.index.push(IndexRecord {
            offset: self.next,
            last_key: std::mem::take(&mut bw.last_key),
        });
        self.next += (padding + raw_bytes) as u64;
        Ok(())
    }

    /// `writer_flush_block()` (`writer.c:875-882`).
    fn flush_block(&mut self) -> Result<()> {
        match &self.block_writer {
            Some(bw) if bw.entries > 0 => self.flush_nonempty_block(),
            _ => Ok(()),
        }
    }
}
