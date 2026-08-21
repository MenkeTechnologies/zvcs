use std::io::Write;

use crate::{State, Version, entry, extension, write::util::CountBytes};

/// A way to specify which of the optional extensions to write.
#[derive(Default, Debug, Copy, Clone)]
pub enum Extensions {
    /// Writes all available optional extensions to avoid losing any information.
    #[default]
    All,
    /// Only write the given optional extensions, with each extension being marked by a boolean flag.
    ///
    /// # Note: mandatory extensions
    ///
    /// Mandatory extensions, like `sdir` or other lower-case ones, may not be configured here as they need to be present
    /// or absent depending on the state of the index itself and for it to be valid.
    Given {
        /// Write the tree-cache extension, if present.
        tree_cache: bool,
        /// Write the end-of-index-entry extension.
        end_of_index_entry: bool,
    },
    /// Write no optional extension at all for what should be the smallest possible index
    None,
}

impl Extensions {
    /// Returns `Some(signature)` if it should be written out.
    pub fn should_write(&self, signature: extension::Signature) -> Option<extension::Signature> {
        match self {
            Extensions::None => None,
            Extensions::All => Some(signature),
            Extensions::Given {
                tree_cache,
                end_of_index_entry,
            } => match signature {
                extension::tree::SIGNATURE => tree_cache,
                extension::end_of_index_entry::SIGNATURE => end_of_index_entry,
                // `strip_extensions` is the only gate git puts on `link` and `REUC`
                // (read-cache.c:2197 and :2222) — neither has a knob of its own, and
                // both describe state that exists nowhere else in the file. `Given`
                // is this crate's "not stripped", so both are always written when
                // the state carries them; only [`Extensions::None`] drops them.
                extension::link::SIGNATURE | extension::resolve_undo::SIGNATURE => &true,
                _ => &false,
            }
            .then(|| signature),
        }
    }
}

/// The options for use when [writing an index][State::write_to()].
///
/// Note that default options write either index V2 or V3 depending on the content of the entries.
#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    /// Configures which extensions to write.
    pub extensions: Extensions,
    /// Set the trailing hash of the produced index to all zeroes to save some time.
    ///
    /// This value is typically controlled by `index.skipHash` and is respected when the index is written
    /// via [`File::write()`](crate::File::write()) and [`File::write_to()`](crate::File::write_to()).
    /// Note that
    pub skip_hash: bool,
}

impl State {
    /// Serialize this instance to `out` with [`options`][Options].
    ///
    /// Note that the `tree` (tree-cache) extension is written as-is and is **not** recomputed or
    /// invalidated to match the entries; see [`File::write()`](crate::File::write()) for the
    /// implications and for the two ways to keep it honest.
    pub fn write_to(
        &self,
        out: impl std::io::Write,
        Options {
            extensions,
            skip_hash: _,
        }: Options,
    ) -> Result<Version, gix_hash::io::Error> {
        let _span = gix_features::trace::detail!("gix_index::State::write()");
        let version = self.detect_required_version();

        let mut write = CountBytes::new(out);
        let num_entries: u32 = self
            .entries()
            .len()
            .try_into()
            .expect("definitely not 4billion entries");
        let removed_entries: u32 = self
            .entries()
            .iter()
            .filter(|e| e.flags.contains(entry::Flags::REMOVE))
            .count()
            .try_into()
            .expect("definitely not too many entries");

        let offset_to_entries = header(&mut write, version, num_entries - removed_entries)?;
        // `if (nr_threads != 1 && record_ieot()) { ... ieot_entries = DIV_ROUND_UP(...) }`
        // (read-cache.c:2877-2904) is computed before a single entry is written, because the
        // block boundaries are decided while writing them.
        let entries_per_block = self
            .offset_table_threads
            .and_then(|threads| extension::index_entry_offset_table::entries_per_block(threads, num_entries));
        let (offset_to_extensions, offsets) = entries(&mut write, self, offset_to_entries, entries_per_block)?;
        let (extension_toc, out) = self.write_extensions(write, offset_to_extensions, extensions, &offsets)?;

        if num_entries > 0
            && extensions
                .should_write(extension::end_of_index_entry::SIGNATURE)
                .is_some()
            && !extension_toc.is_empty()
        {
            extension::end_of_index_entry::write_to(out, self.object_hash, offset_to_extensions, extension_toc)?;
        }

        Ok(version)
    }

