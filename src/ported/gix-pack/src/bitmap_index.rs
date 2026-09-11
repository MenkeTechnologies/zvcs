//! Reading a pack or multi-pack reachability bitmap (`.bitmap`), ported from
//! git 2.55.0 `pack-bitmap.c`.
//!
//! This is the inverse of [`data::output::bitmap`](crate::data::output::bitmap),
//! and reads the same file that half writes: a v1 header, the four type bitmaps
//! in commit/tree/blob/tag order, and one entry per bitmapped commit.
//!
//! # The two coordinate systems
//!
//! A bitmap's *bits* address pack positions — an object's place when the pack
//! is read in offset order — while an entry's *header* names its commit by
//! index position, where the commit sorts in the `.idx`, which is object-id
//! order. For a multi-pack bitmap the same split holds with the multi-pack
//! index's own orders: bits address pseudo-pack order, which the `RIDX` chunk
//! resolves, and headers address the lexicographic `OIDL` order. Nothing here
//! resolves either one — a caller that has the index does that — so both are
//! handed out as the plain `u32` they are on disk.
//!
//! # What is left on the floor
//!
//! The pseudo-merge extension (`BITMAP_OPT_PSEUDO_MERGES`) is located but not
//! decoded: nothing in this tree writes one, and every read this serves —
//! `rev-list --test-bitmap` — looks up a single commit, which the entries
//! answer on their own. The trailing name-hash cache is likewise located only,
//! since it is a pack-objects heuristic rather than reachability.

use gix_bitmap::ewah;

pub use gix_bitmap::ewah::write::bitmap_words_equal;

/// git's `BITMAP_IDX_SIGNATURE`.
const SIGNATURE: &[u8; 4] = b"BITM";
/// `BITMAP_OPT_FULL_DAG`, which git refuses to read a file without.
const OPT_FULL_DAG: u16 = 0x1;
/// `BITMAP_OPT_HASH_CACHE`: a trailing `u32` per object.
const OPT_HASH_CACHE: u16 = 0x4;
/// `BITMAP_OPT_LOOKUP_TABLE`: a trailing table of triplets, one per entry.
const OPT_LOOKUP_TABLE: u16 = 0x10;
/// `BITMAP_OPT_PSEUDO_MERGES`: a trailing table of merged-commit bitmaps.
const OPT_PSEUDO_MERGES: u16 = 0x20;

/// git's `BITMAP_LOOKUP_TABLE_TRIPLET_WIDTH` (pack-bitmap.h:35): a `u32` commit
/// position, a `u64` offset and a `u32` row of the entry to XOR against.
const LOOKUP_TABLE_TRIPLET_WIDTH: usize = 16;

/// git's `MAX_XOR_OFFSET` (pack-bitmap.c:381), the furthest back an entry may
/// point for its XOR base.
const MAX_XOR_OFFSET: u8 = 160;

/// The header of one bitmapped commit plus the bitmap that follows it.
///
/// The bitmap is stored as git wrote it: when `xor_offset` is non-zero it is
/// the XOR of the real bitmap with the one `xor_offset` entries earlier, and
/// [`File::bitmap_at()`] is what undoes that.
pub struct Entry {
    /// Where the commit sorts in the index's object-id order, git's
    /// `commit_idx_pos`.
    pub commit_index_pos: u32,
    /// How many entries back the XOR base sits, or zero for a bitmap stored
    /// whole.
    pub xor_offset: u8,
    /// git's `BITMAP_FLAG_REUSE` and nothing else so far.
    pub flags: u8,
    /// The stored — possibly XOR-ed — bitmap.
    pub bitmap: ewah::Builder,
}

/// One row of the lookup-table extension, git's
/// `struct bitmap_lookup_table_triplet`.
pub struct Triplet {
    /// The commit's position in object-id order.
    pub commit_pos: u32,
    /// Where the commit's entry starts in the file.
    pub offset: u64,
    /// The row holding the XOR base, or `0xffffffff` for none.
    pub xor_row: u32,
}

