//! `pack-check.c`'s `verify_pack()`, with the `unpack_entry()` (packfile.c) and
//! `patch_delta()` (patch-delta.c) it drives, reduced to what `fsck --full`
//! observes: the `error:` lines, in git's order, and which objects came back.
//!
//! For each pack git checks the `.idx` trailer, then the `.pack` trailer
//! against the pack's own bytes and against the copy the `.idx` records, then
//! walks every object in pack-offset order: the CRC-32 the `.idx` stores, the
//! inflate of the object (following its delta chain to the base first), and
//! finally the object's hash. Each failure is one `error()` and the walk goes
//! on, so a damaged pack reports every object it cannot produce.
//!
//! Not reproduced: the delta-base cache, which only saves work — an entry that
//! fails to inflate is never cached, so every line it causes is printed as
//! often as git prints it — and the streaming path git takes for a blob at or
//! above `core.bigFileThreshold`, which reports the same lines.

use std::collections::HashSet;

use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::odb::pack;

/// What [`verify_pack`] found in one pack.
#[derive(Default)]
pub(super) struct PackCheck {
    /// The `error:` lines, in the order git prints them.
    pub lines: Vec<String>,
    /// Whether `verify_pack()` returned non-zero, i.e. `ERROR_PACK`.
    pub failed: bool,
    /// `mark_bad_packed_object()`: ids a later delta could not use as its base.
    /// `find_pack_entry()` skips them from then on, so `has_object_pack()`
    /// answers no for them.
    pub bad: HashSet<ObjectId>,
    /// Objects the walk never handed to its callback — unpacked to nothing or
    /// to the wrong hash — which therefore never get `HAS_OBJ`.
    pub unverified: HashSet<ObjectId>,
    /// Objects the walk did hand to `fsck_obj_buffer()`, from any pack.
    pub verified: HashSet<ObjectId>,
}

impl PackCheck {
    fn error(&mut self, line: String) {
        self.lines.push(format!("error: {line}"));
        self.failed = true;
    }
}

/// `verify_pack()` (pack-check.c:183-196) over one pack whose `.idx` opened.
///
/// `pack_name` is `p->pack_name` as git spells it in every message. `repo` is
/// only asked for a delta base this pack could not produce, as git's
/// `odb_read_object_info_extended()` fallback asks every other source.
pub(super) fn verify_pack(
    repo: &gix::Repository,
    index: &pack::index::File,
    idx_bytes: &[u8],
    pack_bytes: &[u8],
    pack_name: &str,
    out: &mut PackCheck,
) {
    let hash = repo.object_hash();
    let rawsz = hash.len_in_bytes();

    // `verify_pack_index()`: `hashfile_checksum_valid()` over the `.idx`.
    if idx_bytes.len() >= rawsz {
        let (body, trailer) = idx_bytes.split_at(idx_bytes.len() - rawsz);
        if digest(hash, body).as_deref() != Some(trailer) {
            out.error(format!("Packfile index for {pack_name} hash mismatch"));
        }
    }

    // `verify_packfile()`'s trailer checks (pack-check.c:73-94).
    if pack_bytes.len() < rawsz {
        out.error(format!("packfile {pack_name} cannot be accessed"));
        return;
    }
    let pack_sig_ofs = pack_bytes.len() - rawsz;
    let pack_sig = &pack_bytes[pack_sig_ofs..];
    if digest(hash, &pack_bytes[..pack_sig_ofs]).as_deref() != Some(pack_sig) {
        out.error(format!("{pack_name} pack checksum mismatch"));
    }
    // `index_base + index_size - r->hash_algo->hexsz`: git steps back by the
    // *hex* length, which for the two-hash `.idx` trailer lands on the pack
    // checksum it records.
    let hexsz = hash.len_in_hex();
    let recorded = idx_bytes
        .len()
        .checked_sub(hexsz)
        .and_then(|at| idx_bytes.get(at..at + rawsz));
    if recorded != Some(pack_sig) {
        out.error(format!("{pack_name} pack checksum does not match its index"));
    }

    // Every object, sorted by pack offset, with the trailer as the end sentinel.
    let mut entries: Vec<(u64, u32)> = (0..index.num_objects())
        .map(|nr| (index.pack_offset_at_index(nr), nr))
        .collect();
    entries.sort_unstable();
    let ends: Vec<u64> = entries
        .iter()
        .skip(1)
        .map(|e| e.0)
        .chain(std::iter::once(pack_sig_ofs as u64))
        .collect();

    let reader = Reader { repo, index, pack: pack_bytes, pack_name, hash };
    for (&(offset, nr), &end) in entries.iter().zip(&ends) {
        let oid = index.oid_at_index(nr).to_owned();
        // `check_pack_crc()` for a v2 index (pack-check.c:108-117).
        if let Some(crc) = index.crc32_at_index(nr) {
            let span = pack_bytes.get(offset as usize..end as usize).unwrap_or_default();
            if gix::features::hash::crc32(span) != crc {
                out.error(format!(
                    "index CRC mismatch for object {oid} from {pack_name} at offset {offset}"
                ));
            }
        }
        match reader.unpack_entry(offset, out) {
            None => {
                out.error(format!("cannot unpack {oid} from {pack_name} at offset {offset}"));
                out.unverified.insert(oid);
            }
            Some((kind, data)) => {
                if gix::objs::compute_hash(hash, kind, &data).ok() != Some(oid) {
                    out.error(format!("packed {oid} from {pack_name} is corrupt"));
                    out.unverified.insert(oid);
                } else {
                    out.verified.insert(oid);
                }
            }
        }
    }
}

