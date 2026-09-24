//! `basics.c`: big-endian helpers, binary search, and `tables.list` parsing.

use crate::{Error, Result};

/// `enum reftable_hash` (`reftable-basics.h`): the hash function a table's
/// object IDs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HashId {
    /// `REFTABLE_HASH_SHA1`.
    #[default]
    Sha1,
    /// `REFTABLE_HASH_SHA256`.
    Sha256,
}

impl HashId {
    /// `hash_size()` (`basics.c:267-278`): the length of an object ID in bytes.
    pub fn size(self) -> usize {
        match self {
            HashId::Sha1 => 20,
            HashId::Sha256 => 32,
        }
    }

    /// The format ID written into a version 2 header, `"sha1"` or `"s256"` in
    /// big endian (`REFTABLE_FORMAT_ID_*`, `basics.h:288-289`).
    pub(crate) fn format_id(self) -> u32 {
        match self {
            HashId::Sha1 => FORMAT_ID_SHA1,
            HashId::Sha256 => FORMAT_ID_SHA256,
        }
    }
}

/// `REFTABLE_HASH_SIZE_MAX`.
pub const HASH_SIZE_MAX: usize = 32;

pub(crate) const FORMAT_ID_SHA1: u32 = 0x7368_6131;
pub(crate) const FORMAT_ID_SHA256: u32 = 0x7332_3536;

/// `reftable_put_be24()`.
pub(crate) fn put_be24(out: &mut [u8], i: u32) {
    out[0] = (i >> 16) as u8;
    out[1] = (i >> 8) as u8;
    out[2] = i as u8;
}

/// `reftable_get_be24()`.
pub(crate) fn get_be24(p: &[u8]) -> u32 {
    (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2])
}

/// `reftable_get_be16()`.
pub(crate) fn get_be16(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[0], p[1]])
}

/// `reftable_get_be32()`.
pub(crate) fn get_be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

/// `reftable_get_be64()`.
pub(crate) fn get_be64(p: &[u8]) -> u64 {
    u64::from_be_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]])
}

/// `binsearch()` (`basics.c:150-176`): the smallest index `i` in `[0, sz)` at
/// which `f(i) > 0`, assuming `f` is ascending, or `sz` if there is none. A
/// negative `f` aborts the search and also yields `sz`.
pub(crate) fn binsearch(sz: usize, mut f: impl FnMut(usize) -> i32) -> usize {
    // C would probe `f(0)` of an empty range; nothing can be there.
    if sz == 0 {
        return 0;
    }
    let mut lo = 0;
    let mut hi = sz;

    // Invariants:
    //
    //  (hi == sz) || f(hi) == true
    //  (lo == 0 && f(0) == true) || fi(lo) == false
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let ret = f(mid);
        if ret < 0 {
            return sz;
        }
        if ret > 0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }

    if lo != 0 {
        return hi;
    }
    if f(0) != 0 { 0 } else { 1 }
}

/// `parse_names()` (`basics.c:198-247`): split a newline separated list of
/// table names, discarding empty ones. A final entry without its newline is a
/// format error, as `strchr()` finds none; so is an embedded NUL before the
/// newline, which ends `strchr()`'s search early.
pub(crate) fn parse_names(buf: &[u8]) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut p = 0;
    while p < buf.len() {
        let rest = &buf[p..];
        let next = match rest.iter().position(|&b| b == b'\n' || b == 0) {
            Some(n) if rest[n] == b'\n' => p + n,
            _ => return Err(Error::Format),
        };
        if p < next {
            names.push(String::from_utf8_lossy(&buf[p..next]).into_owned());
        }
        p = next + 1;
    }
    Ok(names)
}

/// `common_prefix_size()` (`basics.c:258-265`).
pub(crate) fn common_prefix_size(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}