/// A parsed `.bitmap`.
pub struct File {
    /// Always 1; git refuses anything else.
    pub version: u16,
    /// The `pack_bitmap_opts` the writer recorded.
    pub flags: u16,
    /// How many commits this file carries bitmaps for.
    pub entry_count: u32,
    /// The checksum of the pack or multi-pack index this bitmap belongs to,
    /// which git verifies against the multi-pack index before trusting it.
    pub checksum: gix_hash::ObjectId,
    /// Which pack positions hold commits.
    pub commits: ewah::Builder,
    /// Which pack positions hold trees.
    pub trees: ewah::Builder,
    /// Which pack positions hold blobs.
    pub blobs: ewah::Builder,
    /// Which pack positions hold tags.
    pub tags: ewah::Builder,
    /// One per bitmapped commit, in the order the file stores them — empty when
    /// [`lookup_table`](File::lookup_table) is present, exactly as git skips
    /// `load_bitmap_entries_v1()` in that case.
    pub entries: Vec<Entry>,
    /// The lookup-table extension, when the writer emitted one.
    pub lookup_table: Option<Vec<Triplet>>,
    /// The whole file, kept for the lookup-table path, which seeks to an entry
    /// rather than reading them in order — git keeps the mmap for the same
    /// reason.
    data: Vec<u8>,
}

///
pub mod decode {
    /// The error returned by [`File::at()`](super::File::at()).
    ///
    /// Every variant carries the message git's `error()` prints before
    /// `prepare_bitmap_git()` gives up, because the caller reproduces both
    /// lines.
    #[derive(Debug, Clone, thiserror::Error)]
    #[allow(missing_docs)]
    pub enum Error {
        /// The file could not be read at all, which git reports through
        /// `git_open()`'s caller rather than as a bitmap problem.
        #[error("{0}")]
        Io(String),
        /// git calls `BUG()` here rather than `error()`, which aborts the
        /// process instead of reporting a corrupt file.
        #[error("BUG: pack-bitmap.c:270: unsupported options for bitmap index file (Git requires BITMAP_OPT_FULL_DAG)")]
        MissingFullDag,
        #[error("corrupted bitmap index (too small)")]
        TooSmall,
        #[error("corrupted bitmap index file (wrong header)")]
        WrongHeader,
        #[error("unsupported version '{version}' for bitmap index file")]
        UnsupportedVersion { version: u16 },
        #[error("corrupted bitmap index file (too short to fit hash cache)")]
        ShortHashCache,
        #[error("corrupted bitmap index file (too short to fit lookup table)")]
        ShortLookupTable,
        #[error("corrupted bitmap index file (too short to fit pseudo-merge table header)")]
        ShortPseudoMergeHeader,
        #[error("corrupted bitmap index file (too short to fit pseudo-merge table)")]
        ShortPseudoMergeTable,
        #[error("{detail}")]
        CorruptEwah { detail: String },
        #[error("corrupt ewah bitmap: truncated header for entry {index}")]
        TruncatedEntryHeader { index: u32 },
        #[error("corrupt ewah bitmap: truncated header for bitmap at offset {offset}")]
        TruncatedLookupEntry { offset: u64 },
        #[error("corrupt ewah bitmap: commit index {position} out of range")]
        CommitIndexOutOfRange { position: u32 },
        #[error("corrupted bitmap pack index")]
        BadXorOffset,
        #[error("invalid XOR offset in bitmap pack index")]
        MissingXorBase,
    }

    impl Error {
        /// True when git reports this through `BUG()`, which prints one line
        /// naming the C source position and then aborts — a shell reports the
        /// `SIGABRT` as 134 — rather than printing `error:` and giving up.
        pub fn is_bug(&self) -> bool {
            matches!(self, Error::MissingFullDag)
        }

        /// The `error:` lines git prints for this, in the order it prints them.
        ///
        /// git calls `error()` at every level that notices, so a bad EWAH stream
        /// reports twice: once from `ewah_read_mmap()` saying what was short, and
        /// once from `read_bitmap()` (pack-bitmap.c:180-184) saying the index
        /// could not be loaded.
        pub fn error_lines(&self) -> Vec<String> {
            match self {
                Error::CorruptEwah { detail } => {
                    vec![detail.clone(), "failed to load bitmap index (corrupted?)".into()]
                }
                other => vec![other.to_string()],
            }
        }
    }
}

impl File {
    /// Parse the `.bitmap` at `path`, which belongs to an index of
    /// `num_objects` objects hashed with `object_hash`.
    ///
    /// git works off an mmap and keeps `map_pos` as it goes
    /// (`load_bitmap_header()` then `load_bitmap()`, pack-bitmap.c:245-334 and
    /// :650-681); this reads the file once and walks the same positions.
    pub fn at(
        path: &std::path::Path,
        object_hash: gix_hash::Kind,
        num_objects: u32,
    ) -> Result<Self, decode::Error> {
        let data = std::fs::read(path).map_err(|err| decode::Error::Io(err.to_string()))?;
        Self::from_bytes(&data, object_hash, num_objects)
    }