/// The digest of `bytes` in the repository's hash, as raw bytes.
fn digest(hash: gix::hash::Kind, bytes: &[u8]) -> Option<Vec<u8>> {
    let mut hasher = gix::hash::hasher(hash);
    hasher.update(bytes);
    hasher.try_finalize().ok().map(|id| id.as_bytes().to_vec())
}

/// `OBJ_OFS_DELTA` / `OBJ_REF_DELTA`.
const OBJ_OFS_DELTA: i32 = 6;
const OBJ_REF_DELTA: i32 = 7;

struct Reader<'a> {
    repo: &'a gix::Repository,
    index: &'a pack::index::File,
    pack: &'a [u8],
    pack_name: &'a str,
    hash: gix::hash::Kind,
}

impl Reader<'_> {
    /// `unpack_object_header()` (packfile.c:1137-1164, :1225-1249): the type and
    /// size at `*curpos`, advancing past the header; `-1` is `OBJ_BAD`.
    fn object_header(&self, curpos: &mut u64, out: &mut PackCheck) -> (i32, usize) {
        let buf = self.pack.get(*curpos as usize..).unwrap_or_default();
        let Some(&first) = buf.first() else {
            return (-1, 0);
        };
        let mut c = first as usize;
        let kind = ((c >> 4) & 7) as i32;
        let mut size = c & 15;
        let mut shift = 4u32;
        let mut used = 1usize;
        while c & 0x80 != 0 {
            if buf.len() <= used || usize::BITS - 7 < shift {
                out.error("bad object header".to_string());
                return (-1, 0);
            }
            c = buf[used] as usize;
            used += 1;
            size = size.saturating_add((c & 0x7f) << shift);
            shift += 7;
        }
        *curpos += used as u64;
        (kind, size)
    }

    /// `get_delta_base()` (packfile.c:1273-1312): the base's offset, `0` when
    /// the reference is out of bounds or names an object not in this pack.
    fn delta_base(&self, curpos: &mut u64, kind: i32, delta_obj_offset: u64) -> u64 {
        let info = self.pack.get(*curpos as usize..).unwrap_or_default();
        if kind == OBJ_OFS_DELTA {
            let mut used = 0usize;
            let Some(&c0) = info.first() else { return 0 };
            used += 1;
            let mut c = c0;
            let mut base_offset = u64::from(c & 127);
            while c & 128 != 0 {
                base_offset += 1;
                if base_offset == 0 || base_offset >> (64 - 7) != 0 {
                    return 0; // overflow
                }
                let Some(&next) = info.get(used) else { return 0 };
                used += 1;
                c = next;
                base_offset = (base_offset << 7) + u64::from(c & 127);
            }
            if base_offset == 0 || base_offset >= delta_obj_offset {
                return 0; // out of bound
            }
            *curpos += used as u64;
            delta_obj_offset - base_offset
        } else {
            let rawsz = self.hash.len_in_bytes();
            let Some(raw) = info.get(..rawsz) else { return 0 };
            *curpos += rawsz as u64;
            let oid = ObjectId::from_bytes_or_panic(raw);
            self.index
                .lookup(oid)
                .map_or(0, |nr| self.index.pack_offset_at_index(nr))
        }
    }

    /// `unpack_compressed_entry()` (packfile.c:1723-1766): inflate exactly
    /// `size` bytes at `curpos`, printing `git_inflate()`'s complaint when zlib
    /// refuses the stream.
    fn compressed_entry(&self, curpos: u64, size: usize, out: &mut PackCheck) -> Option<Vec<u8>> {
        let input = self.pack.get(curpos as usize..).unwrap_or_default();
        // `stream.avail_out = size + 1`: one spare byte catches a payload that
        // is longer than its header says.
        let mut buffer = vec![0u8; size + 1];
        let mut z = gix::zlib::Decompress::new();
        let status = match z.decompress(input, &mut buffer, gix::zlib::FlushDecompress::Finish) {
            Ok(status) => status,
            Err(e) => {
                out.lines.push(super::fsck::inflate_error_line(&z, &e));
                return None;
            }
        };
        if !matches!(status, gix::zlib::Status::StreamEnd) || z.total_out() != size as u64 {
            return None;
        }
        buffer.truncate(size);
        Some(buffer)
    }

    /// The type of the object at `offset` for a delta-base diagnostic: the id
    /// the `.idx` lists there, if any (`offset_to_pack_pos()`).
    fn oid_at_offset(&self, offset: u64) -> Option<ObjectId> {
        (0..self.index.num_objects())
            .find(|&nr| self.index.pack_offset_at_index(nr) == offset)
            .map(|nr| self.index.oid_at_index(nr).to_owned())
    }

    /// `unpack_entry()` (packfile.c:1784-2000).
    fn unpack_entry(&self, obj_offset: u64, out: &mut PackCheck) -> Option<(Kind, Vec<u8>)> {
        let mut obj_offset = obj_offset;
        let mut curpos = obj_offset;
        // (obj_offset, curpos past the base reference, delta size)
        let mut delta_stack: Vec<(u64, u64, usize)> = Vec::new();

        // PHASE 1: drill down to the innermost base object.
        let (mut kind, mut size);
        let mut data: Option<Vec<u8>> = None;
        let mut failed_reference = false;
        loop {
            (kind, size) = self.object_header(&mut curpos, out);
            if kind != OBJ_OFS_DELTA && kind != OBJ_REF_DELTA {
                break;
            }
            let base_offset = self.delta_base(&mut curpos, kind, obj_offset);
            if base_offset == 0 {
                out.error(format!(
                    "failed to validate delta base reference at offset {curpos} from {}",
                    self.pack_name
                ));
                // Bail to phase 2, in hopes of recovery.
                failed_reference = true;
                break;
            }
            delta_stack.push((obj_offset, curpos, size));
            curpos = base_offset;
            obj_offset = base_offset;
        }

        // PHASE 2: handle the base.
        match kind {
            OBJ_OFS_DELTA | OBJ_REF_DELTA if failed_reference => {}
            1..=4 => data = self.compressed_entry(curpos, size, out),
            _ => out.error(format!(
                "unknown object type {kind} at offset {obj_offset} in {}",
                self.pack_name
            )),
        }
        let mut final_kind = base_kind(kind);

        // PHASE 3: apply deltas in order.
        while let Some((delta_offset, delta_curpos, delta_size)) = delta_stack.pop() {
            let mut base = data.take();
            if base.is_none() {
                // "We're probably in deep shit, but let's try to fetch the
                // required base anyway from another pack or loose."
                if let Some(base_oid) = self.oid_at_offset(obj_offset) {
                    out.error(format!(
                        "failed to read delta base object {base_oid} at offset {obj_offset} from {}",
                        self.pack_name
                    ));
                    out.bad.insert(base_oid);
                    base = self.elsewhere(base_oid).map(|(k, d)| {
                        final_kind = Some(k);
                        d
                    });
                }
            }
            obj_offset = delta_offset;
            let Some(base) = base else { continue };
            match self.compressed_entry(delta_curpos, delta_size, out) {
                None => {
                    out.error(format!(
                        "failed to unpack compressed delta at offset {delta_curpos} from {}",
                        self.pack_name
                    ));
                }
                Some(delta) => {
                    data = match patch_delta(&base, &delta) {
                        Ok(result) => Some(result),
                        Err(line) => {
                            if let Some(line) = line {
                                out.lines.push(format!("error: {line}"));
                            }
                            out.error("failed to apply delta".to_string());
                            None
                        }
                    };
                }
            }
        }
        Some((final_kind?, data?))
    }

    /// The fallback read of a delta base this pack could not produce: every
    /// other copy the odb has, which for a base marked bad here means a loose
    /// object or another pack.
    fn elsewhere(&self, id: ObjectId) -> Option<(Kind, Vec<u8>)> {
        let object = self.repo.find_object(id).ok()?;
        Some((object.kind, object.data.clone()))
    }
}

