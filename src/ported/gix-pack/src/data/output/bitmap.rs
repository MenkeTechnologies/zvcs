//! Writing a pack's reachability bitmap index (`.bitmap`), ported from git
//! 2.55.0 `pack-bitmap-write.c`.
//!
//! # What the file says
//!
//! A `.bitmap` is a cache of "which objects in this pack are reachable from
//! commit X", stored as EWAH bitmaps over *pack positions* — the position an
//! object has when the pack is read in offset order, which is what a `.rev`
//! reverse index resolves. It opens with four bitmaps naming the commits, the
//! trees, the blobs and the tags in the pack, then carries one entry per
//! selected commit.
//!
//! An entry names its commit by *index position* — where the commit sorts in
//! the `.idx`, which is object-id order — because that is the only lookup a
//! reader has before it knows anything else about the pack. The two coordinate
//! systems are why this function takes both orders, and mixing them up produces
//! a file that decodes without error and answers every question wrongly, so the
//! parameter names say which is which.
//!
//! # What this half does and does not decide
//!
//! Everything here is format: the header, the four type bitmaps, the XOR
//! chaining between entries, the optional lookup table and hash cache, and the
//! trailing checksum. Which commits are worth an entry, and which objects each
//! one reaches, is decided by the caller, which is the half that has a
//! repository to walk.
//!
//! # Deliberate departure from git
//!
//! git XORs two entries through `ewah_xor()`, on the compressed streams. This
//! XORs the plain word arrays the caller already holds and re-compresses, which
//! is the same bits but can drop trailing all-zero words git would have kept.
//! A reader XOR-ing an entry back against its base recovers the same bitmap
//! either way; the only effect is that a candidate is occasionally judged one
//! word smaller here than git would judge it, so the XOR base chosen can
//! differ.

use gix_bitmap::ewah;

/// git's `BITMAP_IDX_SIGNATURE`.
const SIGNATURE: &[u8; 4] = b"BITM";
/// The only format version git has ever written.
const VERSION: u16 = 1;
/// `BITMAP_OPT_FULL_DAG`, always set: the bitmaps cover the whole history of
/// each selected commit rather than a slice of it.
const OPT_FULL_DAG: u16 = 0x1;
/// `BITMAP_OPT_HASH_CACHE`: a trailing table of name hashes, one per object.
const OPT_HASH_CACHE: u16 = 0x4;
/// `BITMAP_OPT_LOOKUP_TABLE`: a trailing table letting a reader find one
/// commit's bitmap without decoding every entry before it.
const OPT_LOOKUP_TABLE: u16 = 0x10;

/// How far back `compute_xor_offsets()` looks for a bitmap to XOR against,
/// git's `MAX_XOR_OFFSET_SEARCH`.
const MAX_XOR_OFFSET_SEARCH: usize = 10;

/// Which optional sections the file carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// `pack.writeBitmapHashCache`: append one `pack_name_hash()` per object,
    /// in index order, which lets a reader group objects by likely path without
    /// reading any of them.
    pub hash_cache: bool,
    /// `pack.writeBitmapLookupTable`: append a table sorted by commit position
    /// so a reader can binary-search to one entry instead of decoding the
    /// entries ahead of it.
    pub lookup_table: bool,
}

/// One commit that gets a bitmap; git's `struct bitmapped_commit`.
#[derive(Debug, Clone)]
pub struct Commit {
    /// Where the commit sorts in the pack's `.idx`, i.e. among the object ids
    /// in ascending order. This is what the entry header stores.
    pub index_position: u32,
    /// The commit's committer time, which orders the entries and therefore
    /// decides which of them can serve as an XOR base for which.
    pub date: i64,
    /// Every object reachable from this commit, as a plain bitmap over *pack*
    /// positions, one bit per object with the low bit of word 0 being pack
    /// position zero.
    pub reachable: Vec<u64>,
}

