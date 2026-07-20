//! Writing of pack reverse indices (`.rev`), the permutation that maps a pack's
//! entries in *offset* order onto their positions in the `.idx`'s *name* order.
//!
//! git writes one of these next to every pack it creates when
//! `pack.writeReverseIndex` is set, which has been the default since git 2.41.
//! Only the reader side of this format was needed until now, and gitoxide has
//! neither — so this is the writer, kept deliberately small.
//!
//! The on-disk layout, per `Documentation/gitformat-pack.txt`:
//!
//! | bytes      | meaning                                                  |
//! |------------|----------------------------------------------------------|
//! | 4          | the signature `RIDX`                                     |
//! | 4          | version, always `1`                                      |
//! | 4          | hash identifier: `1` for SHA-1, `2` for SHA-256          |
//! | `4 * N`    | for each entry in ascending pack-offset order, its index |
//! |            | position within the `.idx` (i.e. in object-name order)   |
//! | `hash_len` | the checksum of the pack this index belongs to           |
//! | `hash_len` | the checksum over all preceding bytes of this file       |

use std::io::Write;

use crate::index;

/// The error returned by [`write_reverse_index()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("could not write reverse index")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Hasher(#[from] gix_hash::hasher::Error),
}

/// git's on-disk identifier for a hash function, as the `.rev` header carries it.
fn hash_id(kind: gix_hash::Kind) -> u32 {
    match kind {
        gix_hash::Kind::Sha1 => 1,
        _ => 2,
    }
}

/// Write the reverse index of `index` to `out`.
///
/// The permutation is derived from `index` alone: its entries are enumerated in
/// name order — which is the order the `.idx` stores them, so an entry's
/// position in that enumeration *is* its index position — and then sorted by
/// pack offset. Ties cannot occur, as two entries never share an offset.
pub fn write_reverse_index<T>(index: &index::File<T>, out: &mut dyn Write) -> Result<(), Error>
where
    T: crate::FileData + Sync,
{
    let object_hash = index.object_hash();
    let mut by_offset: Vec<(crate::data::Offset, u32)> = index
        .iter()
        .enumerate()
        .map(|(index_position, entry)| (entry.pack_offset, index_position as u32))
        .collect();
    by_offset.sort_unstable_by_key(|(pack_offset, _)| *pack_offset);

    let mut bytes = Vec::with_capacity(12 + by_offset.len() * 4 + object_hash.len_in_bytes() * 2);
    bytes.extend_from_slice(b"RIDX");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&hash_id(object_hash).to_be_bytes());
    for (_, index_position) in &by_offset {
        bytes.extend_from_slice(&index_position.to_be_bytes());
    }
    bytes.extend_from_slice(index.pack_checksum().as_slice());

    let mut hasher = gix_hash::hasher(object_hash);
    hasher.update(&bytes);
    bytes.extend_from_slice(hasher.try_finalize()?.as_slice());

    out.write_all(&bytes)?;
    Ok(())
}