/// The object type of a non-delta pack entry.
fn base_kind(kind: i32) -> Option<Kind> {
    match kind {
        1 => Some(Kind::Commit),
        2 => Some(Kind::Tree),
        3 => Some(Kind::Blob),
        4 => Some(Kind::Tag),
        _ => None,
    }
}

/// `get_delta_hdr_size()` (delta.h:89-102).
fn delta_hdr_size(data: &[u8], at: &mut usize) -> usize {
    let mut size = 0usize;
    let mut shift = 0u32;
    loop {
        let cmd = data[*at];
        *at += 1;
        if shift < usize::BITS {
            size |= usize::from(cmd & 0x7f) << shift;
        }
        shift += 7;
        if cmd & 0x80 == 0 || *at >= data.len() {
            return size;
        }
    }
}

/// `patch_delta()` (patch-delta.c:15-96): the delta applied to `base`, or
/// `Err` for a delta that does not fit it, carrying the one `error()` git
/// prints for it, if any. Shared with `unpack-objects`.
pub(super) fn patch_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, Option<&'static str>> {
    const DELTA_SIZE_MIN: usize = 4;
    if delta.len() < DELTA_SIZE_MIN {
        return Err(None);
    }
    let mut at = 0usize;
    if delta_hdr_size(delta, &mut at) != base.len() {
        return Err(None);
    }
    let mut size = delta_hdr_size(delta, &mut at);
    let mut dst = Vec::with_capacity(size);
    const GONE_WILD: Result<Vec<u8>, Option<&str>> = Err(Some("delta replay has gone wild"));
    while at < delta.len() {
        let cmd = delta[at];
        at += 1;
        if cmd & 0x80 != 0 {
            let (mut cp_off, mut cp_size) = (0usize, 0usize);
            for (bit, shift, is_off) in [
                (0x01, 0, true),
                (0x02, 8, true),
                (0x04, 16, true),
                (0x08, 24, true),
                (0x10, 0, false),
                (0x20, 8, false),
                (0x40, 16, false),
            ] {
                if cmd & bit != 0 {
                    let Some(&b) = delta.get(at) else { return GONE_WILD };
                    at += 1;
                    let v = usize::from(b) << shift;
                    if is_off { cp_off |= v } else { cp_size |= v }
                }
            }
            if cp_size == 0 {
                cp_size = 0x10000;
            }
            match cp_off.checked_add(cp_size) {
                Some(end) if end <= base.len() && cp_size <= size => {
                    dst.extend_from_slice(&base[cp_off..end]);
                    size -= cp_size;
                }
                _ => return GONE_WILD,
            }
        } else if cmd != 0 {
            let n = usize::from(cmd);
            if n > size || n > delta.len() - at {
                return GONE_WILD;
            }
            dst.extend_from_slice(&delta[at..at + n]);
            at += n;
            size -= n;
        } else {
            return Err(Some("unexpected delta opcode 0"));
        }
    }
    if size != 0 {
        return GONE_WILD;
    }
    Ok(dst)
}
