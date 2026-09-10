use std::time::SystemTime;

use crate::multi_index;

mod error {
    /// The error returned by [`crate::multi_index::write_from_index_paths()`].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error(transparent)]
        Io(#[from] gix_hash::io::Error),
        #[error("Interrupted")]
        Interrupted,
        #[error(transparent)]
        OpenIndex(#[from] crate::index::init::Error),
    }
}
pub use error::Error;

/// An entry suitable for sorting and writing
pub(crate) struct Entry {
    pub(crate) id: gix_hash::ObjectId,
    pub(crate) pack_index: u32,
    pub(crate) pack_offset: crate::data::Offset,
    /// Used for sorting in case of duplicates
    index_mtime: SystemTime,
}

/// Options for use in [`multi_index::write_from_index_paths()`].
pub struct Options {
    /// The kind of hash to use for objects and to expect in the input files.
    pub object_hash: gix_hash::Kind,
    /// When set, plan the `RIDX` and `BTMP` chunks that a multi-pack `.bitmap`
    /// is read alongside, and compute the pseudo-pack order they encode.
    ///
    /// git adds both chunks together, for `MIDX_WRITE_REV_INDEX` or
    /// `MIDX_WRITE_BITMAP` (midx-write.c:1674-1682, v2.55.0), and `--bitmap`
    /// sets both bits at once.
    pub bitmap_order: Option<BitmapOrder>,
}

/// Which pack `midx_pack_order()` sorts ahead of the others.
///
/// git picks one whenever it writes the reverse-index chunks: the pack named by
/// `--preferred-pack`, else the oldest by mtime, "to ensure that the pack from
/// which the first object is selected in pseudo pack-order has all of its
/// objects selected from that pack (and not another pack containing a
/// duplicate)" (midx-write.c:1458-1497, v2.55.0). `None` is git's
/// `NO_PREFERRED_PACK`, which leaves every entry demoted equally.
pub struct BitmapOrder {
    /// The `.idx` file name of the preferred pack, matched against the sorted
    /// index names this writer builds the `PNAM` chunk from.
    pub preferred_index_name: Option<std::ffi::OsString>,
}

/// One object as the multi-index records it, in the lexicographic order the
/// `OIDL` chunk stores.
///
/// Handed back so a caller that goes on to write a multi-pack `.bitmap` can
/// address the same objects the file does without re-reading it.
#[derive(Debug, Clone)]
pub struct EntryInfo {
    /// The object's id.
    pub id: gix_hash::ObjectId,
    /// Which pack holds it, as an index into the sorted `PNAM` list.
    pub pack_index: u32,
    /// Its offset in that pack.
    pub pack_offset: crate::data::Offset,
}

/// The result of [`multi_index::write_from_index_paths()`].
pub struct Outcome {
    /// The calculated multi-index checksum of the file at `multi_index_path`.
    pub multi_index_checksum: gix_hash::ObjectId,
    /// Every object the multi-index holds, in `OIDL` order.
    pub entries: Vec<EntryInfo>,
    /// `midx_pack_order()`: for each position in pseudo-pack order, the
    /// position the object has in [`Outcome::entries`]. Empty unless
    /// [`Options::bitmap_order`] asked for it.
    pub pack_order: Vec<u32>,
}

/// `midx_pack_order()` (midx-write.c:659-703, v2.55.0).
///
/// `placement` is `(pack index, offset in that pack)` per object, in the
/// multi-index's lexicographic order. Sorts those by
/// `(preferred-demoted pack, offset)` and answers with the resulting
/// permutation — the `RIDX` chunk — plus, per pack, where its first object
/// landed and how many it contributed, which is the `BTMP` chunk.
pub fn pack_order(
    placement: &[(u32, crate::data::Offset)],
    preferred_pack: Option<u32>,
    num_packs: usize,
) -> (Vec<u32>, Vec<(u32, u32)>) {
    // ```c
    // data[i].pack = midx_pack_perm(ctx, e->pack_int_id);
    // if (!e->preferred || ctx->compact)
    //         data[i].pack |= (1U << 31);
    // ```
    //
    // The high bit is the whole mechanism: an entry from the preferred pack
    // keeps its small key and therefore sorts ahead of every entry that does
    // not, whichever pack those came from.
    const DEMOTED: u32 = 1 << 31;
    let mut data: Vec<(u32, crate::data::Offset, u32)> = placement
        .iter()
        .enumerate()
        .map(|(at, (pack_index, pack_offset))| {
            let key = if Some(*pack_index) == preferred_pack {
                *pack_index
            } else {
                *pack_index | DEMOTED
            };
            (key, *pack_offset, at as u32)
        })
        .collect();
    // `midx_pack_order_cmp()` compares pack then offset and nothing else. Two
    // entries can only tie on both when they are the same object in the same
    // pack, which the deduplication above has already ruled out, so the order
    // is total and git's unstable `QSORT` cannot disagree with this one.
    data.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut order = Vec::with_capacity(data.len());
    let mut positions: Vec<Option<u32>> = vec![None; num_packs];
    let mut counts: Vec<u32> = vec![0; num_packs];
    for (at, (key, _, entry)) in data.iter().enumerate() {
        let pack = (key & !DEMOTED) as usize;
        if positions[pack].is_none() {
            positions[pack] = Some(at as u32);
        }
        counts[pack] += 1;
        order.push(*entry);
    }
    // "if (pack->bitmap_pos == BITMAP_POS_UNKNOWN) pack->bitmap_pos = 0;" — a
    // pack that contributed nothing still gets a row, and it names position
    // zero rather than the sentinel.
    let per_pack = positions
        .into_iter()
        .zip(counts)
        .map(|(position, count)| (position.unwrap_or(0), count))
        .collect();
    (order, per_pack)
}

/// The progress ids used in [`crate::multi_index::write_from_index_paths()`].
///
/// Use this information to selectively extract the progress of interest in case the parent application has custom visualization.
#[derive(Debug, Copy, Clone)]
pub enum ProgressId {
    /// Counts each path in the input set whose entries we enumerate and write into the multi-index
    FromPathsCollectingEntries,
    /// The amount of bytes written as part of the multi-index.
    BytesWritten,
}

impl From<ProgressId> for gix_features::progress::Id {
    fn from(v: ProgressId) -> Self {
        match v {
            ProgressId::FromPathsCollectingEntries => *b"MPCE",
            ProgressId::BytesWritten => *b"MPBW",
        }
    }
}

impl<T> multi_index::File<T> {
    pub(crate) const SIGNATURE: &'static [u8] = b"MIDX";
    pub(crate) const HEADER_LEN: usize = 4 /*signature*/ +
        1 /*version*/ +
        1 /*object id version*/ +
        1 /*num chunks */ +
        1 /*num base files */ +
        4 /*num pack files*/;
}

pub(super) mod function {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicBool, Ordering},
        time::{Instant, SystemTime},
    };

    use gix_features::progress::{Count, DynNestedProgress, Progress};

    use crate::{MMap, multi_index};

    use super::{Entry, Error, Options, Outcome, ProgressId};

    /// Create a new multi-index file for writing to `out` from the pack index files at `index_paths`.
    ///
    /// Progress is sent to `progress` and interruptions checked via `should_interrupt`.
    pub fn write_from_index_paths(
        mut index_paths: Vec<PathBuf>,
        out: &mut dyn std::io::Write,
        progress: &mut dyn DynNestedProgress,
        should_interrupt: &AtomicBool,
        Options {
            object_hash,
            bitmap_order,
        }: Options,
    ) -> Result<Outcome, Error> {
        let out = gix_hash::io::Write::new(out, object_hash);
        let (index_paths_sorted, index_filenames_sorted) = {
            index_paths.sort();
            let file_names = index_paths
                .iter()
                .map(|p| PathBuf::from(p.file_name().expect("file name present")))
                .collect::<Vec<_>>();
            (index_paths, file_names)
        };

        let entries = {
            let mut entries = Vec::new();
            let start = Instant::now();
            let mut progress = progress.add_child_with_id(
                "Collecting entries".into(),
                ProgressId::FromPathsCollectingEntries.into(),
            );
            progress.init(Some(index_paths_sorted.len()), gix_features::progress::count("indices"));

            // This could be parallelized… but it's probably not worth it unless you have 500mio objects.
            for (index_id, index) in index_paths_sorted.iter().enumerate() {
                let mtime = index
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let index = crate::index::File::at(index, object_hash)?;

                entries.reserve(index.num_objects() as usize);
                entries.extend(index.iter().map(|e| Entry {
                    id: e.oid,
                    pack_index: index_id as u32,
                    pack_offset: e.pack_offset,
                    index_mtime: mtime,
                }));
                progress.inc();
                if should_interrupt.load(Ordering::Relaxed) {
                    return Err(Error::Interrupted);
                }
            }
            progress.show_throughput(start);

            let start = Instant::now();
            progress.set_name("Deduplicate".into());
            progress.init(Some(entries.len()), gix_features::progress::count("entries"));
            entries.sort_by(|l, r| {
                l.id.cmp(&r.id)
                    .then_with(|| l.index_mtime.cmp(&r.index_mtime).reverse())
                    .then_with(|| l.pack_index.cmp(&r.pack_index))
            });
            entries.dedup_by_key(|e| e.id);
            progress.inc_by(entries.len());
            progress.show_throughput(start);
            if should_interrupt.load(Ordering::Relaxed) {
                return Err(Error::Interrupted);
            }
            entries
        };

        let mut cf = gix_chunk::file::Index::for_writing();
        cf.plan_chunk(
            multi_index::chunk::index_names::ID,
            multi_index::chunk::index_names::storage_size(&index_filenames_sorted),
        );
        cf.plan_chunk(multi_index::chunk::fanout::ID, multi_index::chunk::fanout::SIZE as u64);
        cf.plan_chunk(
            multi_index::chunk::lookup::ID,
            multi_index::chunk::lookup::storage_size(entries.len(), object_hash),
        );
        cf.plan_chunk(
            multi_index::chunk::offsets::ID,
            multi_index::chunk::offsets::storage_size(entries.len()),
        );

        let num_large_offsets = multi_index::chunk::large_offsets::num_large_offsets(&entries);
        if let Some(num_large_offsets) = num_large_offsets {
            cf.plan_chunk(
                multi_index::chunk::large_offsets::ID,
                multi_index::chunk::large_offsets::storage_size(num_large_offsets),
            );
        }

        // git plans `RIDX` and `BTMP` last, after the optional `LOFF`, and both
        // together (midx-write.c:1674-1682, v2.55.0).
        let (order, bitmapped_packs) = match &bitmap_order {
            None => (Vec::new(), Vec::new()),
            Some(super::BitmapOrder { preferred_index_name }) => {
                let preferred = preferred_index_name.as_ref().and_then(|name| {
                    index_filenames_sorted
                        .iter()
                        .position(|candidate| candidate.as_os_str() == name.as_os_str())
                        .map(|at| at as u32)
                });
                let placement: Vec<(u32, crate::data::Offset)> =
                    entries.iter().map(|e| (e.pack_index, e.pack_offset)).collect();
                super::pack_order(&placement, preferred, index_filenames_sorted.len())
            }
        };
        if bitmap_order.is_some() {
            cf.plan_chunk(
                multi_index::chunk::revindex::ID,
                multi_index::chunk::revindex::storage_size(entries.len()),
            );
            cf.plan_chunk(
                multi_index::chunk::bitmapped_packs::ID,
                multi_index::chunk::bitmapped_packs::storage_size(index_filenames_sorted.len()),
            );
        }

        let mut write_progress =
            progress.add_child_with_id("Writing multi-index".into(), ProgressId::BytesWritten.into());
        let write_start = Instant::now();
        write_progress.init(
            Some(cf.planned_storage_size() as usize + multi_index::File::<MMap>::HEADER_LEN),
            gix_features::progress::bytes(),
        );
        let mut out = gix_features::progress::Write {
            inner: out,
            progress: write_progress,
        };

        let bytes_written = multi_index::File::<MMap>::write_header(
            &mut out,
            cf.num_chunks().try_into().expect("BUG: wrote more than 256 chunks"),
            index_paths_sorted.len() as u32,
            object_hash,
        )
        .map_err(gix_hash::io::Error::from)?;

        {
            progress.set_name("Writing chunks".into());
            progress.init(Some(cf.num_chunks()), gix_features::progress::count("chunks"));

            let mut chunk_write = cf
                .into_write(&mut out, bytes_written)
                .map_err(gix_hash::io::Error::from)?;
            while let Some(chunk_to_write) = chunk_write.next_chunk() {
                match chunk_to_write {
                    multi_index::chunk::index_names::ID => {
                        multi_index::chunk::index_names::write(&index_filenames_sorted, &mut chunk_write)
                    }
                    multi_index::chunk::fanout::ID => multi_index::chunk::fanout::write(&entries, &mut chunk_write),
                    multi_index::chunk::lookup::ID => multi_index::chunk::lookup::write(&entries, &mut chunk_write),
                    multi_index::chunk::offsets::ID => {
                        multi_index::chunk::offsets::write(&entries, num_large_offsets.is_some(), &mut chunk_write)
                    }
                    multi_index::chunk::large_offsets::ID => multi_index::chunk::large_offsets::write(
                        &entries,
                        num_large_offsets.expect("available if planned"),
                        &mut chunk_write,
                    ),
                    multi_index::chunk::revindex::ID => {
                        multi_index::chunk::revindex::write(&order, &mut chunk_write)
                    }
                    multi_index::chunk::bitmapped_packs::ID => {
                        multi_index::chunk::bitmapped_packs::write(&bitmapped_packs, &mut chunk_write)
                    }
                    unknown => unreachable!("BUG: forgot to implement chunk {:?}", std::str::from_utf8(&unknown)),
                }
                .map_err(gix_hash::io::Error::from)?;
                progress.inc();
                if should_interrupt.load(Ordering::Relaxed) {
                    return Err(Error::Interrupted);
                }
            }
        }

        // write trailing checksum
        let multi_index_checksum = out.inner.hash.try_finalize().map_err(gix_hash::io::Error::from)?;
        out.inner
            .inner
            .write_all(multi_index_checksum.as_slice())
            .map_err(gix_hash::io::Error::from)?;
        out.progress.show_throughput(write_start);

        Ok(Outcome {
            multi_index_checksum,
            entries: entries
                .into_iter()
                .map(|entry| super::EntryInfo {
                    id: entry.id,
                    pack_index: entry.pack_index,
                    pack_offset: entry.pack_offset,
                })
                .collect(),
            pack_order: order,
        })
    }
}

impl multi_index::File<crate::MMap> {
    fn write_header(
        out: &mut dyn std::io::Write,
        num_chunks: u8,
        num_indices: u32,
        object_hash: gix_hash::Kind,
    ) -> std::io::Result<usize> {
        out.write_all(Self::SIGNATURE)?;
        out.write_all(&[crate::multi_index::Version::V1 as u8])?;
        out.write_all(&[object_hash as u8])?;
        out.write_all(&[num_chunks])?;
        out.write_all(&[0])?; /* unused number of base files */
        out.write_all(&num_indices.to_be_bytes())?;

        Ok(Self::HEADER_LEN)
    }
}

