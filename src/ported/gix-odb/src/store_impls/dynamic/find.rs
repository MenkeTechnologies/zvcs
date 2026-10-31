use std::ops::Deref;

use gix_pack::cache::DecodeEntry;

use crate::store::{handle, load_index};

pub(crate) mod error {
    use crate::{loose, pack};

    /// Returned by [`Handle::try_find()`][gix_pack::Find::try_find()]
    #[derive(thiserror::Error, Debug)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("An error occurred while obtaining an object from the loose object store")]
        Loose(#[from] loose::find::Error),
        #[error("An error occurred while obtaining an object from the packed object store")]
        Pack(#[from] pack::data::decode::Error),
        #[error(transparent)]
        LoadIndex(#[from] crate::store::load_index::Error),
        #[error(transparent)]
        LoadPack(#[from] std::io::Error),
        #[error(transparent)]
        EntryType(#[from] gix_pack::data::entry::decode::Error),
        #[error("Reached recursion limit of {} while resolving ref delta bases for {}", .max_depth, .id)]
        DeltaBaseRecursionLimit {
            /// the maximum recursion depth we encountered.
            max_depth: usize,
            /// The original object to lookup
            id: gix_hash::ObjectId,
        },
        #[error("The base object {} could not be found but is required to decode {}", .base_id, .id)]
        DeltaBaseMissing {
            /// the id of the base object which failed to lookup
            base_id: gix_hash::ObjectId,
            /// The original object to lookup
            id: gix_hash::ObjectId,
        },
        #[error("An error occurred when looking up a ref delta base object {} to decode {}", .base_id, .id)]
        DeltaBaseLookup {
            #[source]
            err: Box<Self>,
            /// the id of the base object which failed to lookup
            base_id: gix_hash::ObjectId,
            /// The original object to lookup
            id: gix_hash::ObjectId,
        },
        /// The object is only in packs that gave up on it: `odb_source_packed_read_object_info()`'s
        /// message, naming the first such pack (odb/source-packed.c:128-130, odb.c:548-607).
        #[error("packed object {id} (stored in {pack}) is corrupt")]
        PackedObjectCorrupt {
            /// The object that was asked for.
            id: gix_hash::ObjectId,
            /// `p->pack_name` of the first pack that failed to produce it.
            pack: String,
        },
    }

    #[derive(Copy, Clone)]
    pub(crate) struct DeltaBaseRecursion<'a> {
        pub depth: usize,
        pub original_id: &'a gix_hash::oid,
    }

    impl<'a> DeltaBaseRecursion<'a> {
        pub fn new(id: &'a gix_hash::oid) -> Self {
            Self {
                original_id: id,
                depth: 0,
            }
        }
        pub fn inc_depth(mut self) -> Self {
            self.depth += 1;
            self
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn error_size() {
            let actual = std::mem::size_of::<Error>();
            assert!(actual <= 88, "{actual} <= 88: should not grow without us noticing");
        }
    }
}
pub use error::Error;

thread_local! {
    /// Set by [`with_corrupt_reads_as_errors()`].
    static CORRUPT_READS_AS_ERRORS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with whole-object reads handing [`Error::PackedObjectCorrupt`] back rather than dying with
/// it as `odb_read_object()` does (odb.c:597-603), for a caller that still holds output git would have
/// written by then and must flush it before it dies with the same message.
pub fn with_corrupt_reads_as_errors<T>(f: impl FnOnce() -> T) -> T {
    let before = CORRUPT_READS_AS_ERRORS.replace(true);
    let out = f();
    CORRUPT_READS_AS_ERRORS.set(before);
    out
}

use crate::store::types::PackId;

impl<S> super::Handle<S>
where
    S: Deref<Target = super::Store> + Clone,
{
    fn try_find_cached_inner<'a, 'b>(
        &'b self,
        mut id: &'b gix_hash::oid,
        buffer: &'a mut Vec<u8>,
        inflate: &mut gix_zlib::Inflate,
        pack_cache: &mut dyn DecodeEntry,
        snapshot: &mut load_index::Snapshot,
        recursion: Option<error::DeltaBaseRecursion<'_>>,
    ) -> Result<Option<(gix_object::Data<'a>, Option<gix_pack::data::entry::Location>)>, Error> {
        if let Some(r) = recursion {
            if r.depth >= self.max_recursion_depth {
                return Err(Error::DeltaBaseRecursionLimit {
                    max_depth: self.max_recursion_depth,
                    id: r.original_id.to_owned(),
                });
            }
        } else if !self.ignore_replacements {
            id = super::access::follow_replacements(&self.store.replacements, id);
        }

        // `do_oid_object_info_extended()` (odb.c:541-616): the pack the first corrupt read names, and
        // whether the second read, which every miss gets, has run.
        let mut bad_pack: Option<String> = None;
        let mut second_read = false;
        loop {
            let mut failed: Option<(std::sync::Arc<gix_pack::data::File>, u64)> = None;
            {
                let marker = snapshot.marker;
                'indices: for (idx, index) in snapshot.indices.iter_mut().enumerate() {
                    if let Some(handle::index_lookup::Outcome {
                        object_index: handle::IndexForObjectInPack { pack_id, pack_offset },
                        index_file,
                        pack: possibly_pack,
                    }) = index.lookup(id)
                    {
                        let pack = match possibly_pack {
                            Some(pack) => pack,
                            None => match self.store.load_pack(pack_id, marker)? {
                                Some(pack) => {
                                    *possibly_pack = Some(pack);
                                    possibly_pack.as_ref().expect("just put it in")
                                }
                                // The pack this index points at is gone. That happens transiently
                                // while another process repacks, and permanently when a stale
                                // multi-pack-index still names packs a repack deleted - git keeps
                                // resolving those objects from whatever index still holds them, so
                                // finish scanning the snapshot instead of giving up here. The loose
                                // databases and the index refresh below still run afterwards, which
                                // is what covers the concurrent-repack case.
                                None => continue 'indices,
                            },
                        };
                        // `packfile_fill_entry()` and `midx_fill_entry()` pass over an object marked
                        // bad in this pack, which then names a corrupt answer (packfile.c:1846-1853,
                        // midx.c:621-626).
                        if self.store.is_bad_packed_object(pack.path(), id) {
                            bad_pack.get_or_insert_with(|| self.store.pack_name(pack.path()));
                            continue 'indices;
                        }
                        let pack_arc = std::sync::Arc::clone(pack);
                        let pack = &*pack_arc;
                        let res = 'decode: {
                            let entry = match pack.entry(pack_offset) {
                                Ok(entry) => entry,
                                Err(err) => break 'decode Err(err.into()),
                            };
                            let header_size = entry.header_size();
                            let res = pack.decode_entry(
                                entry,
                                buffer,
                                inflate,
                                &|id, _out| {
                                    let pack_offset = index_file.pack_offset_by_id(id)?;
                                    pack.entry(pack_offset)
                                        .ok()
                                        .map(gix_pack::data::decode::entry::ResolvedBase::InPack)
                                },
                                pack_cache,
                            );
                            let res = match res {
                                Ok(r) => Ok((
                                    (r.kind, pack.object_hash()),
                                    Some(gix_pack::data::entry::Location {
                                        pack_id: pack.id,
                                        pack_offset,
                                        entry_size: r.compressed_size + header_size,
                                    }),
                                )),
                                Err(gix_pack::data::decode::Error::DeltaBaseUnresolved(base_id)) => {
                                    // Only with multi-pack indices it's allowed to jump to refer to other packs within this
                                    // multi-pack. Otherwise this would constitute a thin pack which is only allowed in transit.
                                    // However, if we somehow end up with that, we will resolve it safely, even though we could
                                    // avoid handling this case and error instead.

                                    // Since this is a special case, we just allocate here to make it work. It's an actual delta-ref object
                                    // which is sent by some servers that points to an object outside of the pack we are looking
                                    // at right now. With the complexities of loading packs, we go into recursion here. Git itself
                                    // doesn't do a cycle check, and we won't either but limit the recursive depth.
                                    // The whole ordeal isn't as efficient as it could be due to memory allocation and
                                    // later mem-copying when trying again.
                                    let mut buf = Vec::new();
                                    let obj_kind = self
                                        .try_find_cached_inner(
                                            &base_id,
                                            &mut buf,
                                            inflate,
                                            pack_cache,
                                            snapshot,
                                            recursion
                                                .map(error::DeltaBaseRecursion::inc_depth)
                                                .or_else(|| error::DeltaBaseRecursion::new(id).into()),
                                        )
                                        .map_err(|err| Error::DeltaBaseLookup {
                                            err: Box::new(err),
                                            base_id,
                                            id: id.to_owned(),
                                        })
                                        .and_then(|found| {
                                            found.ok_or_else(|| Error::DeltaBaseMissing {
                                                base_id,
                                                id: id.to_owned(),
                                            })
                                        });
                                    let obj_kind = match obj_kind {
                                        Ok(found) => found.0.kind,
                                        Err(err) => break 'decode Err(err),
                                    };
                                    let handle::index_lookup::Outcome {
                                        object_index:
                                            handle::IndexForObjectInPack {
                                                pack_id: _,
                                                pack_offset,
                                            },
                                        index_file,
                                        pack: possibly_pack,
                                    } = match snapshot.indices[idx].lookup(id) {
                                        Some(res) => res,
                                        None => {
                                            let mut out = None;
                                            for index in &mut snapshot.indices {
                                                out = index.lookup(id);
                                                if out.is_some() {
                                                    break;
                                                }
                                            }

                                            out.unwrap_or_else(|| {
                                           panic!("could not find object {id} in any index after looking up one of its base objects {base_id}" )
                                       })
                                        }
                                    };
                                    let pack = possibly_pack
                                        .as_ref()
                                        .expect("pack to still be available like just now");
                                    let entry = match pack.entry(pack_offset) {
                                        Ok(entry) => entry,
                                        Err(err) => break 'decode Err(err.into()),
                                    };
                                    let header_size = entry.header_size();
                                    pack.decode_entry(
                                        entry,
                                        buffer,
                                        inflate,
                                        &|id, out| {
                                            index_file
                                                .pack_offset_by_id(id)
                                                .and_then(|pack_offset| {
                                                    pack.entry(pack_offset)
                                                        .ok()
                                                        .map(gix_pack::data::decode::entry::ResolvedBase::InPack)
                                                })
                                                .or_else(|| {
                                                    (id == base_id).then(|| {
                                                        out.resize(buf.len(), 0);
                                                        out.copy_from_slice(buf.as_slice());
                                                        gix_pack::data::decode::entry::ResolvedBase::OutOfPack {
                                                            kind: obj_kind,
                                                            end: out.len(),
                                                        }
                                                    })
                                                })
                                        },
                                        pack_cache,
                                    )
                                    .map(move |r| {
                                        (
                                            (r.kind, pack.object_hash()),
                                            Some(gix_pack::data::entry::Location {
                                                pack_id: pack.id,
                                                pack_offset,
                                                entry_size: r.compressed_size + header_size,
                                            }),
                                        )
                                    })
                                }
                                Err(err) => Err(err),
                            };
                            res.map_err(Error::from)
                        };
                        match res {
                            Ok(((kind, object_hash), location)) => {
                                if idx != 0 {
                                    snapshot.indices.swap(0, idx);
                                }
                                let data = gix_object::Data {
                                    kind,
                                    object_hash,
                                    data: buffer.as_slice(),
                                };
                                return Ok(Some((data, location)));
                            }
                            // `packed_object_info()` failed: read the entry again as git does, which
                            // prints what git prints, and stop asking this source's packs
                            // (odb/source-packed.c:117-127).
                            Err(_) => {
                                failed = Some((pack_arc, pack_offset));
                                break 'indices;
                            }
                        }
                    }
                }
            }
            if let Some((pack, pack_offset)) = failed {
                let mut odb = Recover {
                    handle: self,
                    snapshot: &mut *snapshot,
                    inflate: &mut *inflate,
                    pack: pack.path(),
                };
                if let Some((kind, data)) =
                    self.read_bad_entry(&pack, &mut odb, |p, odb| p.read_object(pack_offset, odb))
                {
                    *buffer = data;
                    return Ok(Some((
                        gix_object::Data {
                            kind,
                            object_hash: pack.object_hash(),
                            data: buffer.as_slice(),
                        },
                        None,
                    )));
                }
                self.store.mark_bad_packed_object(pack.path(), id);
                bad_pack.get_or_insert_with(|| self.store.pack_name(pack.path()));
            } else if let Some(new_snapshot) = self.store.load_one_index(self.known_index_ctx(snapshot.marker))? {
                // `find_pack_entry()` asks every pack a source knows before its loose half is read
                // (odb/source-files.c:79-96): the indices not loaded yet come first.
                *snapshot = new_snapshot;
                self.clear_cache();
                continue;
            }

            for lodb in snapshot.loose_dbs.iter() {
                // TODO: remove this double-lookup once the borrow checker allows it.
                if lodb.contains(id) {
                    return lodb
                        .try_find(id, buffer)
                        .map(|obj| obj.map(|obj| (obj, None)))
                        .map_err(Into::into);
                }
            }

            match self.store.load_one_index(self.index_ctx(snapshot.marker))? {
                Some(new_snapshot) => {
                    *snapshot = new_snapshot;
                    self.clear_cache();
                }
                None => match self.second_read(&mut second_read, bad_pack.take(), id)? {
                    Some(pack) => bad_pack = Some(pack),
                    None => return Ok(None),
                },
            }
        }
    }

    /// The end of `do_oid_object_info_extended()`'s first pass over the sources (odb.c:575-607) for
    /// an object nothing produced. Without a pack that gave up on it, a second read finds no more
    /// than the refresh just did, so it is a plain miss. With one, the second read runs once, now
    /// passing over the object in that pack, and hands `bad_pack` back to keep it; after that the
    /// answer is that the object is corrupt.
    pub(super) fn second_read(
        &self,
        second_read: &mut bool,
        bad_pack: Option<String>,
        id: &gix_hash::oid,
    ) -> Result<Option<String>, Error> {
        match bad_pack {
            None => Ok(None),
            Some(pack) if !*second_read => {
                *second_read = true;
                Ok(Some(pack))
            }
            Some(pack) => Err(Error::PackedObjectCorrupt {
                id: id.to_owned(),
                pack,
            }),
        }
    }

    /// Read the entry `pack` failed to decode through [`packed_read`](super::packed_read) as git
    /// reads it, with `odb` answering the reads it makes of the rest of the object database.
    pub(super) fn read_bad_entry<T>(
        &self,
        pack: &gix_pack::data::File,
        odb: &mut dyn super::packed_read::Odb,
        read: impl FnOnce(&super::packed_read::Pack<'_>, &mut dyn super::packed_read::Odb) -> Option<T>,
    ) -> Option<T> {
        let data = pack.entry_slice(0..pack.data_len() as u64)?;
        let name = self.store.pack_name(pack.path());
        let index = gix_pack::index::File::at(pack.path().with_extension("idx"), pack.object_hash()).ok();
        let p = super::packed_read::Pack {
            data,
            name: &name,
            index,
            hash: pack.object_hash(),
        };
        read(&p, odb)
    }

    /// The size of `id` as `packed_object_info()` with `sizep` alone computes it (packfile.c:1308-1350),
    /// which never checks the type: `cat-file -s`'s read (builtin/cat-file.c:154-169). Loose objects and
    /// entries `gix-pack` decodes answer through [`gix_object::FindHeader`].
    pub fn try_size(&self, id: &gix_hash::oid) -> Result<Option<u64>, Error> {
        self.fetch_from_promisor(id);
        let mut snapshot = self.snapshot.borrow_mut();
        let id = match self.ignore_replacements {
            true => id,
            false => super::access::follow_replacements(&self.store.replacements, id),
        };
        let mut bad_pack: Option<String> = None;
        let mut second_read = false;
        loop {
            let mut found: Option<(std::sync::Arc<gix_pack::data::File>, u64)> = None;
            {
                let marker = snapshot.marker;
                for index in snapshot.indices.iter_mut() {
                    if let Some(handle::index_lookup::Outcome {
                        object_index: handle::IndexForObjectInPack { pack_id, pack_offset },
                        pack: possibly_pack,
                        ..
                    }) = index.lookup(id)
                    {
                        let pack = match possibly_pack {
                            Some(pack) => &*pack,
                            None => match self.store.load_pack(pack_id, marker)? {
                                Some(pack) => {
                                    *possibly_pack = Some(pack);
                                    possibly_pack.as_ref().expect("just put it in")
                                }
                                None => continue,
                            },
                        };
                        if self.store.is_bad_packed_object(pack.path(), id) {
                            bad_pack.get_or_insert_with(|| self.store.pack_name(pack.path()));
                            continue;
                        }
                        found = Some((std::sync::Arc::clone(pack), pack_offset));
                        break;
                    }
                }
            }
            if let Some((pack, pack_offset)) = found {
                let mut inflate = self.inflate.borrow_mut();
                let mut odb = Recover {
                    handle: self,
                    snapshot: &mut snapshot,
                    inflate: &mut inflate,
                    pack: pack.path(),
                };
                if let Some(size) = self.read_bad_entry(&pack, &mut odb, |p, _| p.read_size(pack_offset)) {
                    return Ok(Some(size));
                }
                self.store.mark_bad_packed_object(pack.path(), id);
                bad_pack.get_or_insert_with(|| self.store.pack_name(pack.path()));
            } else if let Some(new_snapshot) = self.store.load_one_index(self.known_index_ctx(snapshot.marker))? {
                // The indices of the packs already known come before the loose objects, as above.
                *snapshot = new_snapshot;
                self.clear_cache();
                continue;
            }
            for lodb in snapshot.loose_dbs.iter() {
                if lodb.contains(id) {
                    return lodb
                        .try_header(id)
                        .map(|hdr| hdr.map(|(size, _kind)| size))
                        .map_err(Into::into);
                }
            }
            match self.store.load_one_index(self.index_ctx(snapshot.marker))? {
                Some(new_snapshot) => {
                    *snapshot = new_snapshot;
                    self.clear_cache();
                }
                None => match self.second_read(&mut second_read, bad_pack.take(), id)? {
                    Some(pack) => bad_pack = Some(pack),
                    None => return Ok(None),
                },
            }
        }
    }

    /// Whether the copy of `id` that `index` names is one `mark_bad_packed_object()` gave up on, which
    /// `packfile_fill_entry()` and `midx_fill_entry()` then pass over (packfile.c:1846-1853, midx.c:621-626).
    fn names_bad_copy(&self, index: &mut handle::IndexLookup, id: &gix_hash::oid) -> bool {
        if self.store.bad_objects.lock().is_empty() {
            return false;
        }
        index
            .lookup(id)
            .and_then(|found| {
                found
                    .pack
                    .as_ref()
                    .map(|pack| self.store.is_bad_packed_object(pack.path(), id))
            })
            .unwrap_or(false)
    }

    pub(crate) fn clear_cache(&self) {
        self.packed_object_count.borrow_mut().take();
    }

    /// If `id` is not in this store and a promisor hook is installed, ask it for the object and make whatever
    /// it wrote visible to this handle.
    ///
    /// This is the *partial clone* read path: the clone left objects behind on purpose and the first read of
    /// one of them has to go back to the remote. `git` does the same in `oid_object_info_extended()`, which
    /// calls `promisor_remote_get_direct()` on a miss and retries the lookup once.
    ///
    /// Must be called with no borrow held on `snapshot` or `inflate`, as the hook runs arbitrary code.
    /// Doing nothing is always a valid outcome; the caller's lookup then simply reports the object missing.
    pub(super) fn fetch_from_promisor(&self, id: &gix_hash::oid) {
        if !self.store.has_promisor() || gix_pack::Find::contains(self, id) {
            return;
        }
        if !self.store.fetch_from_promisor(&[id.to_owned()]) {
            return;
        }
        // The store has folded the new pack into its slot map already. Adopt that state unconditionally
        // rather than going through `refresh`, which a caller may have turned off precisely because it
        // expected misses.
        *self.snapshot.borrow_mut() = self.store.collect_snapshot();
        self.clear_cache();
    }
}

impl<S> gix_pack::Find for super::Handle<S>
where
    S: Deref<Target = super::Store> + Clone,
{
    // TODO: probably make this method fallible, but that would mean its own error type.
    fn contains(&self, id: &gix_hash::oid) -> bool {
        let mut snapshot = self.snapshot.borrow_mut();
        loop {
            for (idx, index) in snapshot.indices.iter_mut().enumerate() {
                if index.contains(id) && !self.names_bad_copy(index, id) {
                    if idx != 0 {
                        snapshot.indices.swap(0, idx);
                    }
                    return true;
                }
            }

            for lodb in snapshot.loose_dbs.iter() {
                if lodb.contains(id) {
                    return true;
                }
            }

            match self.store.load_one_index(self.index_ctx(snapshot.marker)) {
                Ok(Some(new_snapshot)) => {
                    *snapshot = new_snapshot;
                    self.clear_cache();
                }
                Ok(None) => return false, // nothing more to load, or our refresh mode doesn't allow disk refreshes
                Err(_) => return false, // something went wrong, nothing we can communicate here with this trait. TODO: Maybe that should change?
            }
        }
    }

    fn try_find_cached<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
        pack_cache: &mut dyn DecodeEntry,
    ) -> Result<Option<(gix_object::Data<'a>, Option<gix_pack::data::entry::Location>)>, gix_object::find::Error> {
        self.fetch_from_promisor(id);
        let mut snapshot = self.snapshot.borrow_mut();
        let mut inflate = self.inflate.borrow_mut();
        match self.try_find_cached_inner(id, buffer, &mut inflate, pack_cache, &mut snapshot, None) {
            // A whole-object read is `odb_read_object()`'s, which asks with `OBJECT_INFO_DIE_IF_CORRUPT`
            // and dies with the corrupt read's message (odb.c:597-603, 744-758).
            Err(err @ Error::PackedObjectCorrupt { .. }) if !CORRUPT_READS_AS_ERRORS.get() => {
                super::packed_read::die(&err.to_string())
            }
            res => res.map_err(|err| Box::new(err) as _),
        }
    }

    fn location_by_oid(&self, id: &gix_hash::oid, buf: &mut Vec<u8>) -> Option<gix_pack::data::entry::Location> {
        assert!(
            matches!(self.token.as_ref(), Some(handle::Mode::KeepDeletedPacksAvailable)),
            "BUG: handle must be configured to `prevent_pack_unload()` before using this method"
        );

        assert!(
            self.store_ref().replacements.is_empty() || self.ignore_replacements,
            "Everything related to packing must not use replacements. These are not used here, but it should be turned off for good measure."
        );

        let mut snapshot = self.snapshot.borrow_mut();
        let mut inflate = self.inflate.borrow_mut();
        loop {
            {
                let marker = snapshot.marker;
                'indices: for (idx, index) in snapshot.indices.iter_mut().enumerate() {
                    if let Some(handle::index_lookup::Outcome {
                        object_index: handle::IndexForObjectInPack { pack_id, pack_offset },
                        index_file: _,
                        pack: possibly_pack,
                    }) = index.lookup(id)
                    {
                        let pack = match possibly_pack {
                            Some(pack) => pack,
                            None => match self.store.load_pack(pack_id, marker).ok()? {
                                Some(pack) => {
                                    *possibly_pack = Some(pack);
                                    possibly_pack.as_deref().expect("just put it in")
                                }
                                // Gone, exactly as in `try_find_cached_inner()`: keep scanning the
                                // remaining indices, and only then fall through to the refresh.
                                None => continue 'indices,
                            },
                        };
                        let entry = pack.entry(pack_offset).ok()?;
                        // This allocation is driven by on-disk pack metadata, so keep it aligned with
                        // `gix_pack::data::File::with_alloc_limit_bytes()`.
                        let size: usize = entry.decompressed_size.try_into().ok()?;
                        if pack.alloc_limit_bytes.is_some_and(|limit| size > limit) {
                            return None;
                        }
                        buf.resize(size, 0);
                        assert_eq!(pack.id, pack_id.to_intrinsic_pack_id(), "both ids must always match");

                        let res = pack
                            .decompress_entry(&entry, &mut inflate, buf)
                            .ok()
                            .map(|entry_size_past_header| gix_pack::data::entry::Location {
                                pack_id: pack.id,
                                pack_offset,
                                entry_size: entry.header_size() + entry_size_past_header,
                            });

                        if idx != 0 {
                            snapshot.indices.swap(0, idx);
                        }
                        return res;
                    }
                }
            }

            {
                let new_snapshot = self.store.load_one_index(self.index_ctx(snapshot.marker)).ok()??;
                *snapshot = new_snapshot;
                self.clear_cache();
            }
        }
    }

    fn pack_offsets_and_oid(&self, pack_id: u32) -> Option<Vec<(u64, gix_hash::ObjectId)>> {
        assert!(
            matches!(self.token.as_ref(), Some(handle::Mode::KeepDeletedPacksAvailable)),
            "BUG: handle must be configured to `prevent_pack_unload()` before using this method"
        );
        let pack_id = PackId::from_intrinsic_pack_id(pack_id);
        loop {
            let snapshot = self.snapshot.borrow();
            {
                for index in &snapshot.indices {
                    if let Some(iter) = index.iter(pack_id) {
                        return Some(iter.map(|e| (e.pack_offset, e.oid)).collect());
                    }
                }
            }

            {
                let new_snapshot = self.store.load_one_index(self.index_ctx(snapshot.marker)).ok()??;
                drop(snapshot);
                *self.snapshot.borrow_mut() = new_snapshot;
            }
        }
    }

    fn entry_by_location(&self, location: &gix_pack::data::entry::Location) -> Option<gix_pack::find::Entry> {
        assert!(
            matches!(self.token.as_ref(), Some(handle::Mode::KeepDeletedPacksAvailable)),
            "BUG: handle must be configured to `prevent_pack_unload()` before using this method"
        );
        let pack_id = PackId::from_intrinsic_pack_id(location.pack_id);
        let mut snapshot = self.snapshot.borrow_mut();
        let marker = snapshot.marker;
        loop {
            {
                for index in &mut snapshot.indices {
                    if let Some(possibly_pack) = index.pack(pack_id) {
                        let pack = match possibly_pack {
                            Some(pack) => pack,
                            None => {
                                let pack = self.store.load_pack(pack_id, marker).ok()?.expect(
                                "BUG: pack must exist from previous call to location_by_oid() and must not be unloaded",
                            );
                                *possibly_pack = Some(pack);
                                possibly_pack.as_deref().expect("just put it in")
                            }
                        };
                        return pack
                            .entry_slice(location.entry_range(location.pack_offset))
                            .map(|data| gix_pack::find::Entry {
                                data: data.to_owned(),
                                version: pack.version(),
                            });
                    }
                }
            }

            snapshot.indices.insert(
                0,
                self.store
                    .index_by_id(pack_id, marker)
                    .expect("BUG: index must always be present, must not be unloaded or overwritten"),
            );
        }
    }
}

impl<S> gix_object::Find for super::Handle<S>
where
    S: Deref<Target = super::Store> + Clone,
    Self: gix_pack::Find,
{
    fn try_find<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> Result<Option<gix_object::Data<'a>>, gix_object::find::Error> {
        gix_pack::Find::try_find(self, id, buffer).map(|t| t.map(|t| t.0))
    }
}

impl<S> gix_object::FindHeader for super::Handle<S>
where
    S: Deref<Target = super::Store> + Clone,
{
    fn try_header(&self, id: &gix_hash::oid) -> Result<Option<gix_object::Header>, gix_object::find::Error> {
        self.fetch_from_promisor(id);
        let mut snapshot = self.snapshot.borrow_mut();
        let mut inflate = self.inflate.borrow_mut();
        self.try_header_inner(id, &mut inflate, &mut snapshot, None)
            .map(|maybe_header| {
                maybe_header.map(|hdr| gix_object::Header {
                    kind: hdr.kind(),
                    size: hdr.size(),
                })
            })
            .map_err(|err| Box::new(err) as _)
    }
}

impl<S> gix_object::Exists for super::Handle<S>
where
    S: Deref<Target = super::Store> + Clone,
    Self: gix_pack::Find,
{
    fn exists(&self, id: &gix_hash::oid) -> bool {
        // `odb_has_object(..., ODB_HAS_OBJECT_FETCH_PROMISOR)`, which is what
        // `repo_has_object_file()` passes: asking whether an object is here is
        // enough to go and get it from the promisor remote, and `cat-file -e`
        // on a partial clone's skipped blob answers 0 for exactly that reason.
        // The commands that must observe an absence instead of repairing it turn
        // the hook off through `set_fetch_if_missing()`, the same lever git
        // uses.
        self.fetch_from_promisor(id);
        gix_pack::Find::contains(self, id)
    }
}

/// What [`packed_read`](super::packed_read) asks of the rest of the object database while it reads a bad
/// entry of `pack`: the reads go through the same handle, and see the objects already marked bad.
pub(super) struct Recover<'a, S>
where
    S: Deref<Target = super::Store> + Clone,
{
    pub handle: &'a super::Handle<S>,
    pub snapshot: &'a mut load_index::Snapshot,
    pub inflate: &'a mut gix_zlib::Inflate,
    pub pack: &'a std::path::Path,
}

impl<S> super::packed_read::Odb for Recover<'_, S>
where
    S: Deref<Target = super::Store> + Clone,
{
    fn read_type(&mut self, id: &gix_hash::oid) -> Option<gix_object::Kind> {
        // `odb_read_object_info()` follows replacements (odb.c:722-735).
        self.handle
            .try_header_inner(id, self.inflate, self.snapshot, None)
            .ok()
            .flatten()
            .map(|hdr| hdr.kind())
    }

    fn read_object(&mut self, id: &gix_hash::oid) -> Option<(gix_object::Kind, Vec<u8>)> {
        // `unpack_entry()` asks with no flags, so no replacement is followed (packfile.c:1617-1628).
        let mut buf = Vec::new();
        self.handle
            .try_find_cached_inner(
                id,
                &mut buf,
                self.inflate,
                &mut gix_pack::cache::Never,
                self.snapshot,
                Some(error::DeltaBaseRecursion::new(id)),
            )
            .ok()
            .flatten()
            .map(|(obj, _)| (obj.kind, obj.data.to_vec()))
    }

    fn mark_bad(&mut self, id: &gix_hash::oid) {
        self.handle.store.mark_bad_packed_object(self.pack, id);
    }
}
