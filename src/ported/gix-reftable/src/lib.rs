//! A port of git's reftable library (`reftable/*.c`, git v2.55.0).
//!
//! A reftable is a binary, block-oriented file holding references and reflog
//! entries sorted by name. A repository keeps a *stack* of them under
//! `$GIT_DIR/reftable`, listed newest-last in `tables.list`; a lookup merges the
//! stack so that newer tables shadow older ones, and writers append a table per
//! transaction and compact the stack geometrically.
//!
//! The module layout follows the C sources one to one:
//!
//! | module          | C source                     |
//! |-----------------|------------------------------|
//! | [`basics`]      | `basics.c`                   |
//! | [`error`]       | `error.c`, `reftable-error.h`|
//! | [`record`]      | `record.c`                   |
//! | [`block`]       | `block.c`                    |
//! | [`blocksource`] | `blocksource.c`              |
//! | [`writer`]      | `writer.c`, `tree.c`         |
//! | [`table`]       | `table.c`                    |
//! | [`iter`]        | `iter.c`                     |
//! | [`pq`]          | `pq.c`                       |
//! | [`merged`]      | `merged.c`                   |
//! | [`stack`]       | `stack.c`, `system.c`        |
//! | [`fsck`]        | `fsck.c`                     |
//!
//! Error handling mirrors the library's integer protocol: a negative C return
//! is an [`Error`], and the positive "not found / end of iteration" return is
//! `Ok(false)` (or `Ok(None)`) at the Rust boundary.
#![deny(missing_docs, unsafe_code)]

pub mod basics;
pub mod block;
pub mod blocksource;
pub mod error;
pub mod fsck;
pub mod iter;
pub mod merged;
pub mod pq;
pub mod record;
pub mod stack;
pub mod table;
pub mod writer;

pub use basics::HashId;
pub use error::{Error, Result};
pub use iter::Iterator;
pub use merged::MergedTable;
pub use record::{LogRecord, LogUpdate, LogValue, RefRecord, RefValue};
pub use stack::{Addition, LogExpiryConfig, Stack};
pub use table::Table;
pub use writer::{WriteOptions, Writer};

/// Block type of ref records (`REFTABLE_BLOCK_TYPE_REF`, `reftable-constants.h`).
pub const BLOCK_TYPE_REF: u8 = b'r';
/// Block type of reflog records (`REFTABLE_BLOCK_TYPE_LOG`).
pub const BLOCK_TYPE_LOG: u8 = b'g';
/// Block type of object-to-ref index records (`REFTABLE_BLOCK_TYPE_OBJ`).
pub const BLOCK_TYPE_OBJ: u8 = b'o';
/// Block type of index records (`REFTABLE_BLOCK_TYPE_INDEX`).
pub const BLOCK_TYPE_INDEX: u8 = b'i';
/// Accept any block type when reading a block (`REFTABLE_BLOCK_TYPE_ANY`).
pub const BLOCK_TYPE_ANY: u8 = 0;

/// `MAX_RESTARTS` (`constants.h`).
pub(crate) const MAX_RESTARTS: u32 = (1 << 16) - 1;
/// `DEFAULT_BLOCK_SIZE` (`constants.h`).
pub const DEFAULT_BLOCK_SIZE: u32 = 4096;
/// `DEFAULT_GEOMETRIC_FACTOR` (`constants.h`).
pub(crate) const DEFAULT_GEOMETRIC_FACTOR: u8 = 2;