    fn write_extensions<T>(
        &self,
        mut write: CountBytes<T>,
        offset_to_extensions: u32,
        extensions: Extensions,
        offsets: &[extension::index_entry_offset_table::Offset],
    ) -> std::io::Result<(Vec<(extension::Signature, u32)>, T)>
    where
        T: std::io::Write,
    {
        type WriteExtFn<'a> = &'a dyn Fn(&mut dyn std::io::Write) -> Option<std::io::Result<extension::Signature>>;
        let extensions: &[WriteExtFn<'_>] = &[
            // "Lets write out CACHE_EXT_INDEXENTRYOFFSETTABLE first so that we can minimize the
            // number of extensions we have to scan through to find it during load. Write it out
            // regardless of the strip_extensions parameter as we need it when loading the shared
            // index." (read-cache.c:2975-2990) — hence both its position at the head of this list
            // and its immunity to the [`Extensions`] filter, which is this crate's
            // `strip_extensions`. Nothing is written unless the block plan produced blocks, so
            // the default (no `index.threads`) stays byte-for-byte what it was.
            &|write| {
                (!offsets.is_empty()).then(|| {
                    let signature = extension::index_entry_offset_table::SIGNATURE;
                    write.write_all(&signature)?;
                    let size = 4 + 8 * u32::try_from(offsets.len()).expect("far fewer than 4 billion blocks");
                    write.write_all(&size.to_be_bytes())?;
                    extension::index_entry_offset_table::write_to(write, offsets).map(|()| signature)
                })
            },
            // `if (!strip_extensions && istate->split_index && !is_null_oid(&istate->split_index->base_oid))`
            // (read-cache.c:2197) — ahead of the tree-cache, and only ever present
            // when the caller deliberately split this index
            // ([`State::set_link()`](State::set_link())).
            &|write| {
                extensions
                    .should_write(extension::link::SIGNATURE)
                    .and_then(|signature| {
                        self.link()
                            .filter(|link| !link.shared_index_checksum.is_null())
                            .map(|link| extension::link::write_to(link, write).map(|()| signature))
                    })
            },
            &|write| {
                extensions
                    .should_write(extension::tree::SIGNATURE)
                    .and_then(|signature| self.tree().map(|tree| tree.write_to(write).map(|_| signature)))
            },
            // `if (!strip_extensions && istate->resolve_undo)` (read-cache.c:2222),
            // written after the tree-cache and before the untracked cache. It is
            // emitted whenever the state carries the extension, empty record list
            // included, because git's condition is on the pointer and not on its
            // contents.
            &|write| {
                extensions
                    .should_write(extension::resolve_undo::SIGNATURE)
                    .and_then(|signature| {
                        self.resolve_undo()
                            .map(|paths| extension::resolve_undo::write_to(paths, write).map(|()| signature))
                    })
            },
            &|write| {
                self.is_sparse()
                    .then(|| extension::sparse::write_to(write).map(|_| extension::sparse::SIGNATURE))
            },
        ];

        let mut offset_to_previous_ext = offset_to_extensions;
        let mut out = Vec::with_capacity(5);
        for write_ext in extensions {
            if let Some(signature) = write_ext(&mut write).transpose()? {
                let offset_past_ext = write.count;
                let ext_size = offset_past_ext - offset_to_previous_ext - (extension::MIN_SIZE as u32);
                offset_to_previous_ext = offset_past_ext;
                out.push((signature, ext_size));
            }
        }
        Ok((out, write.inner))
    }
}