    /// [`at()`](File::at()) over bytes already in hand.
    pub fn from_bytes(
        data: &[u8],
        object_hash: gix_hash::Kind,
        num_objects: u32,
    ) -> Result<Self, decode::Error> {
        let hash_len = object_hash.len_in_bytes();
        // `sizeof(*header) - GIT_MAX_RAWSZ + hash_algo->rawsz` (:250).
        let header_size = 12 + hash_len;

        if data.len() < header_size + hash_len {
            return Err(decode::Error::TooSmall);
        }
        if &data[..4] != SIGNATURE {
            return Err(decode::Error::WrongHeader);
        }

        let version = u16::from_be_bytes([data[4], data[5]]);
        if version != 1 {
            return Err(decode::Error::UnsupportedVersion { version });
        }

        let flags = u16::from_be_bytes([data[6], data[7]]);
        let entry_count = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        let checksum = gix_hash::ObjectId::from_bytes_or_panic(&data[12..12 + hash_len]);

        // git's `BUG("unsupported options for bitmap index file")` at :268-270,
        // which is an abort rather than a rejection — the caller reports it as
        // one.
        if flags & OPT_FULL_DAG == 0 {
            return Err(decode::Error::MissingFullDag);
        }

        // The extensions are located from the end, in the order git peels them
        // off (:262-328), each one shrinking `index_end` for the next.
        let mut index_end = data.len() - hash_len;
        let body_end = |index_end: usize| index_end.saturating_sub(header_size);

        if flags & OPT_HASH_CACHE != 0 {
            let cache_size = (num_objects as usize) * std::mem::size_of::<u32>();
            if cache_size > body_end(index_end) {
                return Err(decode::Error::ShortHashCache);
            }
            index_end -= cache_size;
        }

        let mut lookup_table = None;
        if flags & OPT_LOOKUP_TABLE != 0 {
            let table_size = (entry_count as usize) * LOOKUP_TABLE_TRIPLET_WIDTH;
            if table_size > body_end(index_end) {
                return Err(decode::Error::ShortLookupTable);
            }
            let table = &data[index_end - table_size..index_end];
            lookup_table = Some(
                table
                    .chunks_exact(LOOKUP_TABLE_TRIPLET_WIDTH)
                    .map(|row| Triplet {
                        commit_pos: u32::from_be_bytes(row[..4].try_into().expect("4 bytes")),
                        offset: u64::from_be_bytes(row[4..12].try_into().expect("8 bytes")),
                        xor_row: u32::from_be_bytes(row[12..].try_into().expect("4 bytes")),
                    })
                    .collect(),
            );
            index_end -= table_size;
        }

        if flags & OPT_PSEUDO_MERGES != 0 {
            if std::mem::size_of::<usize>() > body_end(index_end) {
                return Err(decode::Error::ShortPseudoMergeHeader);
            }
            let table_size = u64::from_be_bytes(
                data[index_end - 8..index_end].try_into().expect("8 bytes"),
            ) as usize;
            if table_size > body_end(index_end) {
                return Err(decode::Error::ShortPseudoMergeTable);
            }
            index_end -= table_size;
        }

        let _ = index_end;

        let mut pos = header_size;
        let commits = read_bitmap(data, &mut pos)?;
        let trees = read_bitmap(data, &mut pos)?;
        let blobs = read_bitmap(data, &mut pos)?;
        let tags = read_bitmap(data, &mut pos)?;

        // "if (!bitmap_git->table_lookup && load_bitmap_entries_v1(...))" (:667).
        let mut entries = Vec::new();
        if lookup_table.is_none() {
            entries.reserve(entry_count as usize);
            for index in 0..entry_count {
                if data.len() - pos < 6 {
                    return Err(decode::Error::TruncatedEntryHeader { index });
                }
                let commit_index_pos =
                    u32::from_be_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
                let xor_offset = data[pos + 4];
                let flags = data[pos + 5];
                pos += 6;

                // `nth_bitmap_object_oid()` failing, which is how git notices an
                // entry naming a commit the index does not have (:413-415).
                if commit_index_pos >= num_objects {
                    return Err(decode::Error::CommitIndexOutOfRange {
                        position: commit_index_pos,
                    });
                }

                if xor_offset > MAX_XOR_OFFSET || u32::from(xor_offset) > index {
                    return Err(decode::Error::BadXorOffset);
                }
                // git keeps a ring of the last `MAX_XOR_OFFSET` bitmaps and
                // fails when the slot it wants is empty; storing every entry in
                // order makes the same slot unconditionally present, so only
                // the bounds check above can reject a chain.
                if xor_offset > 0 && entries.is_empty() {
                    return Err(decode::Error::MissingXorBase);
                }

                let bitmap = read_bitmap(data, &mut pos)?;
                entries.push(Entry {
                    commit_index_pos,
                    xor_offset,
                    flags,
                    bitmap,
                });
            }
        }

        Ok(File {
            version,
            flags,
            entry_count,
            checksum,
            commits,
            trees,
            blobs,
            tags,
            entries,
            lookup_table,
            data: data.to_owned(),
        })
    }

