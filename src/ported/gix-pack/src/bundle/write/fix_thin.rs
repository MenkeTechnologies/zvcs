//! Thin-pack completion the way `index-pack --fix-thin` does it.
//!
//! The pack is copied through exactly as it arrives. Only once all of it has been read are its `OBJ_REF_DELTA`
//! entries resolved against the objects the pack itself holds (`resolve_deltas()`, `builtin/index-pack.c:1329-1362`),
//! and only the ones left unresolved have their base read from the object database and appended to the end of the
//! pack, after which the header's object count and the trailer are rewritten (`conclude_pack()` and
//! `fix_unresolved_deltas()`, `builtin/index-pack.c:1370-1541`). A base the pack already holds, delta or not, is
//! therefore never stored a second time, even when the object database has it too.

use std::{
    collections::{HashSet, VecDeque},
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, atomic::AtomicBool},
};

use gix_hash::ObjectId;

use crate::{
    bundle::write::types::SharedTempFile,
    data::{self, entry::Header, input},
    index::write::ref_delta::{Base, Walker},
};

/// An iterator over the entries of a pack whose bytes are written verbatim to `data_file` while being read, which
/// yields the base objects needed to complete it once the received entries are exhausted.
pub(crate) struct FixThinIter<I, Find> {
    inner: I,
    lookup: Find,
    compression: gix_zlib::Compression,
    object_hash: gix_hash::Kind,
    pack_version: data::Version,
    data_file: SharedTempFile,
    /// Header and byte range of every received entry, in pack order.
    received: Vec<(Header, data::EntryRange)>,
    /// The entries appended to complete the pack, still to be handed out.
    appended: VecDeque<input::Entry>,
    /// Set once the received entries ran out and the pack was concluded.
    concluded: bool,
    base_sink: Arc<parking_lot::Mutex<Vec<ObjectId>>>,
}

