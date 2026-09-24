//! `blocksource.c`: the bytes a table is read from.
//!
//! C abstracts over a vtable with an in-memory and a memory-mapped file
//! implementation, and hands out `reftable_block_data` slices that must be
//! returned to their source. Here a [`BlockSource`] is shared by reference
//! count, and a block keeps its source alive by holding a clone of the `Arc`.

use std::{path::Path, sync::Arc};

use crate::{Error, Result};

enum Backing {
    Mapped(memmap2::Mmap),
    Buf(Vec<u8>),
}

/// `struct reftable_block_source`: the whole content of one table.
pub struct BlockSource {
    backing: Backing,
}

impl BlockSource {
    /// `block_source_from_buf()`: read a table held in memory.
    pub fn from_buf(buf: Vec<u8>) -> Arc<Self> {
        Arc::new(BlockSource { backing: Backing::Buf(buf) })
    }

    /// `reftable_block_source_from_file()` (`blocksource.c:132-174`): map the
    /// file at `path`. A missing file is [`Error::NotExist`], which the stack
    /// treats as a sign of a concurrent writer.
    pub fn from_file(path: &Path) -> Result<Arc<Self>> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(Error::NotExist),
            Err(_) => return Err(Error::General),
        };
        let len = file.metadata().map_err(|_| Error::Io)?.len();
        // An empty file cannot be mapped portably; it is too short to be a
        // table anyway, which `Table::new()` reports.
        let backing = if len == 0 {
            Backing::Buf(Vec::new())
        } else {
            // SAFETY: git never rewrites a table in place: a table is written
            // under a temporary name and renamed into the stack, and only ever
            // unlinked afterwards.
            #[expect(unsafe_code)]
            let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|_| Error::Io)?;
            Backing::Mapped(map)
        };
        Ok(Arc::new(BlockSource { backing }))
    }

    /// `block_source_size()`.
    pub fn size(&self) -> u64 {
        self.bytes().len() as u64
    }

    /// All bytes of the source.
    pub fn bytes(&self) -> &[u8] {
        match &self.backing {
            Backing::Mapped(m) => m,
            Backing::Buf(b) => b,
        }
    }
}

/// `struct reftable_block_data`: a contiguous range of a source, or bytes
/// owned outright (an inflated log block).
#[derive(Clone)]
pub(crate) enum BlockData {
    Shared { source: Arc<BlockSource>, off: usize, len: usize },
    Owned(Vec<u8>),
}

impl Default for BlockData {
    fn default() -> Self {
        BlockData::Owned(Vec::new())
    }
}

impl BlockData {
    /// `block_source_read_data()`: `size` bytes at `off`, which must be in range.
    pub(crate) fn read(source: &Arc<BlockSource>, off: u64, size: u32) -> BlockData {
        BlockData::Shared {
            source: Arc::clone(source),
            off: off as usize,
            len: size as usize,
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            BlockData::Shared { source, off, len } => &source.bytes()[*off..*off + *len],
            BlockData::Owned(v) => v,
        }
    }
}
