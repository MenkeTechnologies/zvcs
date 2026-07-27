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
/// `BITMAP_OPT_PSEUDO_MERGES`: a section of bitmaps that each stand for a *set*
/// of commits rather than one, so a reader that has already reached all of them
/// can take their whole reachable set in a single OR.
const OPT_PSEUDO_MERGES: u16 = 0x20;

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

/// One pseudo-merge; git's `bitmapped_commit` with its `pseudo_merge` bit set.
///
/// A pseudo-merge stands in for a synthetic merge commit whose parents are a
/// batch of ref tips that were not worth a bitmap each. A reader that finds it
/// has already reached every one of `parents` may OR `reachable` in whole and
/// stop walking, which is the entire point of the section.
///
/// Both bitmaps address *pack* positions, unlike [`Commit::index_position`] —
/// git reaches them through `find_object_pos()`, which is `oe_in_pack_pos()`,
/// while entry headers go through `oid_pos()` on the index. Storing index
/// positions here produces a file that decodes and answers wrongly.
#[derive(Debug, Clone)]
pub struct PseudoMerge {
    /// The commits this merge stands for, as a plain bitmap over pack
    /// positions. A reader ORs `reachable` in exactly when all of these bits
    /// are already set in the result it is building.
    pub parents: Vec<u64>,
    /// Every object all of `parents` reach between them, unioned, again over
    /// pack positions.
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
    pseudo_merges: &[PseudoMerge],
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
    // git's `bitmap_writer_finish()` sets this from the writer's state rather
    // than from options: the section exists exactly when something was selected
    // for it.
    if !pseudo_merges.is_empty() {
        flags |= OPT_PSEUDO_MERGES;
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

    // Order matters and is git's, in `bitmap_writer_finish()`: entries, then
    // pseudo-merges, then the lookup table, then the hash cache. A reader peels
    // those three trailing sections off the end in the reverse order, so moving
    // any of them makes every one before it unreadable.
    if !pseudo_merges.is_empty() {
        write_pseudo_merges(pseudo_merges, &mut out);
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

/// git's `write_pseudo_merges()`: the whole optional pseudo-merge section, laid
/// out as `Documentation/technical/bitmap-format.adoc` specifies.
///
/// Five runs, in this order:
///
/// 1. the merges themselves, each a `parents` bitmap followed by a `reachable`
///    one, whose byte offsets everything below points back at;
/// 2. a fixed-width lookup table, one twelve-byte `(pack position, offset)` row
///    per commit that is a parent of *any* merge, ascending by position so a
///    reader can binary-search it;
/// 3. an extended table, present only for the commits that are a parent of more
///    than one merge, which the row above points at with its top bit set;
/// 4. the offset of every merge, so a reader can enumerate them without the
///    lookup table;
/// 5. a twenty-four byte trailer naming the two counts and locating both the
///    lookup table and the section itself.
///
/// The commit-to-merges index git keeps alongside the writer is derived here
/// from the `parents` bitmaps instead, which holds the same commits: git adds a
/// commit to that map in `select_pseudo_merges_1()` at the same moment it
/// appends it as a parent, and never one without the other.
fn write_pseudo_merges(merges: &[PseudoMerge], out: &mut Vec<u8>) {
    // Both the section's internal offsets and its trailer are relative to here,
    // not to the merges, so a reader can find one from the other.
    let start = out.len();

    let mut merge_offset: Vec<u64> = Vec::with_capacity(merges.len());
    for merge in merges {
        merge_offset.push(out.len() as u64);
        ewah::Builder::from_bitmap_words(&merge.parents).write_to(out);
        ewah::Builder::from_bitmap_words(&merge.reachable).write_to(out);
    }

    // git's `pseudo_merge_commits`, keyed by pack position rather than object
    // id. A `BTreeMap` supplies the ascending order git gets from sorting the
    // keys by `find_object_pos()`, and pushing in merge order supplies the
    // ascending merge numbering git gets from `writer->pseudo_merges_nr`.
    let mut commits: std::collections::BTreeMap<u32, Vec<usize>> = std::collections::BTreeMap::new();
    for (at, merge) in merges.iter().enumerate() {
        for (word, bits) in merge.parents.iter().enumerate() {
            let mut bits = *bits;
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                commits.entry((word * 64) as u32 + bit).or_default().push(at);
            }
        }
    }

    // Where the first extended entry will land, which the fixed-width rows have
    // to point at before any of them has been written.
    let mut next_ext = out.len() + commits.len() * (4 + 8);
    let table_start = out.len();

    for (position, of) in &commits {
        out.extend_from_slice(&position.to_be_bytes());
        if of.len() == 1 {
            out.extend_from_slice(&merge_offset[of[0]].to_be_bytes());
        } else {
            // git dies with "too many pseudo-merges" when this offset would
            // collide with the flag bit. That needs a `.bitmap` of eight
            // exabytes, which cannot be reached by a `Vec` in memory.
            debug_assert_eq!(next_ext as u64 & 1 << 63, 0, "offset collides with the flag bit");
            out.extend_from_slice(&(next_ext as u64 | 1 << 63).to_be_bytes());
            next_ext += 4 + of.len() * 8;
        }
    }

    for of in commits.values().filter(|of| of.len() > 1) {
        out.extend_from_slice(&(of.len() as u32).to_be_bytes());
        for &at in of {
            out.extend_from_slice(&merge_offset[at].to_be_bytes());
        }
    }

    for offset in &merge_offset {
        out.extend_from_slice(&offset.to_be_bytes());
    }

    out.extend_from_slice(&(merges.len() as u32).to_be_bytes());
    out.extend_from_slice(&(commits.len() as u32).to_be_bytes());
    out.extend_from_slice(&((table_start - start) as u64).to_be_bytes());
    // Counts itself, which is how a reader that has only reached the end of the
    // section can step back to its start.
    let section_len = (out.len() - start + 8) as u64;
    out.extend_from_slice(&section_len.to_be_bytes());
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
    use super::{Commit, Options, PseudoMerge, write};
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
            &[],
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
            &[],
            Options::default(),
        )
        .expect("hashing cannot fail");
        let with_cache = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            &[],
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
            &[],
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

    /// Walk the pseudo-merge section back from the end exactly as git's
    /// `load_bitmap_header()` does — trailer first, then the tables it locates —
    /// and hand back what a reader would find.
    ///
    /// Returns the section start, the number of merges, the number of commits,
    /// the merge offsets from the position table, and the lookup table rows.
    fn read_pseudo_merges(bytes: &[u8], trailing: usize) -> (usize, u32, u32, Vec<u64>, Vec<(u32, u64)>) {
        let end = bytes.len() - 20 - trailing;
        let be64 = |at: usize| u64::from_be_bytes(bytes[at..at + 8].try_into().expect("8 bytes"));
        let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));

