//! `block.c`: writing and reading single blocks of records.
//!
//! A block is `type (1) | size (3, be24) | records… | restart offsets (3 each) |
//! restart count (2)`, preceded by the file header in the first block of a
//! table. Log blocks are zlib-compressed after their four-byte block header.

use std::sync::Arc;

use crate::{
    BLOCK_TYPE_ANY, BLOCK_TYPE_LOG, DEFAULT_BLOCK_SIZE, Error, MAX_RESTARTS, Result,
    basics::{binsearch, get_be16, get_be24, put_be24},
    blocksource::{BlockData, BlockSource},
    record::{Record, decode_key, decode_keylen, encode_key, is_block_type},
};

/// `header_size()` (`block.c:18-27`): the file header length per format version.
pub fn header_size(version: u8) -> usize {
    match version {
        1 => 24,
        _ => 28,
    }
}

/// `footer_size()` (`block.c:29-38`): the file footer length per format version.
pub fn footer_size(version: u8) -> usize {
    match version {
        1 => 68,
        _ => 72,
    }
}

/// `struct block_writer` (`block.h`): accumulates records into one block.
pub(crate) struct BlockWriter {
    /// The block being written, `block_size` bytes; the writer owns it where C
    /// borrows the table writer's buffer.
    pub(crate) block: Vec<u8>,
    block_size: u32,
    /// Offset of the file header; nonzero only in the first block.
    header_off: u32,
    pub(crate) restart_interval: u16,
    hash_size: usize,
    /// Offset of the next byte to write.
    next: u32,
    restarts: Vec<u32>,
    pub(crate) last_key: Vec<u8>,
    scratch: Vec<u8>,
    pub(crate) entries: usize,
}

impl BlockWriter {
    /// `block_writer_init()` (`block.c:73-94`): start a block of type `typ`.
    pub(crate) fn new(typ: u8, block_size: u32, header_off: u32, hash_size: usize) -> Self {
        let mut block = vec![0u8; block_size as usize];
        block[header_off as usize] = typ;
        BlockWriter {
            block,
            block_size,
            header_off,
            restart_interval: 16,
            hash_size,
            next: header_off + 4,
            restarts: Vec::new(),
            last_key: Vec::new(),
            scratch: Vec::new(),
            entries: 0,
        }
    }

    /// `block_writer_type()`.
    pub(crate) fn typ(&self) -> u8 {
        self.block[self.header_off as usize]
    }

    /// The number of restart points so far.
    pub(crate) fn restart_len(&self) -> usize {
        self.restarts.len()
    }

    /// `block_writer_register_restart()` (`block.c:40-71`).
    fn register_restart(&mut self, n: u32, mut is_restart: bool) -> Result<()> {
        let mut rlen = self.restarts.len() as u32;
        if rlen >= MAX_RESTARTS {
            is_restart = false;
        }
        if is_restart {
            rlen += 1;
        }
        if 2 + 3 * rlen + n > self.block_size - self.next {
            return Err(Error::EntryTooBig);
        }
        if is_restart {
            self.restarts.push(self.next);
        }
        self.next += n;
        self.last_key.clear();
        self.last_key.extend_from_slice(&self.scratch);
        self.entries += 1;
        Ok(())
    }

    /// `block_writer_add()` (`block.c:105-147`): append `rec`, or fail with
    /// [`Error::EntryTooBig`] if the block is full.
    pub(crate) fn add(&mut self, rec: &Record) -> Result<()> {
        rec.key(&mut self.scratch);
        if self.scratch.is_empty() {
            return Err(Error::Api);
        }

        let last: &[u8] = if self.entries % usize::from(self.restart_interval) == 0 {
            &[]
        } else {
            &self.last_key
        };
        let out = &mut self.block[self.next as usize..self.block_size as usize];
        let (n, is_restart) = encode_key(out, last, &self.scratch, rec.val_type())?;
        let m = rec.encode(&mut out[n..], self.hash_size)?;
        self.register_restart((n + m) as u32, is_restart)
    }

