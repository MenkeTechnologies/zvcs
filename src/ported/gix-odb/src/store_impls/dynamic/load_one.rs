use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use crate::store::{handle, types};

impl super::Store {
    /// If Ok(None) is returned, the pack-id was stale and referred to an unloaded pack or a pack which couldn't be
    /// loaded as its file didn't exist on disk anymore.
    /// If the oid is known, just load indices again to continue
    /// (objects rarely ever removed so should be present, maybe in another pack though),
    /// and redo the entire lookup for a valid pack id whose pack can probably be loaded next time.
    pub(crate) fn load_pack(
        &self,
        id: types::PackId,
        marker: types::SlotIndexMarker,
    ) -> std::io::Result<Option<Arc<gix_pack::data::File>>> {
        let index = self.index.load();
        if index.generation != marker.generation {
            return Ok(None);
        }
        fn load_pack(
            path: &Path,
            id: types::PackId,
            object_hash: gix_hash::Kind,
            alloc_limit_bytes: Option<usize>,
        ) -> std::io::Result<Arc<gix_pack::data::File>> {
            gix_pack::data::File::at(path, object_hash)
                .map(|pack| pack.with_alloc_limit_bytes(alloc_limit_bytes))
                .map(|mut pack| {
                    pack.id = id.to_intrinsic_pack_id();
                    Arc::new(pack)
                })
                .map_err(|err| match err {
                    gix_pack::data::header::decode::Error::Io { source, .. } => source,
                    other => std::io::Error::other(other),
                })
        }

        let slot = &self.files[id.index];
        // pin the current state before loading in the generation. That way we won't risk seeing the wrong value later.
        let slot_files = &**slot.files.load();
        if slot.generation.load(Ordering::SeqCst) > marker.generation {
            // There is a disk consolidation in progress which just overwrote a slot that could be disposed with some other
            // pack, one we didn't intend to load.
            // Hope that when the caller returns/retries the new index is set so they can fetch it and retry.
            return Ok(None);
        }
        match id.multipack_index {
            None => {
                match slot_files {
                    Some(types::IndexAndPacks::Index(bundle)) => {
                        match bundle.data.loaded() {
                            Some(pack) => Ok(Some(pack.clone())),
                            None => {
                                // `is_pack_valid()` opens a pack it holds nothing of to prove it valid before
                                // `packfile_fill_entry()` names it (packfile.c:1820-1866); one that fails stays
                                // unopened, so the next lookup tries it again.
                                let index = bundle.index.loaded().map(|index| &**index);
                                if !self.is_pack_valid(bundle.data.path(), index) {
                                    return Ok(None);
                                }
                                let _lock = slot.write.lock();
                                let mut files = slot.files.load_full();
                                let files_mut = Arc::make_mut(&mut files);
                                let pack = match files_mut {
                                    Some(types::IndexAndPacks::Index(bundle)) => {
                                        bundle.data.load_with_recovery(|path| {
                                            load_pack(path, id, self.object_hash, self.alloc_limit_bytes)
                                        })?
                                    }
                                    Some(types::IndexAndPacks::MultiIndex(_)) => {
                                        // something changed between us getting the lock, trigger a complete index refresh.
                                        None
                                    }
                                    None => None,
                                };
                                slot.files.store(files);
                                Ok(pack)
                            }
                        }
                    }
                    // This can also happen if they use an old index into our new and refreshed data which might have a multi-index
                    // here.
                    Some(types::IndexAndPacks::MultiIndex(_)) => Ok(None),
                    None => Ok(None),
                }
            }
            Some(pack_index) => {
                match slot_files {
                    Some(types::IndexAndPacks::MultiIndex(bundle)) => {
                        match bundle.data.get(pack_index as usize) {
                            None => Ok(None), // somewhat unexpected, data must be stale
                            Some(on_disk_pack) => match on_disk_pack.loaded() {
                                Some(pack) => Ok(Some(pack.clone())),
                                None => {
                                    // `midx_fill_entry()` asks `is_pack_valid()` the same (midx.c:618-619).
                                    if !self.is_pack_valid(on_disk_pack.path(), None) {
                                        return Ok(None);
                                    }
                                    let _lock = slot.write.lock();
                                    let mut files = slot.files.load_full();
                                    let files_mut = Arc::make_mut(&mut files);
                                    let pack = match files_mut {
                                        Some(types::IndexAndPacks::Index(_)) => {
                                            // something changed between us getting the lock, trigger a complete index refresh.
                                            None
                                        }
                                        Some(types::IndexAndPacks::MultiIndex(bundle)) => bundle
                                            .data
                                            .get_mut(pack_index as usize)
                                            .expect("BUG: must set this handle to be stable")
                                            .load_with_recovery(|path| {
                                                load_pack(path, id, self.object_hash, self.alloc_limit_bytes)
                                            })?,
                                        None => None,
                                    };
                                    slot.files.store(files);
                                    Ok(pack)
                                }
                            },
                        }
                    }
                    // This can also happen if they use an old index into our new and refreshed data which might have a multi-index
                    // here.
                    Some(types::IndexAndPacks::Index(_)) => Ok(None),
                    None => Ok(None),
                }
            }
        }
    }