        let section_len = be64(end - 8) as usize;
        let start = end - section_len;
        let lookup_at = start + be64(end - 16) as usize;
        let commits_nr = be32(end - 20);
        let merges_nr = be32(end - 24);

        let positions_at = end - 24 - merges_nr as usize * 8;
        let offsets = (0..merges_nr as usize).map(|at| be64(positions_at + at * 8)).collect();
        let lookup = (0..commits_nr as usize)
            .map(|at| (be32(lookup_at + at * 12), be64(lookup_at + at * 12 + 4)))
            .collect();
        (start, merges_nr, commits_nr, offsets, lookup)
    }

    /// Bit positions set in a plain word array, ascending.
    fn set_bits(words: &[u64]) -> Vec<u32> {
        let mut out = Vec::new();
        for (word, bits) in words.iter().enumerate() {
            for bit in 0..64u32 {
                if bits & (1 << bit) != 0 {
                    out.push((word * 64) as u32 + bit);
                }
            }
        }
        out
    }

    fn merges() -> Vec<PseudoMerge> {
        // Positions 3 and 4 belong to one merge each; position 9 belongs to
        // both, which is the only thing that makes an extended table appear.
        vec![
            PseudoMerge {
                parents: vec![(1 << 3) | (1 << 9), 0],
                reachable: vec![u64::MAX, 0b1111],
            },
            PseudoMerge {
                parents: vec![(1 << 4) | (1 << 9), 0],
                reachable: vec![0xff, 1 << 40],
            },
        ]
    }

    #[test]
    fn the_section_is_announced_and_costs_nothing_when_empty() {
        let flags = |merges: &[PseudoMerge]| {
            let bytes = write(
                gix_hash::Kind::Sha1,
                &checksum(),
                &kinds(),
                &vec![9u32; 200],
                selection(),
                merges,
                Options::default(),
            )
            .expect("hashing cannot fail");
            (u16::from_be_bytes([bytes[6], bytes[7]]), bytes.len())
        };
        let (bare_flags, bare_len) = flags(&[]);
        let (with_flags, with_len) = flags(&merges());
        assert_eq!(bare_flags & 0x20, 0, "no section, no flag");
        assert_eq!(with_flags & 0x20, 0x20, "BITMAP_OPT_PSEUDO_MERGES");
        assert!(with_len > bare_len, "the section is not free");
    }

    #[test]
    fn a_reader_can_find_every_merge_from_the_trailer() {
        for (name, options) in [
            ("bare", Options::default()),
            (
                "behind both trailing sections",
                Options {
                    hash_cache: true,
                    lookup_table: true,
                },
            ),
        ] {
            let bytes = write(
                gix_hash::Kind::Sha1,
                &checksum(),
                &kinds(),
                &vec![9u32; 200],
                selection(),
                &merges(),
                options,
            )
            .expect("hashing cannot fail");
            // What `load_bitmap_header()` has already peeled off the end before
            // it reaches the pseudo-merge trailer.
            let trailing = usize::from(options.hash_cache) * 200 * 4 + usize::from(options.lookup_table) * 3 * 16;
            let (start, merges_nr, commits_nr, offsets, lookup) = read_pseudo_merges(&bytes, trailing);

            assert_eq!(merges_nr, 2, "{name}: one per merge handed in");
            assert_eq!(commits_nr, 3, "{name}: positions 3, 4 and 9 across both merges");
            assert!(start > 12 + 20, "{name}: the section starts after the header");
            assert_eq!(offsets.len(), 2, "{name}");
            assert!(
                offsets.windows(2).all(|pair| pair[0] < pair[1]),
                "{name}: merges are written in order"
            );
            assert!(
                offsets[0] as usize >= start && (*offsets.last().expect("two merges") as usize) < start + 4096,
                "{name}: offsets are relative to the whole file, not the section"
            );

            assert_eq!(
                lookup.iter().map(|row| row.0).collect::<Vec<_>>(),
                vec![3, 4, 9],
                "{name}: rows ascend by pack position so a reader can bisect"
            );
            let flag = 1u64 << 63;
            assert_eq!(lookup[0].1, offsets[0], "{name}: position 3 is in the first merge only");
            assert_eq!(lookup[1].1, offsets[1], "{name}: position 4 is in the second merge only");
            assert_ne!(lookup[2].1 & flag, 0, "{name}: position 9 is in both, so it is extended");

            // Follow the extended row the way `pseudo_merge_ext_at()` does.
            let at = (lookup[2].1 & !flag) as usize;
            let count = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
            assert_eq!(count, 2, "{name}: two merges hold position 9");
            let extended: Vec<u64> = (0..count as usize)
                .map(|n| {
                    let at = at + 4 + n * 8;
                    u64::from_be_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
                })
                .collect();
            assert_eq!(extended, offsets, "{name}: and it points at both of them");
        }
    }

    #[test]
    fn each_merge_stores_its_parents_then_what_they_reach() {
        let merges = merges();
        let bytes = write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; 200],
            selection(),
            &merges,
            Options::default(),
        )
        .expect("hashing cannot fail");
        let (_, _, _, offsets, _) = read_pseudo_merges(&bytes, 0);

        for (merge, offset) in merges.iter().zip(&offsets) {
            let mut at = *offset as usize;
            for (expected, which) in [(&merge.parents, "parents"), (&merge.reachable, "reachable")] {
                let (decoded, _) = gix_bitmap::ewah::decode(&bytes[at..]).expect("the writer's own output decodes");
                let mut got = Vec::new();
                decoded.for_each_set_bit(|bit| {
                    got.push(bit as u32);
                    Some(())
                });
                assert_eq!(got, set_bits(expected), "{which} round-trips");
                let words = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
                at += 8 + words * 8 + 4;
            }
        }
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
            &[],
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