    /// `block_writer_finish()` (`block.c:149-212`): append the restart points,
    /// compress log blocks, and return the number of bytes of the block to write.
    pub(crate) fn finish(&mut self) -> Result<usize> {
        for i in 0..self.restarts.len() {
            let at = self.next as usize;
            put_be24(&mut self.block[at..], self.restarts[i]);
            self.next += 3;
        }
        let at = self.next as usize;
        self.block[at..at + 2].copy_from_slice(&(self.restarts.len() as u16).to_be_bytes());
        self.next += 2;
        let h = self.header_off as usize;
        put_be24(&mut self.block[1 + h..], self.next);

        // Log records are stored zlib-compressed, at the level git's
        // `deflateInit(zstream, 9)` uses. The compression also spans the restart
        // points just written.
        if self.typ() == BLOCK_TYPE_LOG {
            let skip = 4 + h;
            let compressed =
                gix_zlib::deflate::compress(&self.block[skip..self.next as usize], 9, gix_zlib::deflate::Wrap::Zlib);
            let end = skip + compressed.len();
            if end > self.block.len() {
                self.block.resize(end, 0);
            }
            self.block[skip..end].copy_from_slice(&compressed);
            self.next = end as u32;
        }
        Ok(self.next as usize)
    }
}

/// `struct reftable_block`: a block read from a table.
#[derive(Clone, Default)]
pub struct Block {
    /// Offset of the block header; nonzero for the first block of a table.
    pub(crate) header_off: u32,
    data: BlockData,
    pub(crate) hash_size: usize,
    restart_count: u16,
    pub(crate) restart_off: u32,
    /// The size the block occupies in the file; for log blocks the compressed size.
    pub(crate) full_block_size: u32,
    pub(crate) block_type: u8,
}

/// `read_block()` (`block.c:214-225`): `sz` bytes at `off`, clamped to the source.
fn read_block(source: &Arc<BlockSource>, off: u64, mut sz: u32) -> BlockData {
    let size = source.size();
    if off >= size {
        return BlockData::default();
    }
    if off + u64::from(sz) > size {
        sz = (size - off) as u32;
    }
    BlockData::read(source, off, sz)
}

impl Block {
    /// `reftable_block_init()` (`block.c:227-350`): read the block at `offset`.
    ///
    /// Returns `Ok(None)` where C returns `1`: the block is not of `want_type`.
    pub fn init(
        source: &Arc<BlockSource>,
        offset: u64,
        header_size: u32,
        table_block_size: u32,
        hash_size: usize,
        want_type: u8,
    ) -> Result<Option<Block>> {
        let guess_block_size = if table_block_size != 0 {
            table_block_size
        } else {
            DEFAULT_BLOCK_SIZE
        };
        let mut full_block_size = table_block_size;
        let h = header_size as usize;

        let mut data = read_block(source, offset, guess_block_size);
        if data.bytes().len() < h + 4 {
            return Err(Error::Format);
        }
        let block_type = data.bytes()[h];
        if !is_block_type(block_type) {
            return Err(Error::Format);
        }
        if want_type != BLOCK_TYPE_ANY && block_type != want_type {
            return Ok(None);
        }

        let block_size = get_be24(&data.bytes()[h + 1..]);
        if block_size > guess_block_size {
            data = read_block(source, offset, block_size);
        }

        if block_type == BLOCK_TYPE_LOG {
            let skip = 4 + h;
            let block_size = block_size as usize;
            if block_size < skip {
                return Err(Error::Format);
            }
            // Log blocks give the *uncompressed* size in their header, which is
            // copied over verbatim.
            let src = data.bytes();
            let mut uncompressed = vec![0u8; block_size];
            uncompressed[..skip].copy_from_slice(&src[..skip]);

            let mut z = gix_zlib::Decompress::new();
            let status = z
                .decompress(&src[skip..], &mut uncompressed[skip..], gix_zlib::FlushDecompress::Finish)
                .map_err(|_| Error::Zlib)?;
            if status != gix_zlib::Status::StreamEnd {
                return Err(Error::Zlib);
            }
            if z.total_out() as usize + skip != block_size {
                return Err(Error::Format);
            }
            full_block_size = (skip as u64 + z.total_in()) as u32;
            data = BlockData::Owned(uncompressed);
        } else if full_block_size == 0 {
            full_block_size = block_size;
        } else if block_size < full_block_size
            && (block_size as usize) < data.bytes().len()
            && data.bytes()[block_size as usize] != 0
        {
            // A block smaller than the table's block size is either padded
            // (data followed by '\0') or followed by an unaligned next block.
            full_block_size = block_size;
        }

        let bytes = data.bytes();
        if (block_size as usize) > bytes.len() || (block_size as usize) < h + 4 + 2 {
            return Err(Error::Format);
        }
        let restart_count = get_be16(&bytes[block_size as usize - 2..]);
        let restart_off = block_size
            .checked_sub(2 + 3 * u32::from(restart_count))
            .filter(|&off| off as usize >= h + 4)
            .ok_or(Error::Format)?;

        Ok(Some(Block {
            header_off: header_size,
            data,
            hash_size,
            restart_count,
            restart_off,
            full_block_size,
            block_type,
        }))
    }

