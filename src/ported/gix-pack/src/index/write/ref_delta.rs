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
pub(crate) struct Base {
    /// Index into `ranges` of the entry this object was decoded from, or `None` for an object that is not (yet) part
    /// of the pack, like a thin pack's base taken from the object database.
    pub(crate) index: Option<usize>,
    /// The object type, which every delta built on top of this object inherits.
    pub(crate) kind: gix_object::Kind,
    /// The id of the object, which ref-deltas name their base by.
    pub(crate) id: ObjectId,
    /// The fully reconstructed object data a delta is applied to.
    pub(crate) data: Vec<u8>,
}

/// The delta graph of a pack — which entries are undeltified, and which deltas hang off which base — along with what
/// it takes to reconstruct objects by walking it, the way `resolve_deltas()` walks `ofs_deltas` and `ref_deltas` from
/// every base object outwards (`builtin/index-pack.c:1329-1362`, `find_ofs_delta_children()`/`find_ref_delta_children()`
/// at `:713-761`).
pub(crate) struct Walker<'a, F, R> {
    resolve: F,
    pack: &'a R,
    ranges: &'a [EntryRange],
    object_hash: gix_hash::Kind,
    alloc_limit_bytes: Option<usize>,
    should_interrupt: &'a AtomicBool,
    /// The entries that are not deltas, in pack order.
    pub(crate) roots: Vec<usize>,
    ofs_children: HashMap<usize, Vec<usize>>,
    ref_children: HashMap<ObjectId, Vec<usize>>,
    inflate: gix_zlib::Inflate,
    delta_bytes: Vec<u8>,
}

impl<'a, F, R> Walker<'a, F, R>
where
    R: Send + Sync,
    F: for<'r> Fn(EntryRange, &'r R) -> Option<&'r [u8]> + Send + Clone,
{
    /// Read the header of every entry in `ranges`, sorted by pack offset, to learn the pack's delta graph.
    ///
    /// * `resolve` and `pack` provide the raw bytes of an entry, exactly as they do for [`crate::cache::delta::Tree::traverse()`].
    pub(crate) fn new(
        resolve: F,
        pack: &'a R,
        ranges: &'a [EntryRange],
        object_hash: gix_hash::Kind,
        alloc_limit_bytes: Option<usize>,
        should_interrupt: &'a AtomicBool,
    ) -> Result<Self, Error> {
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
        Ok(Walker {
            resolve,
            pack,
            ranges,
            object_hash,
            alloc_limit_bytes,
            should_interrupt,
            roots,
            ofs_children,
            ref_children,
            inflate: gix_zlib::Inflate::default(),
            delta_bytes: Vec::new(),
        })
    }

    /// Decode the undeltified entry at `index` into a base to walk from.
    pub(crate) fn root(&mut self, index: usize) -> Result<Base, Error> {
        let (entry, data) = decompress(
            &self.resolve,
            self.pack,
            &self.ranges[index],
            self.object_hash,
            self.alloc_limit_bytes,
            &mut self.inflate,
        )?;
        let kind = entry.header.as_kind().expect("a root is never a delta");
        Ok(Base {
            index: Some(index),
            kind,
            id: hash(self.object_hash, kind, &data)?,
            data,
        })
    }

    /// Reconstruct every delta that depends on `base`, directly or through other deltas, and call
    /// `on_ref_delta(child, base)` with the `ranges` index of every ref-delta just before it is resolved, along with
    /// the index of the entry its base came from.
    pub(crate) fn walk(
        &mut self,
        base: Base,
        on_ref_delta: &mut dyn FnMut(usize, Option<usize>),
    ) -> Result<(), Error> {
        let mut stack = vec![base];
        while let Some(base) = stack.pop() {
            if self.should_interrupt.load(Ordering::Relaxed) {
                return Err(Error::Interrupted);
            }
            let ofs = base
                .index
                .and_then(|index| self.ofs_children.get(&index))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let refs = self.ref_children.get(&base.id).map(Vec::as_slice).unwrap_or_default();
            for (child, is_ref_delta) in ofs
                .iter()
                .map(|c| (*c, false))
                .chain(refs.iter().map(|c| (*c, true)))
            {
                if is_ref_delta {
                    on_ref_delta(child, base.index);
                }
                let data = apply_delta(
                    &self.resolve,
                    self.pack,
                    &self.ranges[child],
                    &base.data,
                    self.object_hash,
                    self.alloc_limit_bytes,
                    &mut self.inflate,
                    &mut self.delta_bytes,
                )?;
                let id = hash(self.object_hash, base.kind, &data)?;
                // Leaves are dropped right away so only the current path through the delta tree is held in memory,
                // just like `resolve_deltas()` frees a `base_data` that no unresolved delta refers to anymore.
                if self.ofs_children.contains_key(&child) || self.ref_children.contains_key(&id) {
                    stack.push(Base {
                        index: Some(child),
                        kind: base.kind,
                        id,
                        data,
                    });
                }
            }
        }
        Ok(())
    }
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
    let mut walker = Walker::new(resolve, pack, ranges, object_hash, alloc_limit_bytes, should_interrupt)?;
    let mut bases = BasesByChildOffset::with_capacity(num_ref_deltas);
    for root in walker.roots.clone() {
        if bases.len() == num_ref_deltas {
            break;
        }
        let base = walker.root(root)?;
        walker.walk(base, &mut |child, base| {
            bases.insert(
                ranges[child].start,
                ranges[base.expect("every base in the pack has an index")].start,
            );
        })?;
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