impl<I, Find> FixThinIter<I, Find>
where
    I: Iterator<Item = Result<input::Entry, input::Error>>,
    Find: gix_object::Find,
{
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        inner: I,
        lookup: Find,
        compression: gix_zlib::Compression,
        object_hash: gix_hash::Kind,
        pack_version: data::Version,
        data_file: SharedTempFile,
        base_sink: Arc<parking_lot::Mutex<Vec<ObjectId>>>,
    ) -> Self {
        FixThinIter {
            inner,
            lookup,
            compression,
            object_hash,
            pack_version,
            data_file,
            received: Vec::new(),
            appended: VecDeque::new(),
            concluded: false,
            base_sink,
        }
    }

    /// `conclude_pack()`: append whatever bases the pack needs and fix up its header and trailer.
    fn conclude(&mut self) -> Result<(), input::Error> {
        let Some(entries_end) = self.received.last().map(|(_, range)| range.end) else {
            return Ok(());
        };
        let mut file = self.data_file.lock();
        file.flush().map_err(gix_hash::io::Error::from)?;
        let path = file
            .get_mut()
            .with_mut(|f| f.path().to_owned())
            .map_err(gix_hash::io::Error::from)?;
        // Whatever the reader buffered past the trailer was copied along with the pack, but is no part of it.
        let pack_end = entries_end + self.object_hash.len_in_bytes() as u64;
        file.get_mut()
            .with_mut(|f| f.as_file_mut().set_len(pack_end))
            .and_then(|r| r)
            .map_err(gix_hash::io::Error::from)?;

        let bases = match self.unresolved_bases(&path)? {
            Some(bases) => bases,
            // A pack that cannot be decoded is left as it is: indexing it reports the problem, as `resolve_deltas()`
            // dies before `conclude_pack()` ever runs.
            None => return Ok(()),
        };
        if bases.is_empty() {
            return Ok(());
        }

        // `fixup_pack_header_footer()` (`pack-write.c`): the appended objects go where the trailer was, the header
        // learns the new count, and the trailer is recomputed over all of it.
        let mut offset = entries_end;
        file.seek(SeekFrom::Start(offset)).map_err(gix_hash::io::Error::from)?;
        for mut entry in bases {
            entry.pack_offset = offset;
            entry
                .header
                .write_to(entry.decompressed_size, &mut *file)
                .map_err(gix_hash::io::Error::from)?;
            file.write_all(&entry.compressed.take().expect("always set for an appended base"))
                .map_err(gix_hash::io::Error::from)?;
            offset += entry.bytes_in_pack();
            self.appended.push_back(entry);
        }
        let num_objects = (self.received.len() + self.appended.len()) as u32;
        file.seek(SeekFrom::Start(0)).map_err(gix_hash::io::Error::from)?;
        file.write_all(&data::header::encode(self.pack_version, num_objects))
            .map_err(gix_hash::io::Error::from)?;
        file.flush().map_err(gix_hash::io::Error::from)?;

        let mut hasher = gix_hash::hasher(self.object_hash);
        let mut pack = std::fs::File::open(&path).map_err(gix_hash::io::Error::from)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = pack.read(&mut buf).map_err(gix_hash::io::Error::from)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let trailer = hasher.try_finalize().map_err(gix_hash::io::Error::from)?;
        file.seek(SeekFrom::End(0)).map_err(gix_hash::io::Error::from)?;
        file.write_all(trailer.as_slice()).map_err(gix_hash::io::Error::from)?;
        file.flush().map_err(gix_hash::io::Error::from)?;
        if let Some(last) = self.appended.back_mut() {
            last.trailer = Some(trailer);
        }
        Ok(())
    }

    /// `resolve_deltas()` followed by `fix_unresolved_deltas()`: the base objects, in the order they are to be appended,
    /// that resolve the ref-deltas the pack itself cannot. `None` if the pack could not be decoded.
    fn unresolved_bases(&self, path: &std::path::Path) -> Result<Option<Vec<input::Entry>>, input::Error> {
        let num_ref_deltas = self
            .received
            .iter()
            .filter(|(header, _)| matches!(header, Header::RefDelta { .. }))
            .count();
        if num_ref_deltas == 0 {
            return Ok(Some(Vec::new()));
        }
        let Ok(pack) = crate::mmap::read_only(path) else {
            return Ok(None);
        };
        let ranges: Vec<data::EntryRange> = self.received.iter().map(|(_, range)| range.clone()).collect();
        let should_interrupt = AtomicBool::new(false);
        let Ok(mut walker) = Walker::new(super::resolve_entry, &pack, &ranges, self.object_hash, None, &should_interrupt) else {
            return Ok(None);
        };

        let mut resolved = HashSet::new();
        for root in walker.roots.clone() {
            if resolved.len() == num_ref_deltas {
                break;
            }
            let walked = walker.root(root).and_then(|base| {
                walker.walk(base, &mut |child, _| {
                    resolved.insert(child);
                })
            });
            if walked.is_err() {
                return Ok(None);
            }
        }

        // "a good heuristic is to start resolving deltas in the same order as their position in the pack"
        // (`builtin/index-pack.c:1488-1501`); `received` is in pack order already.
        let mut bases = Vec::new();
        let mut buf = Vec::new();
        for (index, (header, _)) in self.received.iter().enumerate() {
            let Header::RefDelta { base_id } = header else { continue };
            if resolved.contains(&index) {
                continue;
            }
            // `odb_read_object()` returning NULL just moves on; the delta stays unresolved and indexing dies
            // `pack has %d unresolved deltas` (`builtin/index-pack.c:1520-1523, 1405-1409`).
            let Ok(Some(object)) = self.lookup.try_find(base_id, &mut buf) else {
                continue;
            };
            // `check_object_signature()` (`builtin/index-pack.c:1526-1528`).
            if gix_object::compute_hash(self.object_hash, object.kind, object.data).ok() != Some(*base_id) {
                return Err(input::Error::CorruptLocalObject { id: *base_id });
            }
            let entry = input::Entry::from_data_obj(&object, 0, self.compression)?;
            let base = Base {
                index: None,
                kind: object.kind,
                id: *base_id,
                data: object.data.to_vec(),
            };
            self.base_sink.lock().push(*base_id);
            bases.push(entry);
            let walked = walker.walk(base, &mut |child, _| {
                resolved.insert(child);
            });
            if walked.is_err() {
                return Ok(None);
            }
        }
        Ok(Some(bases))
    }
}

impl<I, Find> Iterator for FixThinIter<I, Find>
where
    I: Iterator<Item = Result<input::Entry, input::Error>>,
    Find: gix_object::Find,
{
    type Item = Result<input::Entry, input::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.concluded {
            match self.inner.next() {
                Some(Ok(entry)) => {
                    let start = entry.pack_offset;
                    self.received.push((entry.header, start..start + entry.bytes_in_pack()));
                    return Some(Ok(entry));
                }
                Some(Err(err)) => return Some(Err(err)),
                None => {
                    self.concluded = true;
                    if let Err(err) = self.conclude() {
                        return Some(Err(err));
                    }
                }
            }
        }
        self.appended.pop_front().map(Ok)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // The lower bound counts only what the pack header announced, which is what the index writer reports as
        // received; the bases appended to complete the pack come on top of it.
        if self.concluded {
            (0, Some(self.appended.len()))
        } else {
            let (min, max) = self.inner.size_hint();
            (min, max.map(|max| max * 2))
        }
    }
}