/// Assemble the `.bitmap` for a pack.
///
/// `kinds` is indexed by pack position and `name_hashes` by index position;
/// `pack_checksum` is the hash trailing the `.pack` the file belongs to, which
/// a reader compares before trusting any of this.
///
/// Returns the complete file including its own trailing checksum.
pub fn write(
    object_hash: gix_hash::Kind,
    pack_checksum: &gix_hash::oid,
    kinds: &[gix_object::Kind],
    name_hashes: &[u32],
    mut selected: Vec<Commit>,
    options: Options,
) -> Result<Vec<u8>, gix_hash::hasher::Error> {
    let mut out = Vec::new();

    // The four type bitmaps, git's `bitmap_writer_build_type_index()`. One pass
    // in pack order, so every `set()` moves forwards, which is the only
    // direction an EWAH can be built in.
    let (mut commits, mut trees, mut blobs, mut tags) = (
        ewah::Builder::new(),
        ewah::Builder::new(),
        ewah::Builder::new(),
        ewah::Builder::new(),
    );
    for (at, kind) in kinds.iter().enumerate() {
        match kind {
            gix_object::Kind::Commit => commits.set(at),
            gix_object::Kind::Tree => trees.set(at),
            gix_object::Kind::Blob => blobs.set(at),
            gix_object::Kind::Tag => tags.set(at),
        }
    }

    // git's `compute_xor_offsets()`: order the entries by date so that
    // neighbouring bitmaps are likely to be near-identical, then store each one
    // as its difference from whichever of the last ten compresses best.
    if selected.len() > 1 {
        selected.sort_by_key(|commit| commit.date);
    }
    let mut write_as: Vec<ewah::Builder> = Vec::with_capacity(selected.len());
    let mut xor_offsets: Vec<u8> = Vec::with_capacity(selected.len());
    for at in 0..selected.len() {
        let mut best = ewah::Builder::from_bitmap_words(&selected[at].reachable);
        let mut best_offset = 0usize;
        for back in 1..=MAX_XOR_OFFSET_SEARCH {
            let Some(base) = at.checked_sub(back) else { break };
            let candidate =
                ewah::write::xor_of_bitmap_words(&selected[base].reachable, &selected[at].reachable);
            if candidate.word_count() < best.word_count() {
                best = candidate;
                best_offset = back;
            }
        }
        write_as.push(best);
        xor_offsets.push(best_offset as u8);
    }

    // Header. `entry_count` counts only the entries that follow, and the
    // checksum ties the file to one specific pack.
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&VERSION.to_be_bytes());
    let mut flags = OPT_FULL_DAG;
    if options.hash_cache {
        flags |= OPT_HASH_CACHE;
    }
    if options.lookup_table {
        flags |= OPT_LOOKUP_TABLE;
    }
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&(selected.len() as u32).to_be_bytes());
    out.extend_from_slice(pack_checksum.as_bytes());

    for bitmap in [&commits, &trees, &blobs, &tags] {
        bitmap.write_to(&mut out);
    }

    // The entries themselves, git's `write_selected_commits_v1()`. The lookup
    // table needs to point at each one, so their starts are recorded as we go.
    let mut offsets: Vec<u64> = Vec::with_capacity(selected.len());
    for (at, commit) in selected.iter().enumerate() {
        offsets.push(out.len() as u64);
        out.extend_from_slice(&commit.index_position.to_be_bytes());
        out.push(xor_offsets[at]);
        // git's `stored->flags`, which nothing sets any more.
        out.push(0);
        write_as[at].write_to(&mut out);
    }

    if options.lookup_table {
        write_lookup_table(&selected, &xor_offsets, &offsets, &mut out);
    }

    // git's `write_hash_cache()`: index order, so it lines up with the `.idx`
    // a reader already has open.
    if options.hash_cache {
        for hash in name_hashes {
            out.extend_from_slice(&hash.to_be_bytes());
        }
    }

    let mut hasher = gix_hash::hasher(object_hash);
    hasher.update(&out);
    out.extend_from_slice(hasher.try_finalize()?.as_slice());
    Ok(out)
}