impl State {
    fn detect_required_version(&self) -> Version {
        self.entries
            .iter()
            .find_map(|e| e.flags.contains(entry::Flags::EXTENDED).then_some(Version::V3))
            .unwrap_or(Version::V2)
    }
}

fn header<T: std::io::Write>(
    out: &mut CountBytes<T>,
    version: Version,
    num_entries: u32,
) -> Result<u32, std::io::Error> {
    let version = match version {
        Version::V2 => 2_u32.to_be_bytes(),
        Version::V3 => 3_u32.to_be_bytes(),
        Version::V4 => 4_u32.to_be_bytes(),
    };

    out.write_all(crate::decode::header::SIGNATURE)?;
    out.write_all(&version)?;
    out.write_all(&num_entries.to_be_bytes())?;

    Ok(out.count)
}

/// Write every entry that survives to disk, returning the offset one past the last of them and
/// the `IEOT` block table that was recorded on the way.
///
/// `entries_per_block` is `ieot_entries` from `do_write_index()`; `None` records nothing and is
/// what every write without `index.threads` does. The block bookkeeping is the port of
/// read-cache.c:2911-2957, and three details of it are load-bearing:
///
/// * the boundary test is on the *index* into `state.entries()` (`i % ieot_entries == 0`), not
///   on how many entries have been written, so entries flagged `CE_REMOVE` still advance it;
/// * that test sits *after* the `CE_REMOVE` `continue` (read-cache.c:2915-2916), so a removed
///   entry landing exactly on a boundary does not open a block — a quirk, reproduced;
/// * a block's offset is taken when the block opens, i.e. after the previous entry's padding
///   has been written, matching git's `offset = hashfile_total(f)` (read-cache.c:2941).
fn entries<T: std::io::Write>(
    out: &mut CountBytes<T>,
    state: &State,
    header_size: u32,
    entries_per_block: Option<u32>,
) -> Result<(u32, Vec<extension::index_entry_offset_table::Offset>), std::io::Error> {
    use extension::index_entry_offset_table::Offset;

    let mut offsets = Vec::new();
    // `offset = hashfile_total(f);` right after the header (read-cache.c:2906) — the first
    // block starts at the first entry.
    let mut block_offset = out.count;
    let mut block_entries = 0_u32;

    for (index, entry) in state.entries().iter().enumerate() {
        if entry.flags.contains(entry::Flags::REMOVE) {
            continue;
        }
        if let Some(per_block) = entries_per_block {
            let index = u32::try_from(index).expect("definitely not 4billion entries");
            if index != 0 && index % per_block == 0 {
                offsets.push(Offset {
                    from_beginning_of_file: block_offset,
                    num_entries: block_entries,
                });
                block_entries = 0;
                block_offset = out.count;
            }
        }
        entry.write_to(&mut *out, state)?;
        match (out.count - header_size) % 8 {
            0 => {}
            n => {
                let eight_null_bytes = [0u8; 8];
                out.write_all(&eight_null_bytes[n as usize..])?;
            }
        }
        block_entries += 1;
    }

    // `if (ieot && nr) { ... }` (read-cache.c:2953-2957): the last block is only recorded if
    // it holds something, so an index whose tail entries were all removed cannot contribute an
    // empty block.
    if entries_per_block.is_some() && block_entries != 0 {
        offsets.push(Offset {
            from_beginning_of_file: block_offset,
            num_entries: block_entries,
        });
    }

    Ok((out.count, offsets))
}

mod util {
    pub struct CountBytes<T> {
        pub count: u32,
        pub inner: T,
    }

    impl<T> CountBytes<T>
    where
        T: std::io::Write,
    {
        pub fn new(inner: T) -> Self {
            CountBytes { inner, count: 0 }
        }
    }

    impl<T> std::io::Write for CountBytes<T>
    where
        T: std::io::Write,
    {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let written = self.inner.write(buf)?;
            self.count = self
                .count
                .checked_add(u32::try_from(written).expect("we don't write 4GB buffers"))
                .ok_or_else(|| std::io::Error::other("Cannot write indices larger than 4 gigabytes"))?;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
}
