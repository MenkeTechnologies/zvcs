//! `pack-check.c`'s `verify_pack()`, reduced to what `fsck --full` observes: the
//! `error:` lines, in git's order, and which objects came back. The
//! `unpack_entry()` (packfile.c) and `patch_delta()` (patch-delta.c) it drives
//! are the object database's own ([`gix::odb::store::verify_unpack_entry`]),
//! whose lines are collected here so they print where git prints them.
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
    /// Each pack `is_pack_valid()` refused, by its `.pack` path, with the range of
    /// [`Self::lines`] its `verify_pack()` printed: all of them ahead of the first object
    /// of the pack, since the walk never starts (pack-check.c:69-70).
    pub refused: Vec<(std::path::PathBuf, std::ops::Range<usize>)>,
}

impl PackCheck {
    /// One of `verify_packfile()`'s own `err = error(...)` lines, which make it fail; the lines
    /// `unpack_entry()` prints on the way are not (pack-check.c:52-181).
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
    pack_path: &std::path::Path,
    out: &mut PackCheck,
) {
    let hash = repo.object_hash();
    let rawsz = hash.len_in_bytes();
    let first_line = out.lines.len();

    // `verify_pack_index()`: `hashfile_checksum_valid()` over the `.idx`.
    if idx_bytes.len() >= rawsz {
        let (body, trailer) = idx_bytes.split_at(idx_bytes.len() - rawsz);
        if digest(hash, body).as_deref() != Some(trailer) {
            out.error(format!("Packfile index for {pack_name} hash mismatch"));
        }
    }

    // `verify_packfile()` opens the pack through `is_pack_valid()` before it reads a byte of it
    // (pack-check.c:69-70), whose own line comes first.
    let (valid, lines) =
        gix::odb::store::capture_errors(|| repo.objects.store_ref().is_pack_valid(pack_path, Some(index)));
    out.lines.extend(lines);
    if !valid || pack_bytes.len() < rawsz {
        out.error(format!("packfile {pack_name} cannot be accessed"));
        // `verify_packfile()` returns before its walk (pack-check.c:69-70), so not one of the
        // pack's objects reaches `fsck_obj_buffer()` and none of them gets `HAS_OBJ`.
        out.unverified.extend((0..index.num_objects()).map(|nr| index.oid_at_index(nr).to_owned()));
        out.refused.push((pack_path.to_owned(), first_line..out.lines.len()));
        return;
    }

    // `verify_packfile()`'s trailer checks (pack-check.c:73-94).
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
        let mut recovery = Recovery {
            repo,
            pack_path,
            bad: &mut out.bad,
        };
        let (data, lines) = gix::odb::store::capture_errors(|| {
            gix::odb::store::verify_unpack_entry(pack_bytes, pack_name, index, hash, offset, &mut recovery)
        });
        out.lines.extend(lines);
        match data {
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

/// What `unpack_entry()` asks of the rest of the object database while it reads a pack under
/// check: the delta bases it marks bad are kept in [`PackCheck::bad`] as well as in the object
/// database's own `bad_objects`, and a base it could not read is read from any other copy.
struct Recovery<'a> {
    repo: &'a gix::Repository,
    /// The `.pack` file, which names this pack to the object database's `bad_objects`.
    pack_path: &'a std::path::Path,
    bad: &'a mut HashSet<ObjectId>,
}

impl gix::odb::store::PackedReadOdb for Recovery<'_> {
    fn read_type(&mut self, id: &gix::hash::oid) -> Option<Kind> {
        self.repo.find_header(id).ok().map(|hdr| hdr.kind())
    }

    fn read_object(&mut self, id: &gix::hash::oid) -> Option<(Kind, Vec<u8>)> {
        // `odb_read_object_info_extended()` with no flags, which does not die on a corrupt answer
        // (packfile.c:1617-1628).
        let object = gix::odb::store::find::with_corrupt_reads_as_errors(|| self.repo.find_object(id)).ok()?;
        Some((object.kind, object.data.clone()))
    }

    fn mark_bad(&mut self, id: &gix::hash::oid) {
        self.bad.insert(id.to_owned());
        self.repo.objects.store_ref().mark_bad_packed_object(self.pack_path, id);
    }
}

/// `patch_delta()` (patch-delta.c:15-96): the delta applied to `base`, or
/// `Err` for a delta that does not fit it, carrying the one `error()` git
/// prints for it, if any. Shared with `unpack-objects`.
pub(super) fn patch_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, Option<&'static str>> {
    gix::odb::store::patch_delta_quiet(base, delta)
}