    /// Similar to `.load_pack()`, but for entire indices, bypassing the index entirely and going solely by marker and id.
    /// Returns `None` if the index wasn't available anymore or could otherwise not be loaded, which can be considered a bug
    /// as we should always keep needed indices available.
    pub(crate) fn index_by_id(&self, id: types::PackId, marker: types::SlotIndexMarker) -> Option<handle::IndexLookup> {
        let slot = self.files.get(id.index)?;
        // Pin this value before we check the generation to avoid seeing something newer later.
        let slot_files = &**slot.files.load();
        if slot.generation.load(Ordering::SeqCst) > marker.generation {
            // This means somebody just overwrote our trashed slot with a new (or about to be stored) index, which means the slot isn't
            // what we need it to be.
            // This shouldn't as we won't overwrite slots while handles need stable indices.
            return None;
        }
        let lookup = match (slot_files).as_ref()? {
            types::IndexAndPacks::Index(bundle) => handle::SingleOrMultiIndex::Single {
                index: bundle.index.loaded()?.clone(),
                data: bundle.data.loaded().cloned(),
            },
            types::IndexAndPacks::MultiIndex(multi) => handle::SingleOrMultiIndex::Multi {
                index: multi.multi_index.loaded()?.clone(),
                data: multi.data.iter().map(|f| f.loaded().cloned()).collect(),
            },
        };
        handle::IndexLookup {
            id: id.index,
            file: lookup,
        }
        .into()
    }
}

impl super::Store {
    /// `is_pack_valid()` for a pack this store holds no map of yet: `open_packed_git()` proves it valid
    /// (packfile.c:1820-1838), and a pack that fails is left closed, so every later lookup that lands
    /// in it opens it, and reports it, again. `index` is the pack's own `.idx` when the caller holds
    /// it; otherwise it is read from beside the pack.
    pub fn is_pack_valid(&self, pack: &Path, index: Option<&gix_pack::index::File>) -> bool {
        match self.open_packed_git_1(pack, index) {
            Ok(()) => true,
            Err(Some(msg)) => {
                super::packed_read::error(&msg);
                self.invalid_pack_opens.fetch_add(1, Ordering::Relaxed);
                false
            }
            // `git_open()` or `fstat()` failing returns -1 with nothing printed (packfile.c:555-557);
            // the load that follows reports a pack that is gone as missing.
            Err(None) => true,
        }
    }

    /// `open_packed_git_1()` (packfile.c:529-596): the checks a pack has to pass before any of it is
    /// read, the header against the format and the pack's `.idx`, and the trailing checksum against the
    /// pack checksum the `.idx` records. `Err(Some(msg))` is the `error()` it returns with.
    fn open_packed_git_1(&self, pack: &Path, index: Option<&gix_pack::index::File>) -> Result<(), Option<String>> {
        use std::io::Read;
        use std::os::unix::fs::FileExt;

        let name = self.pack_name(pack);
        // `open_pack_index()` maps the `.idx` beside the pack when a multi-pack-index named it.
        let loaded;
        let index = match index {
            Some(index) => index,
            None => match gix_pack::index::File::at(pack.with_extension("idx"), self.object_hash) {
                Ok(index) => {
                    loaded = index;
                    &loaded
                }
                Err(_) => return Err(Some(format!("packfile {name} index unavailable"))),
            },
        };
        let mut file = std::fs::File::open(pack).map_err(|_| None)?;
        let pack_size = file.metadata().map_err(|_| None)?.len();
        let reading = |err: std::io::Error| Some(format!("error reading from {name}: {}", strerror(&err)));

        let mut hdr = [0u8; 12];
        let mut got = 0;
        while got < hdr.len() {
            match file.read(&mut hdr[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(reading(err)),
            }
        }
        if got != hdr.len() {
            return Err(Some(format!("file {name} is far too short to be a packfile")));
        }
        if &hdr[..4] != b"PACK" {
            return Err(Some(format!("file {name} is not a GIT packfile")));
        }
        let version = u32::from_be_bytes(hdr[4..8].try_into().expect("four bytes"));
        // `pack_version_ok()`: versions 2 and 3 (packfile.h).
        if version != 2 && version != 3 {
            return Err(Some(format!(
                "packfile {name} is version {version} and not supported (try upgrading GIT to a newer version)"
            )));
        }
        let entries = u32::from_be_bytes(hdr[8..12].try_into().expect("four bytes"));
        if index.num_objects() != entries {
            return Err(Some(format!(
                "packfile {name} claims to have {entries} objects while index indicates {} objects",
                index.num_objects()
            )));
        }
        let hashsz = self.object_hash.len_in_bytes() as u64;
        // `pread()` at a negative offset is EINVAL.
        let Some(at) = pack_size.checked_sub(hashsz) else {
            return Err(reading(std::io::Error::from_raw_os_error(EINVAL)));
        };
        let mut hash = vec![0u8; hashsz as usize];
        let mut got = 0;
        while got < hash.len() {
            match file.read_at(&mut hash[got..], at + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(reading(err)),
            }
        }
        if got != hash.len() {
            return Err(Some(format!("packfile {name} signature is unavailable")));
        }
        if hash != index.pack_checksum().as_bytes() {
            return Err(Some(format!("packfile {name} does not match index")));
        }
        Ok(())
    }
}

/// `EINVAL`, 22 on the Linux and macOS libcs.
const EINVAL: i32 = 22;

/// `strerror()` of `err`: Rust's rendering without the ` (os error N)` it appends.
fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match (err.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(at)) => text[..at].to_owned(),
        _ => text,
    }
}
