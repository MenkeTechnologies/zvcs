use crate::extension::{Link, Signature};

/// The signature of the link extension.
pub const SIGNATURE: Signature = *b"link";

/// Bitmaps to know which entries to delete or replace, even though details are still unknown.
#[derive(Clone)]
pub struct Bitmaps {
    /// A bitmap to signal which entries to delete, maybe.
    pub delete: gix_bitmap::ewah::Vec,
    /// A bitmap to signal which entries to replace, maybe.
    pub replace: gix_bitmap::ewah::Vec,
}

///
pub mod decode {

    /// The error returned when decoding link extensions.
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("{0}")]
        Corrupt(&'static str),
        #[error("{kind} bitmap corrupt")]
        BitmapDecode {
            err: gix_bitmap::ewah::decode::Error,
            kind: &'static str,
        },
    }

    impl From<std::num::TryFromIntError> for Error {
        fn from(_: std::num::TryFromIntError) -> Self {
            Self::Corrupt("error in bitmap iteration trying to convert from u64 to usize")
        }
    }
}

pub(crate) fn decode(data: &[u8], object_hash: gix_hash::Kind) -> Result<Link, decode::Error> {
    let (id, data) = data
        .split_at_checked(object_hash.len_in_bytes())
        .ok_or(decode::Error::Corrupt(
            "link extension too short to read share index checksum",
        ))
        .map(|(id, d)| (gix_hash::ObjectId::from_bytes_or_panic(id), d))?;

    if data.is_empty() {
        return Ok(Link {
            shared_index_checksum: id,
            bitmaps: None,
        });
    }

    let (delete, data) =
        gix_bitmap::ewah::decode(data).map_err(|err| decode::Error::BitmapDecode { kind: "delete", err })?;
    let (replace, data) =
        gix_bitmap::ewah::decode(data).map_err(|err| decode::Error::BitmapDecode { kind: "replace", err })?;

    if !data.is_empty() {
        return Err(decode::Error::Corrupt("garbage trailing link extension"));
    }

    Ok(Link {
        shared_index_checksum: id,
        bitmaps: Some(Bitmaps { delete, replace }),
    })
}

/// Where a `sharedindex.<id>` file may live, in the order git looks
/// (read-cache.c:1888-1906).
///
/// `read_index_from()` builds `"%s/sharedindex.%s"` from the *git directory* it
/// was handed (read-cache.c:1893) and only if that file is missing does it retry
/// against the directory the index itself is in:
///
/// ```text
/// base_path2 = xstrfmt("%s/sharedindex.%s", dirname(path_copy), base_oid_hex);
/// ```
/// (read-cache.c:1901-1902)
///
/// The distinction only shows up when the two directories differ, which is
/// exactly what `GIT_INDEX_FILE` pointing outside `$GIT_DIR` does — and what a
/// worktree's `$GIT_DIR/worktrees/<name>/index` does. Resolving only against the
/// index's own directory makes every such repository unreadable, so both are
/// tried here, in git's order.
fn shared_index_candidates(
    index_path: &std::path::Path,
    git_dir: Option<&std::path::Path>,
    checksum: gix_hash::ObjectId,
) -> Vec<std::path::PathBuf> {
    let file_name = format!("sharedindex.{checksum}");
    let mut out = Vec::with_capacity(2);
    if let Some(git_dir) = git_dir {
        out.push(git_dir.join(&file_name));
    }
    if let Some(dir) = index_path.parent() {
        let fallback = dir.join(&file_name);
        if !out.contains(&fallback) {
            out.push(fallback);
        }
    }
    out
}

