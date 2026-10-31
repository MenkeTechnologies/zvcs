//! `packfile.c`'s read path through one pack entry, for the entries `gix-pack`'s decoder refuses.
//!
//! A pack entry that decodes is read by `gix-pack` as before. One that does not is read again here
//! the way git reads it, because what git does with a broken entry is observable: the `error()`
//! lines `unpack_entry()`, `packed_to_object_type()`, `unpack_object_header_buffer()`,
//! `get_size_from_delta()`, `patch_delta()` and `git_inflate()` print, the `die()`s of `use_pack()`,
//! which objects `mark_bad_packed_object()` marks, and the delta bases it recovers from another
//! copy (packfile.c:860-1700). The caller then marks the object bad in this pack and goes on to the
//! next copy, as `odb_source_packed_read_object_info()` does (odb/source-packed.c:117-127).
//!
//! Not reproduced: the delta-base cache, which only saves work, since an entry that fails to inflate
//! is never cached; and `do_check_packed_object_crc`, which only `pack-objects` sets.

use gix_hash::{ObjectId, oid};
use gix_object::Kind;

/// `enum object_type` (object.h:96-106) as `packfile.c` passes it around.
const OBJ_BAD: i32 = -1;
const OBJ_NONE: i32 = 0;
const OBJ_OFS_DELTA: i32 = 6;
const OBJ_REF_DELTA: i32 = 7;

/// The reads of the rest of the object database `packfile.c` makes while it recovers from a bad
/// entry, and `mark_bad_packed_object()` for the pack being read.
pub(crate) trait Odb {
    /// `odb_read_object_info(r, oid, NULL)`: the object's type, `None` for a negative answer.
    fn read_type(&mut self, id: &oid) -> Option<Kind>;
    /// `odb_read_object_info_extended()` with `typep`, `sizep` and `contentp` and no flags.
    fn read_object(&mut self, id: &oid) -> Option<(Kind, Vec<u8>)>;
    /// `mark_bad_packed_object(p, oid)` (packfile.c:981-984).
    fn mark_bad(&mut self, id: &oid);
}

/// What `packfile.c` reads through one `struct packed_git`.
pub(crate) struct Pack<'a> {
    /// The whole pack file, trailer included: `p->pack_size` bytes.
    pub data: &'a [u8],
    /// `p->pack_name`, as every message spells it.
    pub name: &'a str,
    /// The pack's own `.idx` (`p->index_data`), which `find_pack_entry_one()` and the reverse index
    /// read whether or not a multi-pack-index named the entry.
    pub index: Option<gix_pack::index::File>,
    /// `p->repo->hash_algo`.
    pub hash: gix_hash::Kind,
}

/// `die()`: what stdout holds is flushed as `exit()` flushes stdio, then `fatal: <msg>` and 128.
pub(crate) fn die(msg: &str) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    eprintln!("fatal: {msg}");
    std::process::exit(128)
}

fn error(msg: &str) {
    eprintln!("error: {msg}");
}

/// The object type of a non-delta `enum object_type` value.
fn kind_of(kind: i32) -> Option<Kind> {
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

/// `patch_delta()` (patch-delta.c:15-96): `delta` applied to `base`, or `None` after the one
/// `error()` git prints for a delta that does not fit, if any.
pub fn patch_delta(base: &[u8], delta: &[u8]) -> Option<Vec<u8>> {
    patch_delta_quiet(base, delta).map_err(|line| line.map(error)).ok()
}

/// [`patch_delta()`] with its `error()` line handed back rather than printed.
pub fn patch_delta_quiet(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, Option<&'static str>> {
    const DELTA_SIZE_MIN: usize = 4;
    const BAD_LENGTH: Result<Vec<u8>, Option<&str>> = Err(Some("delta replay has gone wild"));
    if delta.len() < DELTA_SIZE_MIN {
        return Err(None);
    }
    let mut at = 0usize;
    // Make sure the orig file size matches what we expect.
    if delta_hdr_size(delta, &mut at) != base.len() {
        return Err(None);
    }
    let mut size = delta_hdr_size(delta, &mut at);
    let mut dst = Vec::with_capacity(size);
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
                    let Some(&b) = delta.get(at) else { return BAD_LENGTH };
                    at += 1;
                    let v = usize::from(b) << shift;
                    if is_off {
                        cp_off |= v;
                    } else {
                        cp_size |= v;
                    }
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
                _ => return BAD_LENGTH,
            }
        } else if cmd != 0 {
            let n = usize::from(cmd);
            if n > size || n > delta.len() - at {
                return BAD_LENGTH;
            }
            dst.extend_from_slice(&delta[at..at + n]);
            at += n;
            size -= n;
        } else {
            // cmd == 0 is reserved for future encoding extensions.
            return Err(Some("unexpected delta opcode 0"));
        }
    }
    if size != 0 {
        return BAD_LENGTH;
    }
    Ok(dst)
}

