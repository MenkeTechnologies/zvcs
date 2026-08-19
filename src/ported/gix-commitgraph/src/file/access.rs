use std::{
    fmt::{Debug, Formatter},
    path::Path,
};

use crate::{
    File, bloom,
    file::{
        self, BLOOM_DATA_HEADER_SIZE, CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW, COMMIT_DATA_ENTRY_SIZE_SANS_HASH,
        commit::Commit,
    },
};

/// Access
impl File {
    /// The number of base graphs that this file depends on.
    pub fn base_graph_count(&self) -> u8 {
        self.base_graph_count
    }

    /// Whether this file carries the `GDA2` chunk, i.e. whether a corrected commit date can be
    /// read for its commits at all.
    ///
    /// This is git's `commit_graph::read_generation_data` (commit-graph.c:460-461).
    pub fn has_generation_data(&self) -> bool {
        self.generation_data_offset.is_some()
    }

    /// The corrected commit date of the commit at lexicographical position `pos`, whose own
    /// committer timestamp is `committer_timestamp`, or `None` if this file has no `GDA2` chunk.
    ///
    /// Port of the `read_generation_data` branch of `fill_commit_graph_info()`
    /// (commit-graph.c:902-915): `GDA2` holds an offset over the commit's own date, and a slot
    /// with [`CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW`] set instead indexes `GDO2` for a `u64` one.
    /// git `die()`s on an index that `GDO2` cannot hold; a reader that must not fail answers
    /// `None` there, which puts the commit back on the same footing as one outside the graph.
    pub fn generation_data_at(&self, pos: file::Position, committer_timestamp: u64) -> Option<u64> {
        let start = self.generation_data_offset?;
        if pos.0 >= self.num_commits() {
            return None;
        }
        let at = start + 4 * pos.0 as usize;
        let offset = u32::from_be_bytes(self.data[at..at + 4].try_into().expect("4 bytes"));
        if offset & CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW == 0 {
            return Some(committer_timestamp + u64::from(offset));
        }
        let overflow = self.generation_data_overflow_range.clone()?;
        let at = overflow.start + 8 * (offset ^ CORRECTED_COMMIT_DATE_OFFSET_OVERFLOW) as usize;
        if at + 8 > overflow.end {
            return None;
        }
        Some(committer_timestamp + u64::from_be_bytes(self.data[at..at + 8].try_into().expect("8 bytes")))
    }

    /// How the changed-path Bloom filters in this file were built, or `None` if
    /// it carries none.
    ///
    /// This is the `BDAT` header, and it is what a writer replacing this file
    /// must match: filters built with different sizing or a different hash
    /// version cannot share a chunk with these.
    pub fn bloom_filter_settings(&self) -> Option<bloom::Settings> {
        self.bloom_filter_settings
    }

    /// The changed-path Bloom filter for the commit at lexicographical position
    /// `pos`, or `None` if this file has no filters or the entry is unusable.
    ///
    /// Port of git's `load_bloom_filter_from_graph()`. `BIDX[i]` is the *end* of
    /// filter `i` within the data, so a filter runs from `BIDX[i-1]` to
    /// `BIDX[i]` with `BIDX[-1]` taken as zero. git tolerates an offset equal to
    /// the data size, because the last filter's end is one past the last byte,
    /// but rejects anything beyond it and rejects a pair that decreases — both
    /// of which mean a corrupt index rather than an absent filter.
    pub fn bloom_filter_at(&self, pos: file::Position) -> Option<bloom::Filter> {
        let indexes = self.bloom_indexes_offset?;
        let data = self.bloom_data_range.clone()?;
        if pos.0 >= self.num_commits() {
            return None;
        }

        let payload = data.len() - BLOOM_DATA_HEADER_SIZE;
        let end_at = indexes + 4 * pos.0 as usize;
        let end = u32::from_be_bytes(self.data[end_at..end_at + 4].try_into().expect("4 bytes")) as usize;
        let start = match pos.0.checked_sub(1) {
            Some(prev) => {
                let at = indexes + 4 * prev as usize;
                u32::from_be_bytes(self.data[at..at + 4].try_into().expect("4 bytes")) as usize
            }
            None => 0,
        };

        if end > payload || start > payload || end < start {
            return None;
        }
        let base = data.start + BLOOM_DATA_HEADER_SIZE;
        Some(bloom::Filter {
            data: self.data[base + start..base + end].to_vec(),
        })
    }