    /// The block's bytes, starting at the table header for the first block.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.data.bytes()
    }

    /// `reftable_block_type()`.
    pub fn block_type(&self) -> u8 {
        self.block_type
    }

    /// `reftable_block_first_key()` (`block.c:366-384`).
    pub(crate) fn first_key(&self, key: &mut Vec<u8>) -> Result<()> {
        let off = self.header_off as usize + 4;
        key.clear();
        decode_key(key, &self.bytes()[off..self.restart_off as usize]).ok_or(Error::General)?;
        if key.is_empty() {
            return Err(Error::Format);
        }
        Ok(())
    }

    /// `block_restart_offset()` (`block.c:386-389`).
    fn restart_offset(&self, idx: usize) -> u32 {
        get_be24(&self.bytes()[self.restart_off as usize + 3 * idx..])
    }
}

/// `struct block_iter`: a cursor over the records of one [`Block`].
///
/// The block is passed to each call instead of being borrowed by the cursor,
/// so that a table iterator can own both.
#[derive(Default, Clone)]
pub(crate) struct BlockIter {
    /// Offset within the block of the next entry to read.
    next_off: u32,
    /// Key of the last entry read.
    last_key: Vec<u8>,
    scratch: Vec<u8>,
}

impl BlockIter {
    /// `block_iter_init()` / `block_iter_seek_start()` (`block.c:391-401`).
    pub(crate) fn seek_start(&mut self, block: &Block) {
        self.last_key.clear();
        self.next_off = block.header_off + 4;
    }

    /// `block_iter_next()` (`block.c:445-473`): `Ok(false)` at the end of the block.
    pub(crate) fn next(&mut self, block: &Block, rec: &mut Record) -> Result<bool> {
        if self.next_off >= block.restart_off {
            return Ok(false);
        }
        let input = &block.bytes()[self.next_off as usize..block.restart_off as usize];
        let (extra, n) = decode_key(&mut self.last_key, input).ok_or(Error::General)?;
        if self.last_key.is_empty() {
            return Err(Error::Format);
        }
        let m = rec
            .decode(&self.last_key, extra, &input[n..], block.hash_size, &mut self.scratch)
            .map_err(|_| Error::General)?;
        self.next_off += (n + m) as u32;
        Ok(true)
    }

    /// `block_iter_reset()` (`block.c:475-480`).
    pub(crate) fn reset(&mut self) {
        self.last_key.clear();
        self.next_off = 0;
    }

    /// `block_iter_seek_key()` (`block.c:488-589`): position the cursor so that
    /// the next call to [`next()`](Self::next) yields the first record whose key
    /// is at or after `want`, if the block has one.
    pub(crate) fn seek_key(&mut self, block: &Block, want: &[u8]) -> Result<()> {
        // Binary search over the restart points for the first one _greater_
        // than the wanted key. Records at restart points are stored without
        // prefix compression, so their keys compare without decoding.
        let mut error = false;
        let i = binsearch(block.restart_count as usize, |idx| {
            let off = block.restart_offset(idx) as usize;
            let input = &block.bytes()[off..block.restart_off as usize];
            let Some((prefix_len, suffix_len, _extra, n)) = decode_keylen(input) else {
                error = true;
                return -1;
            };
            if prefix_len != 0 {
                error = true;
                return -1;
            }
            let rest = &input[n..];
            if suffix_len > rest.len() as u64 {
                error = true;
                return -1;
            }
            let suffix_len = suffix_len as usize;
            let n = want.len().min(suffix_len);
            match want[..n].cmp(&rest[..n]) {
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Greater => 0,
                std::cmp::Ordering::Equal => i32::from(want.len() < suffix_len),
            }
        });
        if error {
            return Err(Error::Format);
        }

        // i == 0: the wanted key sorts before the first record, so it is not in
        // this block; position at the start so iteration ends immediately…
        // (i > 0): the key must be in the section starting at restart i - 1.
        self.next_off = if i > 0 {
            block.restart_offset(i - 1)
        } else {
            block.header_off + 4
        };

        // Go one entry too far and back up, so that the next call to `next()`
        // yields the wanted record.
        let mut rec = Record::new(block.block_type)?;
        loop {
            let prev_off = self.next_off;
            if !self.next(block, &mut rec)? {
                self.next_off = prev_off;
                return Ok(());
            }
            rec.key(&mut self.last_key);
            if self.last_key.as_slice() >= want {
                self.next_off = prev_off;
                return Ok(());
            }
        }
    }
}