/// `git-zlib.c::zerr_to_string()` over the codes `gix-zlib` tells apart.
fn zerr_to_string(e: &gix_zlib::DecompressError) -> &'static str {
    use gix_zlib::DecompressError as E;
    match e {
        E::InsufficientMemory => "out of memory",
        E::NeedDict => "needs dictionary",
        E::DataError => "data stream error",
        E::StreamError => "stream consistency error",
    }
}

/// How an inflate of pack data ended, as `git_inflate()` (git-zlib.c:132-168) reports it.
enum Inflated {
    /// `Z_STREAM_END`.
    End,
    /// `Z_OK` / `Z_BUF_ERROR` with the output buffer full.
    OutputFull,
    /// `Z_OK` / `Z_BUF_ERROR` with input left to feed: the caller's loop asks `use_pack()` again.
    NeedsInput,
    /// Any other status, after `git_inflate()`'s `error("inflate: %s (%s)")`.
    Failed,
}

impl Pack<'_> {
    /// `use_pack()` (packfile.c:620-660): the bytes from `offset` to the end of the file, whose window
    /// is never shorter than a hash; an offset into the trailer or past it is `die()`.
    fn use_pack(&self, offset: u64) -> &[u8] {
        let rawsz = self.hash.len_in_bytes() as u64;
        if offset > (self.data.len() as u64).saturating_sub(rawsz) {
            die("offset beyond end of packfile (truncated pack?)");
        }
        &self.data[offset as usize..]
    }

    /// `git_inflate()`'s loop as `unpack_compressed_entry()` and `get_size_from_delta()` drive it:
    /// inflate the stream at `curpos` into `out`, asking `use_pack()` for more input while zlib can
    /// still make progress. Returns how it ended and how many bytes came out.
    fn inflate(&self, mut curpos: u64, out: &mut [u8], report: bool) -> (Inflated, usize) {
        let mut z = gix_zlib::Decompress::new();
        loop {
            let input = self.use_pack(curpos);
            let (before_in, before_out) = (z.total_in(), z.total_out() as usize);
            let status = z.decompress(input, &mut out[before_out..], gix_zlib::FlushDecompress::Finish);
            let produced = z.total_out() as usize;
            match status {
                Ok(gix_zlib::Status::StreamEnd) => return (Inflated::End, produced),
                Ok(_) if produced == out.len() => return (Inflated::OutputFull, produced),
                Ok(_) => {
                    let consumed = z.total_in() - before_in;
                    if consumed == 0 && produced == before_out {
                        return (Inflated::NeedsInput, produced);
                    }
                    curpos += consumed;
                }
                Err(e) => {
                    if report {
                        error(&format!(
                            "inflate: {} ({})",
                            zerr_to_string(&e),
                            z.error_message().unwrap_or("no message")
                        ));
                    }
                    return (Inflated::Failed, produced);
                }
            }
        }
    }

    /// `unpack_object_header()` (packfile.c:960-979) over `unpack_object_header_buffer()`
    /// (packfile.c:869-895): the representation type at `*curpos` and its size, advancing past the
    /// header; `OBJ_BAD` for a header that runs off its window or overflows.
    fn unpack_object_header(&self, curpos: &mut u64) -> (i32, usize) {
        let buf = self.use_pack(*curpos);
        let mut used = 0usize;
        let mut c = usize::from(buf[used]);
        used += 1;
        let kind = ((c >> 4) & 7) as i32;
        let mut size = c & 15;
        let mut shift = 4u32;
        while c & 0x80 != 0 {
            if buf.len() <= used || usize::BITS - 7 < shift {
                error("bad object header");
                return (OBJ_BAD, 0);
            }
            c = usize::from(buf[used]);
            used += 1;
            size = size.saturating_add((c & 0x7f) << shift);
            shift += 7;
        }
        *curpos += used as u64;
        (kind, size)
    }

    /// `find_pack_entry_one()` (packfile.c:1808-1818): the offset of `id` in this pack's own index,
    /// `0` when it is not there.
    fn find_pack_entry_one(&self, id: &oid) -> u64 {
        self.index
            .as_ref()
            .and_then(|index| index.lookup(id).map(|nr| index.pack_offset_at_index(nr)))
            .unwrap_or(0)
    }

    /// `get_delta_base()` (packfile.c:990-1028): the base's offset, `0` for a reference out of
    /// bounds or to an object not in this pack.
    fn get_delta_base(&self, curpos: &mut u64, kind: i32, delta_obj_offset: u64) -> u64 {
        let base_info = self.use_pack(*curpos);
        if kind == OBJ_OFS_DELTA {
            let mut used = 0usize;
            let mut c = base_info[used];
            used += 1;
            let mut base_offset = u64::from(c & 127);
            while c & 128 != 0 {
                base_offset = base_offset.wrapping_add(1);
                if base_offset == 0 || base_offset >> (64 - 7) != 0 {
                    return 0; // overflow
                }
                let Some(&next) = base_info.get(used) else { return 0 };
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
            *curpos += rawsz as u64;
            let Some(raw) = base_info.get(..rawsz) else { return 0 };
            self.find_pack_entry_one(&ObjectId::from_bytes_or_panic(raw))
        }
    }

    /// `offset_to_pack_pos()` + `nth_packed_object_id()` (pack-revindex.c:433-458): the id of the
    /// object that starts at `offset`, or `None` after `error("bad offset for revindex")`.
    fn oid_at_offset(&self, offset: u64) -> Option<ObjectId> {
        let index = self.index.as_ref()?;
        let found = (0..index.num_objects())
            .find(|&nr| index.pack_offset_at_index(nr) == offset)
            .map(|nr| index.oid_at_index(nr).to_owned());
        if found.is_none() {
            error("bad offset for revindex");
        }
        found
    }

    /// `retry_bad_packed_offset()` (packfile.c:1060-1076): mark the object at `obj_offset` bad here
    /// and ask the rest of the object database for its type.
    fn retry_bad_packed_offset(&self, obj_offset: u64, odb: &mut dyn Odb) -> i32 {
        let Some(id) = self.oid_at_offset(obj_offset) else {
            return OBJ_BAD;
        };
        odb.mark_bad(&id);
        match odb.read_type(&id) {
            Some(kind) => kind as i32,
            None => OBJ_BAD,
        }
    }

    /// `packed_to_object_type()` (packfile.c:1080-1145): follow the delta chain from `obj_offset`,
    /// whose header said `kind` and ended at `curpos`, to the base's type.
    fn packed_to_object_type(&self, mut obj_offset: u64, mut kind: i32, mut curpos: u64, odb: &mut dyn Odb) -> i32 {
        let mut poi_stack: Vec<u64> = Vec::new();
        while kind == OBJ_OFS_DELTA || kind == OBJ_REF_DELTA {
            // Push the object we're going to leave behind.
            poi_stack.push(obj_offset);
            // If parsing the base offset fails, just unwind.
            let base_offset = self.get_delta_base(&mut curpos, kind, obj_offset);
            if base_offset == 0 {
                return self.unwind(poi_stack, odb);
            }
            curpos = base_offset;
            obj_offset = base_offset;
            (kind, _) = self.unpack_object_header(&mut curpos);
            if kind <= OBJ_NONE {
                // If getting the base itself fails, we first retry the base, otherwise unwind.
                kind = self.retry_bad_packed_offset(base_offset, odb);
                if kind > OBJ_NONE {
                    return kind;
                }
                return self.unwind(poi_stack, odb);
            }
        }
        match kind {
            OBJ_BAD | 1..=4 => kind,
            _ => {
                error(&format!(
                    "unknown object type {kind} at offset {obj_offset} in {}",
                    self.name
                ));
                OBJ_BAD
            }
        }
    }

    /// `packed_to_object_type()`'s `unwind:` label: retry every delta left behind, innermost first.
    fn unwind(&self, mut poi_stack: Vec<u64>, odb: &mut dyn Odb) -> i32 {
        while let Some(obj_offset) = poi_stack.pop() {
            let kind = self.retry_bad_packed_offset(obj_offset, odb);
            if kind > OBJ_NONE {
                return kind;
            }
        }
        OBJ_BAD
    }

    /// `get_size_from_delta()` (packfile.c:901-954): the size of the object the delta at `curpos`
    /// produces, `0` after `error("delta data unpack-initial failed")`.
    /// With `report` unset nothing is printed: see [`Self::read_header()`].
    fn get_size_from_delta(&self, curpos: u64, report: bool) -> usize {
        let mut delta_head = [0u8; 20];
        let (how, total_out) = self.inflate(curpos, &mut delta_head, report);
        if !matches!(how, Inflated::End) && total_out != delta_head.len() {
            if report {
                error("delta data unpack-initial failed");
            }
            return 0;
        }
        let mut at = 0usize;
        // Ignore base size.
        delta_hdr_size(&delta_head, &mut at);
        if at >= delta_head.len() {
            return 0;
        }
        // Read the result size.
        delta_hdr_size(&delta_head, &mut at)
    }

    /// `unpack_compressed_entry()` (packfile.c:1446-1488): inflate exactly `size` bytes at `curpos`.
    fn unpack_compressed_entry(&self, curpos: u64, size: usize) -> Option<Vec<u8>> {
        // `stream.avail_out = size + 1`: one spare byte catches a payload longer than its header says.
        let mut buffer = vec![0u8; size + 1];
        match self.inflate(curpos, &mut buffer, true) {
            (Inflated::End, total_out) if total_out == size => {
                buffer.truncate(size);
                Some(buffer)
            }
            // The payload is larger than it should be, or ended early.
            (Inflated::End | Inflated::OutputFull | Inflated::Failed, _) => None,
            // `git_inflate()` returned `Z_BUF_ERROR` with nothing consumed: the loop goes on to ask
            // `use_pack()` for the bytes past the end of the window, which dies.
            (Inflated::NeedsInput, _) => {
                self.use_pack(self.data.len() as u64);
                None
            }
        }
    }

    /// `unpack_entry()` (packfile.c:1503-1720): the object at `obj_offset` with its type, or `None`
    /// once every recovery failed.
    fn unpack_entry(&self, obj_offset: u64, odb: &mut dyn Odb) -> Option<(Kind, Vec<u8>)> {
        let mut obj_offset = obj_offset;
        let mut curpos = obj_offset;
        // (obj_offset, curpos past the base reference, delta size)
        let mut delta_stack: Vec<(u64, u64, usize)> = Vec::new();

        // PHASE 1: drill down to the innermost base object.
        let (mut kind, mut size);
        let mut failed_reference = false;
        loop {
            (kind, size) = self.unpack_object_header(&mut curpos);
            if kind != OBJ_OFS_DELTA && kind != OBJ_REF_DELTA {
                break;
            }
            let base_offset = self.get_delta_base(&mut curpos, kind, obj_offset);
            if base_offset == 0 {
                error(&format!(
                    "failed to validate delta base reference at offset {curpos} from {}",
                    self.name
                ));
                // Bail to phase 2, in hopes of recovery.
                failed_reference = true;
                break;
            }
            // Push object, proceed to base.
            delta_stack.push((obj_offset, curpos, size));
            curpos = base_offset;
            obj_offset = base_offset;
        }

        // PHASE 2: handle the base.
        let mut data = match kind {
            OBJ_OFS_DELTA | OBJ_REF_DELTA if failed_reference => None,
            1..=4 => self.unpack_compressed_entry(curpos, size),
            _ => {
                error(&format!(
                    "unknown object type {kind} at offset {obj_offset} in {}",
                    self.name
                ));
                None
            }
        };
        let mut final_kind = kind_of(kind);

        // PHASE 3: apply deltas in order.
        while let Some((delta_offset, delta_curpos, delta_size)) = delta_stack.pop() {
            let mut base = data.take();
            if base.is_none() {
                // "We're probably in deep shit, but let's try to fetch the required base anyway from
                // another pack or loose."
                if let Some(base_oid) = self.oid_at_offset(obj_offset) {
                    error(&format!(
                        "failed to read delta base object {base_oid} at offset {obj_offset} from {}",
                        self.name
                    ));
                    odb.mark_bad(&base_oid);
                    base = odb.read_object(&base_oid).map(|(k, d)| {
                        final_kind = Some(k);
                        d
                    });
                }
            }
            obj_offset = delta_offset;
            let Some(base) = base else { continue };
            match self.unpack_compressed_entry(delta_curpos, delta_size) {
                None => error(&format!(
                    "failed to unpack compressed delta at offset {delta_curpos} from {}",
                    self.name
                )),
                Some(delta) => {
                    data = patch_delta(&base, &delta);
                    // We could not apply the delta; warn the user, but keep going.
                    if data.is_none() {
                        error("failed to apply delta");
                    }
                }
            }
        }
        Some((final_kind?, data?))
    }

    /// `packed_object_info()` with `typep`, `sizep` and `contentp` (packfile.c:1308-1441), the read
    /// `odb_read_object()` makes: the object, or `None` for the `-1` that marks it bad.
    pub(crate) fn read_object(&self, obj_offset: u64, odb: &mut dyn Odb) -> Option<(Kind, Vec<u8>)> {
        // `cache_or_unpack_entry()`; a `NULL` there is `OBJ_BAD`, which `packed_to_object_type()`
        // hands back without a line of its own.
        self.unpack_entry(obj_offset, odb)
    }

    /// `packed_object_info()` with `typep` alone (packfile.c:1308-1441), the read
    /// `odb_read_object_info(r, oid, NULL)` makes: the type, or `None` for `-1`. A header carries a
    /// size too; it is the one `sizep` would get, worked out without a line of output, since git asked
    /// for none.
    pub(crate) fn read_header(&self, obj_offset: u64, odb: &mut dyn Odb) -> Option<(Kind, u64)> {
        let mut curpos = obj_offset;
        let (kind, size) = self.unpack_object_header(&mut curpos);
        let ptot = self.packed_to_object_type(obj_offset, kind, curpos, odb);
        let kind_out = kind_of(ptot)?;
        Some((
            kind_out,
            self.size_of(obj_offset, kind, size, curpos, false).unwrap_or(0),
        ))
    }

    /// `packed_object_info()` with `sizep` alone (packfile.c:1308-1350), as `cat-file -s` asks: the
    /// size, without ever checking the type.
    pub(crate) fn read_size(&self, obj_offset: u64) -> Option<u64> {
        let mut curpos = obj_offset;
        let (kind, size) = self.unpack_object_header(&mut curpos);
        self.size_of(obj_offset, kind, size, curpos, true)
    }

    /// `packed_object_info()`'s `sizep` branch: a delta's size is the size it produces.
    fn size_of(&self, obj_offset: u64, kind: i32, size: usize, curpos: u64, report: bool) -> Option<u64> {
        if kind != OBJ_OFS_DELTA && kind != OBJ_REF_DELTA {
            return Some(size as u64);
        }
        let mut tmp_pos = curpos;
        if self.get_delta_base(&mut tmp_pos, kind, obj_offset) == 0 {
            return None;
        }
        match self.get_size_from_delta(tmp_pos, report) {
            0 => None,
            size => Some(size as u64),
        }
    }
}