/// git's `write_lookup_table()`: one `(commit position, offset, xor row)`
/// triplet per entry, sorted by commit position so a reader can binary-search
/// it.
///
/// `xor row` is the *row of the table*, not the entry, that holds the base an
/// entry was stored as a difference from, and `0xffffffff` marks an entry that
/// stands on its own.
fn write_lookup_table(selected: &[Commit], xor_offsets: &[u8], offsets: &[u64], out: &mut Vec<u8>) {
    let mut table: Vec<usize> = (0..selected.len()).collect();
    table.sort_by_key(|&at| selected[at].index_position);
    let mut table_inv = vec![0usize; selected.len()];
    for (row, &at) in table.iter().enumerate() {
        table_inv[at] = row;
    }

    for &at in &table {
        let xor_row = match xor_offsets[at] {
            0 => 0xffff_ffffu32,
            offset => table_inv[at - offset as usize] as u32,
        };
        out.extend_from_slice(&selected[at].index_position.to_be_bytes());
        out.extend_from_slice(&offsets[at].to_be_bytes());
        out.extend_from_slice(&xor_row.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::{Commit, Options, write};
    use gix_object::Kind;

    fn checksum() -> gix_hash::ObjectId {
        gix_hash::ObjectId::from_bytes_or_panic(&[7u8; 20])
    }

    /// Three commits whose reachability grows monotonically, which is the shape
    /// that makes XOR chaining pay off.
    fn selection() -> Vec<Commit> {
        (0..3u32)
            .map(|n| Commit {
                index_position: n,
                date: i64::from(n) * 100,
                reachable: {
                    let mut words = vec![0u64; 4];
                    for at in 0..=(n as usize * 20 + 10) {
                        words[at / 64] |= 1 << (at % 64);
                    }
                    words
                },
            })
            .collect()
    }

    fn kinds() -> Vec<Kind> {
        let mut out = vec![Kind::Blob; 200];
        out[0] = Kind::Commit;
        out[1] = Kind::Commit;
        out[2] = Kind::Commit;
        out[50] = Kind::Tree;
        out[199] = Kind::Tag;
        out
    }

    #[test]
    fn the_header_names_the_pack_and_the_sections_present() {
        let bytes = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            Options {
                hash_cache: true,
                lookup_table: true,
            },
        )
        .expect("hashing cannot fail");
        assert_eq!(&bytes[..4], b"BITM");
        assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 1, "format version");
        assert_eq!(
            u16::from_be_bytes([bytes[6], bytes[7]]),
            0x1 | 0x4 | 0x10,
            "full DAG, hash cache and lookup table"
        );
        assert_eq!(
            u32::from_be_bytes(bytes[8..12].try_into().expect("4 bytes")),
            3,
            "one entry per selected commit"
        );
        assert_eq!(&bytes[12..32], checksum().as_bytes(), "the pack it belongs to");
    }

    #[test]
    fn the_optional_sections_cost_exactly_what_they_should() {
        let bare = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            Options::default(),
        )
        .expect("hashing cannot fail");
        let with_cache = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            Options {
                hash_cache: true,
                lookup_table: false,
            },
        )
        .expect("hashing cannot fail");
        let with_table = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            Options {
                hash_cache: false,
                lookup_table: true,
            },
        )
        .expect("hashing cannot fail");
        assert_eq!(
            with_cache.len() - bare.len(),
            200 * 4,
            "the hash cache is four bytes per object in the pack"
        );
        assert_eq!(
            with_table.len() - bare.len(),
            3 * 16,
            "the lookup table is a sixteen-byte triplet per entry"
        );
    }

    #[test]
    fn entries_are_ordered_by_date_and_chained_backwards() {
        let mut shuffled = selection();
        shuffled.reverse();
        let bytes = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            shuffled,
            Options::default(),
        )
        .expect("hashing cannot fail");

        // Skip the header and the four type bitmaps to reach the first entry.
        let mut at = 12 + 20;
        for _ in 0..4 {
            let words = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
            at += 8 + words * 8 + 4;
        }
        let mut dates = Vec::new();
        for entry in 0..3usize {
            let position = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
            let xor_offset = bytes[at + 4];
            assert_eq!(bytes[at + 5], 0, "no entry flags are ever set");
            assert!(
                xor_offset as usize <= entry,
                "an entry can only be a difference from one written before it"
            );
            dates.push(position);
            let words = u32::from_be_bytes(bytes[at + 10..at + 14].try_into().expect("4 bytes")) as usize;
            at += 6 + 8 + words * 8 + 4;
        }
        assert_eq!(dates, vec![0, 1, 2], "the writer re-sorts entries into date order");
    }
}
