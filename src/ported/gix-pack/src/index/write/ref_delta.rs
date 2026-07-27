//! Resolution of `OBJ_REF_DELTA` entries whose base object lives in the very same pack.
//!
//! A pack may name a delta base by object id instead of by offset — that is what `pack.useOfsDelta=false` produces, and
//! what `pack-objects` emits whenever the receiver never asked for `--delta-base-offset`. `index-pack.c` copes with it
//! by collecting those entries in `ref_deltas` during `parse_pack_objects()` and only linking them to their base in
//! `resolve_deltas()`, at which point the base object's id is known because the base has just been reconstructed.
//!
//! This module performs that same linking step: it walks the pack from its undeltified objects outwards, reconstructing
//! every object and recording, for each ref-delta, the pack offset its base was found at. The result lets the delta tree
//! be linked purely by offset, which is all the traversal that computes the index understands.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

use gix_hash::ObjectId;

use crate::{
    cache::delta::traverse::Error,
    data::{self, EntryRange},
};

/// The base offset each ref-delta entry resolved to, keyed by the ref-delta's own pack offset.
pub(super) type BasesByChildOffset = HashMap<data::Offset, data::Offset>;

/// A reconstructed object waiting for its children to be resolved against it.
struct Base {
    /// Index into `ranges` of the entry this object was decoded from.
    index: usize,
    /// The object type, which every delta built on top of this object inherits.
    kind: gix_object::Kind,
    /// The id of the object, which ref-deltas name their base by.
    id: ObjectId,
    /// The fully reconstructed object data a delta is applied to.
    data: Vec<u8>,
}

/// Reconstruct every object in the pack described by `ranges` and return, for each `OBJ_REF_DELTA` entry, the pack
/// offset of the entry holding its base object.
///
/// Ref-deltas whose base is not in the pack are simply absent from the result; the caller decides how to report them.
///
/// * `resolve` and `pack` provide the raw bytes of an entry, exactly as they do for [`crate::cache::delta::Tree::traverse()`].
/// * `ranges` are the byte ranges of all entries, sorted by pack offset.
/// * `num_ref_deltas` is how many ref-delta entries the pack holds, used to stop walking once all of them are resolved.
pub(super) fn resolve_bases<F, R>(
    resolve: F,
    pack: &R,
    ranges: &[EntryRange],
    num_ref_deltas: usize,
    object_hash: gix_hash::Kind,
    alloc_limit_bytes: Option<usize>,
    should_interrupt: &AtomicBool,
) -> Result<BasesByChildOffset, Error>
where
    R: Send + Sync,
    F: for<'r> Fn(EntryRange, &'r R) -> Option<&'r [u8]> + Send + Clone,
{
    let index_by_offset: HashMap<data::Offset, usize> =
        ranges.iter().enumerate().map(|(i, r)| (r.start, i)).collect();

    let mut roots = Vec::new();
    let mut ofs_children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut ref_children: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (index, range) in ranges.iter().enumerate() {
        let bytes = resolve(range.clone(), pack).ok_or(Error::ResolveFailed { pack_offset: range.start })?;
        match data::Entry::from_bytes(bytes, range.start, object_hash)?.header {
            data::entry::Header::OfsDelta { base_distance } => {
                let base_offset = data::entry::Header::verified_base_pack_offset(range.start, base_distance)
                    .ok_or(Error::ResolveFailed { pack_offset: range.start })?;
                let base_index = *index_by_offset
                    .get(&base_offset)
                    .ok_or(Error::OutOfPackRefDelta { base_pack_offset: base_offset })?;
                ofs_children.entry(base_index).or_default().push(index);
            }
            data::entry::Header::RefDelta { base_id } => ref_children.entry(base_id).or_default().push(index),
            _ => roots.push(index),
        }
    }

    let mut bases = BasesByChildOffset::with_capacity(num_ref_deltas);
    let mut inflate = gix_zlib::Inflate::default();
    let mut delta_bytes = Vec::new();
    let mut stack: Vec<Base> = Vec::new();
    for root in roots {
        if bases.len() == num_ref_deltas {
            break;
        }
        let (entry, data) = decompress(&resolve, pack, &ranges[root], object_hash, alloc_limit_bytes, &mut inflate)?;
        let kind = entry.header.as_kind().expect("a root is never a delta");
        stack.push(Base {
            index: root,
            kind,
            id: hash(object_hash, kind, &data)?,
            data,
        });

        while let Some(base) = stack.pop() {
            if should_interrupt.load(Ordering::Relaxed) {
                return Err(Error::Interrupted);
            }
            let ofs = ofs_children.get(&base.index).map(Vec::as_slice).unwrap_or_default();
            let refs = ref_children.get(&base.id).map(Vec::as_slice).unwrap_or_default();
            for (child, is_ref_delta) in ofs
                .iter()
                .map(|c| (*c, false))
                .chain(refs.iter().map(|c| (*c, true)))
            {
                if is_ref_delta {
                    bases.insert(ranges[child].start, ranges[base.index].start);
                }
                let data = apply_delta(
                    &resolve,
                    pack,
                    &ranges[child],
                    &base.data,
                    object_hash,
                    alloc_limit_bytes,
                    &mut inflate,
                    &mut delta_bytes,
                )?;
                let id = hash(object_hash, base.kind, &data)?;
                // Leaves are dropped right away so only the current path through the delta tree is held in memory,
                // just like `resolve_deltas()` frees a `base_data` that no unresolved delta refers to anymore.
                if ofs_children.contains_key(&child) || ref_children.contains_key(&id) {
                    stack.push(Base {
                        index: child,
                        kind: base.kind,
                        id,
                        data,
                    });
                }
            }
        }
    }
    Ok(bases)
}