    /// True when the file carries a lookup table, which is git's
    /// `bitmap_git->table_lookup` — the flag that decides both how a commit is
    /// found and whether `rev-list --test-bitmap` says "entries" or "entries
    /// loaded".
    pub fn has_lookup_table(&self) -> bool {
        self.lookup_table.is_some()
    }

    /// The bitmap of the commit at index position `commit_index_pos` with its
    /// XOR chain resolved, or `None` when the commit has no bitmap; git's
    /// `find_bitmap_for_commit()` (pack-bitmap.c:1020-1048).
    pub fn bitmap_for(&self, commit_index_pos: u32) -> Result<Option<ewah::Builder>, decode::Error> {
        match self.lookup_table.as_deref() {
            None => Ok(self.entry_index_of(commit_index_pos).and_then(|at| self.bitmap_at(at))),
            Some(table) => self.lazy_bitmap_for(table, commit_index_pos),
        }
    }

    /// git's `lazy_bitmap_for_commit()` (pack-bitmap.c:883-1018): follow the
    /// lookup table's XOR rows to the base of the chain, then read the entries
    /// back out of the file from that end forwards.
    fn lazy_bitmap_for(
        &self,
        table: &[Triplet],
        commit_index_pos: u32,
    ) -> Result<Option<ewah::Builder>, decode::Error> {
        let Ok(row) = table.binary_search_by_key(&commit_index_pos, |triplet| triplet.commit_pos) else {
            return Ok(None);
        };

        let mut offsets = vec![table[row].offset];
        let mut xor_row = table[row].xor_row;
        while xor_row != 0xffff_ffff {
            // git's "xor chain exceeds entry count" guard against a table whose
            // rows point in a circle (:918-921).
            if offsets.len() >= table.len() {
                return Err(decode::Error::BadXorOffset);
            }
            let triplet = table.get(xor_row as usize).ok_or(decode::Error::BadXorOffset)?;
            offsets.push(triplet.offset);
            xor_row = triplet.xor_row;
        }

        // The base of the chain is stored whole; everything before it in the
        // walk is XOR-ed against what has been composed so far, youngest last.
        let mut composed = None;
        for &offset in offsets.iter().rev() {
            let stored = self.entry_bitmap_at_offset(offset)?;
            composed = Some(match composed {
                None => stored,
                Some(parent) => ewah::write::xor(&stored, &parent),
            });
        }
        Ok(composed)
    }

    /// Read one entry's stored bitmap from `offset`, skipping the commit
    /// position and XOR offset the lookup table makes redundant (:1003-1006).
    ///
    /// git names the commit in its truncation message, which it can do because
    /// it resolved the object id from the triplet's commit position on the way
    /// in; nothing here holds the index that resolution needs, so the offset
    /// stands in for the name.
    fn entry_bitmap_at_offset(&self, offset: u64) -> Result<ewah::Builder, decode::Error> {
        let truncated = decode::Error::TruncatedLookupEntry { offset };
        let mut pos = usize::try_from(offset).map_err(|_| truncated.clone())?;
        if self.data.len().checked_sub(pos).is_none_or(|left| left < 6) {
            return Err(truncated);
        }
        pos += std::mem::size_of::<u32>() + std::mem::size_of::<u8>() + std::mem::size_of::<u8>();
        read_bitmap(&self.data, &mut pos)
    }