    /// Returns the commit data for the commit located at the given lexicographical position.
    ///
    /// `pos` must range from 0 to `self.num_commits()`.
    ///
    /// # Panics
    ///
    /// Panics if `pos` is out of bounds.
    pub fn commit_at(&self, pos: file::Position) -> Commit<'_> {
        Commit::new(self, pos)
    }

    /// The kind of hash used in this File.
    ///
    /// Note that it is always conforming to the hash used in the owning repository.
    pub fn object_hash(&self) -> gix_hash::Kind {
        self.object_hash
    }

    /// Returns an object id at the given index in our list of (sorted) hashes.
    /// The position ranges from 0 to `self.num_commits()`
    // copied from gix-odb/src/pack/index/ext
    pub fn id_at(&self, pos: file::Position) -> &gix_hash::oid {
        assert!(
            pos.0 < self.num_commits(),
            "expected lexicographical position less than {}, got {}",
            self.num_commits(),
            pos.0
        );
        let pos: usize = pos
            .0
            .try_into()
            .expect("an architecture able to hold 32 bits of integer");
        let start = self.oid_lookup_offset + (pos * self.hash_len);
        gix_hash::oid::from_bytes_unchecked(&self.data[start..][..self.hash_len])
    }

    /// Return an iterator over all object hashes stored in the base graph.
    pub fn iter_base_graph_ids(&self) -> impl Iterator<Item = &gix_hash::oid> {
        let start = self.base_graphs_list_offset.unwrap_or(0);
        let base_graphs_list = &self.data[start..][..self.hash_len * usize::from(self.base_graph_count)];
        base_graphs_list
            .chunks_exact(self.hash_len)
            .map(gix_hash::oid::from_bytes_unchecked)
    }

    /// return an iterator over all commits in this file.
    pub fn iter_commits(&self) -> impl Iterator<Item = Commit<'_>> {
        (0..self.num_commits()).map(move |i| self.commit_at(file::Position(i)))
    }

    /// Return an iterator over all object hashes stored in this file.
    pub fn iter_ids(&self) -> impl Iterator<Item = &gix_hash::oid> {
        (0..self.num_commits()).map(move |i| self.id_at(file::Position(i)))
    }

    /// Translate the given object hash to its position within this file, if present.
    // copied from gix-odb/src/pack/index/ext
    pub fn lookup(&self, id: impl AsRef<gix_hash::oid>) -> Option<file::Position> {
        self.lookup_inner(id.as_ref())
    }

    fn lookup_inner(&self, id: &gix_hash::oid) -> Option<file::Position> {
        let first_byte = usize::from(id.first_byte());
        let mut upper_bound = self.fan[first_byte];
        let mut lower_bound = if first_byte != 0 { self.fan[first_byte - 1] } else { 0 };

        while lower_bound < upper_bound {
            let mid = u32::midpoint(lower_bound, upper_bound);
            let mid_sha = self.id_at(file::Position(mid));

            use std::cmp::Ordering::*;
            match id.cmp(mid_sha) {
                Less => upper_bound = mid,
                Equal => return Some(file::Position(mid)),
                Greater => lower_bound = mid + 1,
            }
        }
        None
    }

    /// Returns the number of commits in this graph file.
    ///
    /// The maximum valid `file::Position` that can be used with this file is one less than
    /// `num_commits()`.
    pub fn num_commits(&self) -> u32 {
        self.fan[255]
    }

    /// Returns the path to this file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl File {
    /// Returns the byte slice for the given commit in this file's Commit Data (CDAT) chunk.
    pub(crate) fn commit_data_bytes(&self, pos: file::Position) -> &[u8] {
        assert!(
            pos.0 < self.num_commits(),
            "expected lexicographical position less than {}, got {}",
            self.num_commits(),
            pos.0
        );
        let pos: usize = pos
            .0
            .try_into()
            .expect("an architecture able to hold 32 bits of integer");
        let entry_size = self.hash_len + COMMIT_DATA_ENTRY_SIZE_SANS_HASH;
        let start = self.commit_data_offset + (pos * entry_size);
        &self.data[start..][..entry_size]
    }

    /// Returns the byte slice for this file's entire Extra Edge List (EDGE) chunk.
    pub(crate) fn extra_edges_data(&self) -> Option<&[u8]> {
        Some(&self.data[self.extra_edges_list_range.clone()?])
    }
}

impl Debug for File {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, r#"File("{:?}")"#, self.path.display())
    }
}