fn decompress<F, R>(
    resolve: &F,
    pack: &R,
    range: &EntryRange,
    object_hash: gix_hash::Kind,
    alloc_limit_bytes: Option<usize>,
    inflate: &mut gix_zlib::Inflate,
) -> Result<(data::Entry, Vec<u8>), Error>
where
    F: for<'r> Fn(EntryRange, &'r R) -> Option<&'r [u8]>,
{
    let bytes = resolve(range.clone(), pack).ok_or(Error::ResolveFailed { pack_offset: range.start })?;
    let entry = data::Entry::from_bytes(bytes, range.start, object_hash)?;
    let mut out = Vec::new();
    inflate_into(
        inflate,
        &bytes[entry.header_size()..],
        entry.decompressed_size,
        &mut out,
        alloc_limit_bytes,
    )?;
    Ok((entry, out))
}

#[expect(clippy::too_many_arguments)]
fn apply_delta<F, R>(
    resolve: &F,
    pack: &R,
    range: &EntryRange,
    base_data: &[u8],
    object_hash: gix_hash::Kind,
    alloc_limit_bytes: Option<usize>,
    inflate: &mut gix_zlib::Inflate,
    delta_bytes: &mut Vec<u8>,
) -> Result<Vec<u8>, Error>
where
    F: for<'r> Fn(EntryRange, &'r R) -> Option<&'r [u8]>,
{
    let bytes = resolve(range.clone(), pack).ok_or(Error::ResolveFailed { pack_offset: range.start })?;
    let entry = data::Entry::from_bytes(bytes, range.start, object_hash)?;
    inflate_into(
        inflate,
        &bytes[entry.header_size()..],
        entry.decompressed_size,
        delta_bytes,
        alloc_limit_bytes,
    )?;

    let (base_size, mut header_ofs) = data::delta::decode_header_size(delta_bytes)?;
    if base_data.len() as u64 != base_size {
        return Err(data::delta::apply::Error::Corrupt {
            message: "delta base size does not match base object size",
        }
        .into());
    }
    let (result_size, consumed) = data::delta::decode_header_size(&delta_bytes[header_ofs..])?;
    header_ofs += consumed;

    let mut out = Vec::new();
    resize_with_limit(&mut out, size_limited(result_size, alloc_limit_bytes)?, alloc_limit_bytes)?;
    data::delta::apply(base_data, &mut out, &delta_bytes[header_ofs..])?;
    Ok(out)
}

fn inflate_into(
    inflate: &mut gix_zlib::Inflate,
    compressed: &[u8],
    decompressed_size: u64,
    out: &mut Vec<u8>,
    alloc_limit_bytes: Option<usize>,
) -> Result<(), Error> {
    let len = size_limited(decompressed_size, alloc_limit_bytes)?;
    resize_with_limit(out, len, alloc_limit_bytes)?;
    inflate.reset();
    inflate.once(compressed, out).map_err(|err| Error::ZlibInflate {
        source: err,
        message: "Failed to decompress entry",
    })?;
    Ok(())
}

fn hash(object_hash: gix_hash::Kind, kind: gix_object::Kind, data: &[u8]) -> Result<ObjectId, Error> {
    gix_object::compute_hash(object_hash, kind, data)
        .map_err(|err| Error::Inspect(Box::new(err) as Box<dyn std::error::Error + Send + Sync>))
}

fn size_limited(size: u64, alloc_limit_bytes: Option<usize>) -> Result<usize, Error> {
    let size: usize = size.try_into().map_err(|_| Error::OutOfMemory)?;
    if alloc_limit_bytes.is_some_and(|limit| size > limit) {
        return Err(Error::OutOfMemory);
    }
    Ok(size)
}

fn resize_with_limit(out: &mut Vec<u8>, len: usize, alloc_limit_bytes: Option<usize>) -> Result<(), Error> {
    if alloc_limit_bytes.is_some_and(|limit| len > limit) {
        return Err(Error::OutOfMemory);
    }
    out.try_reserve(len.saturating_sub(out.len()))?;
    out.resize(len, 0);
    Ok(())
}