    /// The entry for the commit at index position `commit_index_pos`, or `None`
    /// when the commit has no bitmap.
    ///
    /// git keeps an object-id keyed map built while loading the entries; the
    /// index position an entry stores identifies the same commit, and is what a
    /// caller has after looking the commit up in the index.
    pub fn entry_index_of(&self, commit_index_pos: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.commit_index_pos == commit_index_pos)
    }

    /// The bitmap of entry `index` with its XOR chain resolved, git's
    /// `lookup_stored_bitmap()` (pack-bitmap.c:153-170).
    ///
    /// git composes recursively and memoizes as it unwinds; this walks to the
    /// end of the chain first and folds forward, which is the same sequence of
    /// `ewah_xor()` calls in the same argument order — and the order matters,
    /// since the compressed shape of the result is what gets checksummed.
    pub fn bitmap_at(&self, index: usize) -> Option<ewah::Builder> {
        let mut chain = vec![index];
        loop {
            let at = *chain.last().expect("seeded with one entry");
            let entry = self.entries.get(at)?;
            if entry.xor_offset == 0 {
                break;
            }
            chain.push(at.checked_sub(usize::from(entry.xor_offset))?);
        }

        let mut composed = self.entries[*chain.last().expect("non-empty")].bitmap.clone();
        for &at in chain.iter().rev().skip(1) {
            composed = ewah::write::xor(&self.entries[at].bitmap, &composed);
        }
        Some(composed)
    }
}

/// git's `read_bitmap()` (pack-bitmap.c:172-189) over `ewah_read_mmap()`
/// (ewah/ewah_io.c:92-136): decode one EWAH bitmap at `pos` and leave `pos`
/// past it.
///
/// The bounds are checked here rather than left to the decoder because each one
/// has its own message, and a reader that prints a different one than git's is
/// not reporting the same corruption.
fn read_bitmap(data: &[u8], pos: &mut usize) -> Result<ewah::Builder, decode::Error> {
    let corrupt = |detail: String| decode::Error::CorruptEwah {
        detail: format!("corrupt ewah bitmap: {detail}"),
    };
    let rest = data.get(*pos..).ok_or_else(|| corrupt("eof before bit size".into()))?;

    if rest.len() < 4 {
        return Err(corrupt("eof before bit size".into()));
    }
    if rest.len() - 4 < 4 {
        return Err(corrupt("eof before length".into()));
    }
    let word_count = u64::from(u32::from_be_bytes(rest[4..8].try_into().expect("4 bytes")));
    let data_len = word_count * std::mem::size_of::<u64>() as u64;
    let left = (rest.len() - 8) as u64;
    if left < data_len {
        return Err(corrupt(format!("eof in data ({} bytes short)", data_len - left)));
    }
    if left - data_len < 4 {
        return Err(corrupt("eof before rlw".into()));
    }

    let (decoded, tail) = ewah::decode(rest).map_err(|err| corrupt(err.to_string()))?;
    *pos = data.len() - tail.len();
    Ok(ewah::Builder::from_decoded(&decoded))
}

#[cfg(test)]
mod tests {
    use super::File;
    use crate::data::output::bitmap::{Commit, Options, write};
    use gix_object::Kind;

    fn checksum() -> gix_hash::ObjectId {
        gix_hash::ObjectId::from_bytes_or_panic(&[7u8; 20])
    }

    /// How many objects the pack these fixtures describe holds, which the
    /// reader needs to know where the trailing hash cache begins.
    const OBJECTS: usize = 1024;

    /// Three commits that share a large, sparse reachable set and differ in one
    /// word each.
    ///
    /// The shape matters: the writer only stores an entry as a difference when
    /// the difference compresses smaller than the bitmap itself, so a fixture
    /// of a few dozen bits is written whole and exercises no chain at all.
    /// Scattered literal words with a one-word delta between neighbours is what
    /// a real history looks like to the XOR search.
    fn selection() -> Vec<Commit> {
        (0..3u32)
            .map(|n| Commit {
                index_position: n,
                date: i64::from(n) * 100,
                reachable: {
                    let mut words = vec![0u64; 16];
                    for at in (0..12).step_by(2) {
                        words[at] = 0x0f0f_0f0f_0f0f_0f0f;
                    }
                    words[12 + n as usize] = 0x00ff_00ff_00ff_00ff;
                    words
                },
            })
            .collect()
    }

