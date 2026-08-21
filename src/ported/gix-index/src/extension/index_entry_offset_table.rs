//! The `IEOT` (index entry offset table) extension: where each block of index entries starts,
//! so a reader can hand one block per thread to `load_cache_entries_threaded()`
//! (read-cache.c:2126-2190) instead of walking the variable-length entries serially.
//!
//! It is pure acceleration — an index without it decodes identically, just on one thread — which
//! is why git writes it only when the user asked for threaded reads, and why it is written
//! *first* among the extensions: "so that we can minimize the number of extensions we have to
//! scan through to find it during load" (read-cache.c:2975-2980).

use crate::{extension, extension::Signature, util::read_u32};

#[derive(Debug, Clone, Copy)]
pub struct Offset {
    pub from_beginning_of_file: u32,
    pub num_entries: u32,
}

pub const SIGNATURE: Signature = *b"IEOT";

/// `IEOT_VERSION` (read-cache.c:3662) — the only version `read_ieot_extension()` accepts
/// (read-cache.c:3686-3690), so it is also the only one worth writing.
pub const VERSION: u32 = 1;

/// `THREAD_COST` (read-cache.c:2092): the entry count below which git does not consider it
/// worth handing a block to another thread when it is picking the block count itself.
const THREAD_COST: u32 = 10_000;

/// How many entries go into one block, given `nr_threads` and the number of entries in the
/// index — or `None` when git would write no `IEOT` at all.
///
/// Port of the `ieot` half of `do_write_index()` (read-cache.c:2877-2904). `nr_threads` is
/// `index.threads` as `repo_config_get_index_threads()` resolves it, with git's two special
/// values: `0` means "one thread per core" and is the `true` spelling of the key, and a `1`
/// (or an unset key, or an unset `index.recordOffsetTable` with threading off) never reaches
/// here because the caller's `nr_threads != 1 && record_ieot()` gate (read-cache.c:2877)
/// already refused it.
///
/// Two clamps decide whether the extension appears at all, and both are about there being
/// real work to divide: the block count cannot exceed the number of entries, and
/// "no reason to write out the IEOT extension if we don't have enough blocks to utilize
/// multi-threading" (read-cache.c:2896-2900) drops it whenever that leaves one block or fewer.
/// So a two-entry index gets `IEOT` at `index.threads=2` and none at `index.threads=1`,
/// exactly as stock git does.
pub fn entries_per_block(nr_threads: u32, num_entries: u32) -> Option<u32> {
    let blocks = if nr_threads == 0 {
        // `ieot_blocks = istate->cache_nr / THREAD_COST; cpus = online_cpus();
        //  if (ieot_blocks > cpus - 1) ieot_blocks = cpus - 1;` — the cap leaves one core for
        // the thread that loads the extensions. `online_cpus()` cannot report zero, but a
        // single-core machine yields a cap of zero and therefore no extension.
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let cap = u32::try_from(cpus.saturating_sub(1)).unwrap_or(u32::MAX);
        (num_entries / THREAD_COST).min(cap)
    } else {
        // `ieot_blocks = nr_threads; if (ieot_blocks > istate->cache_nr) ieot_blocks = cache_nr;`
        nr_threads.min(num_entries)
    };
    if blocks <= 1 {
        return None;
    }
    // `ieot_entries = DIV_ROUND_UP(entries, ieot_blocks);`
    Some(num_entries.div_ceil(blocks))
}

/// Serialise `offsets` into the extension body — the version word followed by one
/// `(offset, count)` pair per block, all big-endian.
///
/// Port of `write_ieot_extension()` (read-cache.c:3713-3733). The body carries no count of its
/// own; `read_ieot_extension()` derives it from the extension size
/// (`nr = (extsize - sizeof(uint32_t)) / (sizeof(uint32_t) + sizeof(uint32_t))`,
/// read-cache.c:3694-3695), which is why an empty `offsets` would produce an extension that
/// git rejects with "invalid number of IEOT entries" — callers must not write one.
pub fn write_to(mut out: impl std::io::Write, offsets: &[Offset]) -> std::io::Result<()> {
    debug_assert!(!offsets.is_empty(), "an IEOT with no blocks is rejected on read");
    out.write_all(&VERSION.to_be_bytes())?;
    for offset in offsets {
        out.write_all(&offset.from_beginning_of_file.to_be_bytes())?;
        out.write_all(&offset.num_entries.to_be_bytes())?;
    }
    Ok(())
}

pub fn decode(data: &[u8]) -> Option<Vec<Offset>> {
    let (version, mut data) = read_u32(data)?;
    match version {
        1 => {}
        _unknown => return None,
    }

    let entry_size = 4 + 4;
    let num_offsets = data.len() / entry_size;
    if num_offsets == 0 || data.len() % entry_size != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(entry_size);
    for _ in 0..num_offsets {
        let (offset, chunk) = read_u32(data)?;
        let (num_entries, chunk) = read_u32(chunk)?;
        out.push(Offset {
            from_beginning_of_file: offset,
            num_entries,
        });
        data = chunk;
    }
    debug_assert!(data.is_empty());

    out.into()
}

pub fn find(extensions: &[u8], object_hash: gix_hash::Kind) -> Option<Vec<Offset>> {
    extension::Iter::new_without_checksum(extensions, object_hash)?
        .find_map(|(sig, ext_data)| (sig == SIGNATURE).then_some(ext_data))
        .and_then(decode)
}