impl Link {
    pub(crate) fn dissolve_into(
        self,
        split_index: &mut crate::File,
        git_dir: Option<&std::path::Path>,
        object_hash: gix_hash::Kind,
        skip_hash: bool,
        options: crate::decode::Options,
    ) -> Result<(), crate::file::init::Error> {
        let options = crate::decode::Options {
            expected_checksum: self.shared_index_checksum.into(),
            ..options
        };
        let candidates = shared_index_candidates(&split_index.path, git_dir, self.shared_index_checksum);
        let mut shared_index = None;
        let mut last_err = None;
        for candidate in &candidates {
            match crate::File::at(candidate, object_hash, skip_hash, options) {
                Ok(file) => {
                    shared_index = Some(file);
                    break;
                }
                // Only a missing file moves on to the next candidate; a shared index
                // that exists but does not decode is an error about *that* file, and
                // git reports it rather than silently looking elsewhere.
                Err(crate::file::init::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    last_err = Some(crate::file::init::Error::Io(err));
                }
                Err(err) => return Err(err),
            }
        }
        let mut shared_index = match shared_index {
            Some(file) => file,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    crate::file::init::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "Could not find shared index 'sharedindex.{}' referenced by the split index at '{}'",
                            self.shared_index_checksum,
                            split_index.path.display()
                        ),
                    ))
                }));
            }
        };

        if let Some(bitmaps) = self.bitmaps {
            let mut split_entry_index = 0;
            // git's `si->base->cache_nr` before `merge_base_index()` appends the split
            // half's own entries, which is the width both bitmaps address.
            let base_len = shared_index.entries.len();
            let mut replaced = vec![false; base_len];
            let mut removed = vec![false; base_len];

            let mut err = None;
            if bitmaps.replace.for_each_set_bit(|replace_index| {
                let shared_entry = match shared_index.entries.get_mut(replace_index) {
                    Some(e) => e,
                    None => {
                        err = decode::Error::Corrupt("replace bitmap length exceeds shared index length - more entries in bitmap than found in shared index").into();
                        return None
                    }
                };

                if shared_entry.flags.contains(crate::entry::Flags::REMOVE) {
                    err = decode::Error::Corrupt("entry is marked as both replace and delete").into();
                    return None
                }

                let split_entry = match split_index.entries.get(split_entry_index) {
                    Some(e) => e,
                    None => {
                        err = decode::Error::Corrupt("replace bitmap length exceeds split index length - more entries in bitmap than found in split index").into();
                        return None
                    }
                };
                if !split_entry.path.is_empty() {
                    err = decode::Error::Corrupt("paths in split index entries that are for replacement should be empty").into();
                    return None
                }
                if shared_entry.path.is_empty() {
                    err = decode::Error::Corrupt("paths in shared index entries that are replaced should not be empty").into();
                    return None
                }
                shared_entry.stat = split_entry.stat;
                shared_entry.id = split_entry.id;
                // `src->ce_flags |= CE_UPDATE_IN_BASE` before `copy_cache_entry(dst, src)`
                // (split-index.c:151-153), and that `memcpy` spans `ce_flags`: a base entry
                // the split half replaced carries the flag from the moment it is read, which
                // is why it keeps a stand-in on the next write even when nothing touched it —
                // and why `ls-files --debug` prints `flags: 8000000` for it.
                shared_entry.flags = split_entry.flags | crate::entry::Flags::UPDATE_IN_BASE;
                shared_entry.mode = split_entry.mode;
                replaced[replace_index] = true;

                split_entry_index += 1;
                Some(())
            }).is_none() && err.is_none() {
                err = decode::Error::Corrupt("replace bitmap is malformed").into();
            }
            if let Some(err) = err {
                return Err(err.into());
            }

            let split_index_path_backing = std::mem::take(&mut split_index.path_backing);
            for mut split_entry in split_index.entries.drain(split_entry_index..) {
                let start = shared_index.path_backing.len();
                let split_index_path = split_entry.path.clone();

                split_entry.path = start..start + split_entry.path.len();
                shared_index.entries.push(split_entry);

                shared_index
                    .path_backing
                    .extend_from_slice(&split_index_path_backing[split_index_path]);
            }

            if bitmaps.delete.for_each_set_bit(|delete_index| {
                let shared_entry = match shared_index.entries.get_mut(delete_index) {
                    Some(e) => e,
                    None => {
                        err = decode::Error::Corrupt("delete bitmap length exceeds shared index length - more entries in bitmap than found in shared index").into();
                        return None
                    }
                };
                shared_entry.flags.insert(crate::entry::Flags::REMOVE);
                if delete_index < base_len {
                    removed[delete_index] = true;
                }
                Some(())
            }).is_none() && err.is_none() {
                err = decode::Error::Corrupt("delete bitmap is malformed").into();
            }
            if let Some(err) = err {
                return Err(err.into());
            }

            // git's `si->base` survives the merge — `merge_base_index()` only takes
            // `si->delete_bitmap`/`si->replace_bitmap` down, never the base itself
            // (split-index.c:195-199) — and `prepare_to_write_split_index()` walks it again
            // on the next write. This crate cannot leave the base entries shared with the
            // merged ones, so it keeps its own copy of them here, in base order and with the
            // replacements already applied, exactly as `si->base->cache[]` stands.
            let mut si = crate::SplitIndex::from_written_shared_index(
                self.shared_index_checksum,
                &shared_index.entries[..base_len],
                &shared_index.path_backing,
            );
            for (base, (replaced, removed)) in si.base.iter_mut().zip(replaced.iter().zip(removed.iter())) {
                base.replaced = *replaced;
                base.removed = *removed;
            }
            split_index.state.split_index = Some(si);

            shared_index
                .entries
                .retain(|e| !e.flags.contains(crate::entry::Flags::REMOVE));

            let mut shared_entries = std::mem::take(&mut shared_index.entries);
            shared_entries.sort_by(|a, b| a.cmp(b, &shared_index.state));

            split_index.entries = shared_entries;
            split_index.path_backing = std::mem::take(&mut shared_index.path_backing);
        }

        Ok(())
    }
}

/// Serialize `link` to `out` including the extension's signature and size header,
/// git's `write_link_extension()` (split-index.c:83-91).
///
/// The order is the one `read_link_extension()` (read-cache.c:1830-1861) parses:
/// the shared index's checksum, then the delete bitmap, then the replace bitmap.
/// Both bitmaps are optional as a pair — git writes them only
/// `if (!si->base || is_null_oid(&si->base_oid))` is false and the bitmaps exist,
/// and its reader returns successfully the moment the size is exhausted after the
/// object id (`if (!sz) return 0;`).
pub fn write_to(link: &Link, mut out: impl std::io::Write) -> std::io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(link.shared_index_checksum.as_slice());
    if let Some(bitmaps) = &link.bitmaps {
        bitmaps.delete.write_to(&mut body)?;
        bitmaps.replace.write_to(&mut body)?;
    }

    out.write_all(&SIGNATURE)?;
    let size = u32::try_from(body.len())
        .map_err(|_| std::io::Error::other("link extension exceeds 4 gigabytes"))?;
    out.write_all(&size.to_be_bytes())?;
    out.write_all(&body)
}