    fn kinds() -> Vec<Kind> {
        let mut out = vec![Kind::Blob; OBJECTS];
        out[0] = Kind::Commit;
        out[1] = Kind::Commit;
        out[2] = Kind::Commit;
        out[50] = Kind::Tree;
        out[OBJECTS - 1] = Kind::Tag;
        out
    }

    fn written(options: Options) -> Vec<u8> {
        write(
            gix_hash::Kind::Sha1,
            &checksum(),
            &kinds(),
            &vec![9u32; OBJECTS],
            selection(),
            &[],
            options,
        )
        .expect("hashing cannot fail")
    }

    fn read(bytes: &[u8]) -> File {
        File::from_bytes(bytes, gix_hash::Kind::Sha1, OBJECTS as u32).expect("what we write, we can read")
    }

    #[test]
    fn the_header_and_the_pack_it_belongs_to_survive_the_round_trip() {
        let file = read(&written(Options {
            hash_cache: true,
            lookup_table: false,
        }));
        assert_eq!(file.version, 1);
        assert_eq!(file.flags, 0x1 | 0x4, "full DAG and hash cache");
        assert_eq!(file.entry_count, 3, "one entry per selected commit");
        assert_eq!(file.checksum, checksum(), "the pack the bits address");
        assert!(!file.has_lookup_table());
    }

    #[test]
    fn an_xor_chained_entry_resolves_to_the_bitmap_it_was_built_from() {
        let file = read(&written(Options::default()));
        assert!(
            file.entries.iter().any(|entry| entry.xor_offset != 0),
            "the fixture is only interesting while the writer still chains it"
        );
        for commit in selection() {
            let at = file
                .entry_index_of(commit.index_position)
                .expect("every selected commit has an entry");
            let resolved = file.bitmap_at(at).expect("the chain is in range");
            assert!(
                super::bitmap_words_equal(&resolved.to_bitmap_words(), &commit.reachable),
                "entry {at} resolves to the reachability it was given"
            );
        }
    }

    #[test]
    fn the_lookup_table_answers_the_same_bitmaps_the_entries_would_have() {
        let file = read(&written(Options {
            hash_cache: true,
            lookup_table: true,
        }));
        assert!(file.has_lookup_table());
        assert!(
            file.entries.is_empty(),
            "git skips load_bitmap_entries_v1() when it can seek instead"
        );
        for commit in selection() {
            let resolved = file
                .bitmap_for(commit.index_position)
                .expect("the table is well formed")
                .expect("every selected commit is in the table");
            assert!(
                super::bitmap_words_equal(&resolved.to_bitmap_words(), &commit.reachable),
                "commit at index position {} resolves through the table",
                commit.index_position
            );
        }
        assert!(
            file.bitmap_for(OBJECTS as u32 - 1)
                .expect("a miss is not corruption")
                .is_none(),
            "an object with no entry has no bitmap"
        );
    }

    #[test]
    fn the_type_bitmaps_name_the_objects_they_were_built_over() {
        let file = read(&written(Options::default()));
        let has = |words: &[u64], at: usize| words.get(at / 64).is_some_and(|w| w & (1 << (at % 64)) != 0);
        let (commits, trees, blobs, tags) = (
            file.commits.to_bitmap_words(),
            file.trees.to_bitmap_words(),
            file.blobs.to_bitmap_words(),
            file.tags.to_bitmap_words(),
        );
        for (at, kind) in kinds().iter().enumerate() {
            let found = [
                (Kind::Commit, has(&commits, at)),
                (Kind::Tree, has(&trees, at)),
                (Kind::Blob, has(&blobs, at)),
                (Kind::Tag, has(&tags, at)),
            ];
            let set: Vec<Kind> = found.iter().filter(|(_, on)| *on).map(|(kind, _)| *kind).collect();
            assert_eq!(set, vec![*kind], "pack position {at} has exactly its own type");
        }
    }

    #[test]
    fn a_file_that_is_not_a_bitmap_is_refused_rather_than_misread() {
        let mut bytes = written(Options::default());
        bytes[0] = b'X';
        assert!(matches!(
            File::from_bytes(&bytes, gix_hash::Kind::Sha1, OBJECTS as u32),
            Err(decode::Error::WrongHeader)
        ));

        let bytes = written(Options::default());
        assert!(
            matches!(
                File::from_bytes(&bytes[..30], gix_hash::Kind::Sha1, OBJECTS as u32),
                Err(decode::Error::TooSmall)
            ),
            "a file shorter than its own header plus checksum"
        );
    }

    use super::decode;
}
