//! The sliding-window delta search, ported from git 2.55.0
//! `builtin/pack-objects.c` (`type_size_sort`, `try_delta`, `find_deltas`,
//! `ll_find_deltas`, `delta_cacheable`).
//!
//! git sorts the objects it is about to pack by type, then by a hash of the
//! path they were last seen at, then by descending size, so that objects likely
//! to resemble each other end up adjacent. It then slides a window of `window`
//! slots over that order and, for each object, tries to express it as a delta
//! against every other object still in the window, keeping the smallest result
//! that does not exceed the configured chain `depth`.
//!
//! # Deliberate departures from git
//!
//! Each is a place where git has substrate this crate does not, rather than a
//! simplification of the algorithm itself:
//!
//! * **No delta reuse from existing packs.** git's `try_delta()` opens with a
//!   `reuse_delta && IN_PACK(trg) == IN_PACK(src)` shortcut that declines to
//!   re-derive a delta it could copy verbatim. Nothing here copies pack entries,
//!   so every pair is searched afresh. The output is a valid pack either way;
//!   the cost is time, not correctness.
//! * **No preferred bases.** They exist to steer the search using knowledge
//!   from outside the object set — what a fetch peer already has — which nothing
//!   here models, so no entry is a preferred base. Delta *islands* are modelled;
//!   see [`Islands`].
//! * **`max_depth` is always `depth`.** git lowers it via `check_delta_limit()`
//!   when the object already has delta *children* from a reused pack delta. With
//!   no reuse, the child network is empty during the search, so the lowering
//!   never fires.
//! * **Static work partitioning.** git's `ll_find_deltas()` partitions the list
//!   across threads exactly as this does, then keeps rebalancing: an idle thread
//!   steals half the backlog of the busiest one. The stealing is a throughput
//!   optimisation over the same search; skipping it changes how long the search
//!   takes, not which deltas it finds within a segment.
//! * **Cached deltas are kept uncompressed.** git deflates a delta as soon as it
//!   decides to cache it, so that its cache budget holds more of them. Here the
//!   budget accounts for raw delta bytes and the deflate happens once, at write
//!   time. The cache admission rule itself is git's, unchanged.
//! * **`max_size` underflow is saturated.** For an object of 50..2*rawsz bytes
//!   git computes `trg_size/2 - rawsz` in unsigned arithmetic and wraps to a
//!   near-infinite budget, which lets it emit a delta larger than the object it
//!   replaces. That is reachable only with SHA-256. Saturating to zero declines
//!   the delta instead, which is what the surrounding heuristics intend.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gix_hash::ObjectId;

use super::encode::Index;

/// git's own default for `pack.deltaCacheSize` (`DEFAULT_DELTA_CACHE_SIZE`,
/// `pack-objects.h:11`).
pub const DEFAULT_DELTA_CACHE_SIZE: u64 = 256 * 1024 * 1024;

/// The smallest object git will try to deltify, from `should_attempt_deltas()`.
const MIN_SIZE_FOR_DELTA: u64 = 50;

/// How the search is steered. Every field is one of git's `pack.*` knobs, with
/// git's default as the `Default` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// `pack.window` / `--window`: how many other objects each object is tried
    /// against. Zero disables the search entirely.
    pub window: usize,
    /// `pack.depth` / `--depth`: the longest delta chain that may be produced.
    /// Zero disables the search entirely.
    pub depth: usize,
    /// `pack.windowMemory` / `--window-memory`: an upper bound in bytes on the
    /// object data and delta indices held by one window. Zero means unlimited.
    /// Reaching it evicts the oldest window slots, which shrinks the effective
    /// window and so weakens compression in exchange for a memory ceiling.
    pub window_memory_limit: u64,
    /// `pack.deltaCacheSize`: how many bytes of already-computed deltas may be
    /// held in memory. Zero means unlimited. A delta that is not cached is
    /// recomputed by the caller when the pack is written.
    pub max_delta_cache_size: u64,
    /// `pack.deltaCacheLimit`: deltas below this size are always cached.
    pub cache_max_small_delta_size: u64,
    /// `pack.threads` / `--threads`: how many threads search in parallel. Zero
    /// means one per logical core, which is git's `online_cpus()`.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            window: 10,
            depth: 50,
            window_memory_limit: 0,
            max_delta_cache_size: DEFAULT_DELTA_CACHE_SIZE,
            cache_max_small_delta_size: 1000,
            threads: 0,
        }
    }
}

/// One candidate for the search, as much of git's `struct object_entry` as the
/// search actually reads.
#[derive(Debug, Clone)]
pub struct Object {
    /// The object's id, used to fetch its bytes.
    pub id: ObjectId,
    /// Its type. Only objects of the same type are ever deltified against each
    /// other.
    pub kind: gix_object::Kind,
    /// Its uncompressed size in bytes.
    pub size: u64,
    /// git's `pack_name_hash()` of the path the object was last seen at, which
    /// is what makes same-named files across revisions adjacent in the sort.
    /// Zero when no path is known, which degrades the sort to type-and-size.
    pub name_hash: u32,
}

/// The delta the search settled on for one object.
#[derive(Debug, Clone)]
pub struct Delta {
    /// Index into the `objects` slice given to [`find_deltas()`] of the object
    /// this one is expressed against.
    pub base: usize,
    /// The length of the delta stream in bytes.
    pub size: u64,
    /// How many deltas have to be applied to reach a base object, counting this
    /// one. Never exceeds [`Options::depth`].
    pub depth: u32,
    /// The delta bytes, if the cache had room for them. `None` means the caller
    /// must recompute the delta from `base`, which is git's `get_delta()` path.
    pub data: Option<Vec<u8>>,
}

/// Which islands each object belongs to; git's `island_marks` in
/// `delta-islands.c`.
///
/// An island is a set of refs — one fork, one namespace, one mirror — that a
/// pack should be servable from without dragging in objects only reachable from
/// somewhere else. An object's mark is the set of islands that reach it, and a
/// delta may only be built against a base at least as widely reachable as its
/// target; otherwise serving the target would require sending a base the peer
/// has no reason to have.
///
/// An empty table means islands are switched off, which is git's `!island_marks`
/// and makes every pair eligible again.
#[derive(Debug, Clone, Default)]
pub struct Islands {
    /// Indexed like the `objects` slice given to [`find_deltas()`]. `None` is
    /// git's "no entry in the hash": an object no island reaches.
    pub marks: Vec<Option<std::sync::Arc<[u32]>>>,
}

impl Islands {
    /// Whether `self` is a subset of `super_`, git's `island_bitmap_is_subset()`.
    fn is_subset(one: &[u32], other: &[u32]) -> bool {
        one.iter()
            .zip(other)
            .all(|(one, other)| one & other == *one)
    }

    fn marks_of(&self, at: usize) -> Option<&std::sync::Arc<[u32]>> {
        self.marks.get(at).and_then(Option::as_ref)
    }

    /// git's `in_same_island()`: may `trg` be deltified against `src`?
    ///
    /// An unmarked target may delta against anything, since nothing serves it
    /// on its own. An unmarked *base* is refused outright, since a marked
    /// target would then depend on an object no island guarantees is present.
    ///
    /// Public because git asks it from two places: the window scan in
    /// `try_delta()`, and `can_reuse_delta()` before it keeps a delta an
    /// existing pack already holds.
    pub fn in_same_island(&self, trg: usize, src: usize) -> bool {
        if self.marks.is_empty() {
            return true;
        }
        let Some(trg) = self.marks_of(trg) else { return true };
        let Some(src) = self.marks_of(src) else { return false };
        Islands::is_subset(trg, src)
    }

    /// git's `island_delta_cmp()`: order the more widely reachable object first,
    /// so that it is already in the window when the narrower one arrives and can
    /// serve as its base.
    fn delta_cmp(&self, a: usize, b: usize) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if self.marks.is_empty() {
            return Ordering::Equal;
        }
        let (a, b) = (self.marks_of(a), self.marks_of(b));
        if let Some(a_marks) = a {
            if !b.is_some_and(|b| Islands::is_subset(a_marks, b)) {
                return Ordering::Less;
            }
        }
        if let Some(b_marks) = b {
            if !a.is_some_and(|a| Islands::is_subset(b_marks, a)) {
                return Ordering::Greater;
            }
        }
        Ordering::Equal
    }
}

/// git's `pack_name_hash()`: a sortable number built from the last sixteen
/// non-whitespace characters of a path, so that files with the same extension
/// and name sort together regardless of the directory they live in.
pub fn name_hash(path: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &c in path {
        if c.is_ascii_whitespace() {
            continue;
        }
        hash = (hash >> 2).wrapping_add(u32::from(c) << 24);
    }
    hash
}

/// git's numbering of object types, which `type_size_sort` compares directly.
fn type_rank(kind: gix_object::Kind) -> u8 {
    use gix_object::Kind::*;
    match kind {
        Commit => 1,
        Tree => 2,
        Blob => 3,
        Tag => 4,
    }
}

/// Search `objects` for deltas and return, for each of them, the delta chosen
/// or `None` if it stays a base object.
///
/// `already_deltified` is git's first test in `should_attempt_deltas()`:
/// `if (DELTA(entry)) return 0`, which holds for an object whose delta was
/// reused from an existing pack by `check_object()`. Such an object is left out
/// of the sorted list entirely, so it neither gets searched nor ever occupies a
/// window slot — the latter is what keeps a searched delta from choosing a
/// reused delta as its base and closing a cycle. An empty slice means nothing
/// was reused, which is the `--no-reuse-delta` case and the shape every caller
/// with no pack to reuse from passes.
///
/// `new_state` is called once per thread to build whatever an object read needs
/// (an object database handle, typically); `read` then fetches one object's
/// bytes, returning `None` for an object that cannot be read, which is skipped
/// rather than fatal.
pub fn find_deltas<S, New, Read>(
    objects: &[Object],
    islands: &Islands,
    options: &Options,
    already_deltified: &[bool],
    mut new_state: New,
    read: Read,
) -> Vec<Option<Delta>>
where
    S: Send,
    New: FnMut() -> S,
    Read: Fn(&S, &gix_hash::oid) -> Option<Vec<u8>> + Sync,
{
    let mut out: Vec<Option<Delta>> = std::iter::repeat_with(|| None).take(objects.len()).collect();
    if objects.len() < 2 || options.window == 0 || options.depth == 0 {
        return out;
    }

    // git's `prepare_pack()`: collect the objects worth trying, then sort them
    // into the order the window slides over.
    let mut list: Vec<usize> = (0..objects.len())
        .filter(|&i| objects[i].size >= MIN_SIZE_FOR_DELTA)
        .filter(|&i| !already_deltified.get(i).copied().unwrap_or(false))
        .collect();
    if list.len() < 2 {
        return out;
    }
    list.sort_by(|&a, &b| type_size_sort(&objects[a], a, &objects[b], b, islands));

    // `prepare_pack()` passes `window + 1`, so a window of N really means N
    // candidate bases plus the slot holding the object being deltified.
    let window = options.window + 1;
    let threads = match options.threads {
        0 => std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        n => n,
    };

    let cache_size = AtomicU64::new(0);
    let segments = partition(objects, &list, threads, window);
    // Each thread gets its own read state, built here because the thing that
    // builds it (an object database handle) is typically shareable across
    // threads only by being moved, not by being borrowed.
    let mut states: Vec<S> = segments.iter().map(|_| new_state()).collect();

    let results: Vec<HashMap<usize, Delta>> = if segments.len() <= 1 {
        match (segments.first(), states.pop()) {
            (Some(segment), Some(state)) => vec![find_deltas_in_segment(
                objects,
                segment,
                islands,
                options,
                window,
                &state,
                &read,
                &cache_size,
            )],
            _ => Vec::new(),
        }
    } else {
        std::thread::scope(|scope| {
            let handles: Vec<_> = segments
                .iter()
                .zip(states)
                .map(|(segment, state)| {
                    let read = &read;
                    let cache_size = &cache_size;
                    scope.spawn(move || {
                        find_deltas_in_segment(objects, segment, islands, options, window, &state, read, cache_size)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("the delta search itself never panics"))
                .collect()
        })
    };

    for found in results {
        for (at, delta) in found {
            out[at] = Some(delta);
        }
    }
    out
}

/// git's `type_size_sort()`: descending type, descending name hash, descending
/// size, and finally ascending original position so that the newest object of a
/// tie is tried first as a base.
fn type_size_sort(a: &Object, a_at: usize, b: &Object, b_at: usize, islands: &Islands) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    type_rank(b.kind)
        .cmp(&type_rank(a.kind))
        .then_with(|| b.name_hash.cmp(&a.name_hash))
        .then_with(|| islands.delta_cmp(a_at, b_at))
        .then_with(|| b.size.cmp(&a.size))
        .then_with(|| match a_at.cmp(&b_at) {
            Ordering::Equal => Ordering::Equal,
            other => other,
        })
}

/// git's work split in `ll_find_deltas()`: give each thread an equal share, but
/// never a share too small to find anything, and prefer to cut where the name
/// hash changes so that a run of same-named objects stays in one segment.
fn partition<'a>(objects: &[Object], list: &'a [usize], threads: usize, window: usize) -> Vec<&'a [usize]> {
    if threads <= 1 {
        return vec![list];
    }
    let mut out = Vec::with_capacity(threads);
    let mut rest = list;
    for i in 0..threads {
        let mut sub = rest.len() / (threads - i);
        if sub < 2 * window && i + 1 < threads {
            sub = 0;
        }
        while sub > 0
            && sub < rest.len()
            && objects[rest[sub]].name_hash != 0
            && objects[rest[sub]].name_hash == objects[rest[sub - 1]].name_hash
        {
            sub += 1;
        }
        out.push(&rest[..sub]);
        rest = &rest[sub..];
    }
    out.retain(|segment| !segment.is_empty());
    out
}

/// An object's bytes, shared between the window slot that reads them as a delta
/// target and the delta index built over them when the same slot later serves
/// as a base, so they are read and held once.
#[derive(Clone)]
struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// One window slot; git's `struct unpacked`.
#[derive(Default)]
struct Unpacked {
    /// Index into the caller's `objects`, or `None` for an empty slot.
    at: Option<usize>,
    /// The object's bytes, shared with `index` so they are read and indexed once.
    data: Option<Shared>,
    index: Option<Index<Shared>>,
    /// How deep this object's own delta chain is, as decided so far in this run.
    depth: u32,
}

impl Unpacked {
    /// git's `free_unpacked()`: release the slot and report how much of the
    /// window's memory budget that returns.
    fn release(&mut self, objects: &[Object]) -> u64 {
        let mut freed = self.index.as_ref().map_or(0, |index| index.memory_usage());
        self.index = None;
        if self.data.take().is_some() {
            freed += self.at.map_or(0, |at| objects[at].size);
        }
        self.at = None;
        self.depth = 0;
        freed
    }
}

/// Two distinct slots of the window, borrowed at once.
fn two_mut<T>(slice: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    debug_assert_ne!(a, b, "a slot is never compared against itself");
    if a < b {
        let (left, right) = slice.split_at_mut(b);
        (&mut left[a], &mut right[0])
    } else {
        let (left, right) = slice.split_at_mut(a);
        (&mut right[0], &mut left[b])
    }
}

/// git's `find_deltas()` over one contiguous slice of the sorted list.
fn find_deltas_in_segment<S, Read>(
    objects: &[Object],
    list: &[usize],
    islands: &Islands,
    options: &Options,
    window: usize,
    state: &S,
    read: &Read,
    cache_size: &AtomicU64,
) -> HashMap<usize, Delta>
where
    Read: Fn(&S, &gix_hash::oid) -> Option<Vec<u8>>,
{
    let mut found: HashMap<usize, Delta> = HashMap::new();
    let mut array: Vec<Unpacked> = std::iter::repeat_with(Unpacked::default).take(window).collect();
    let mut idx = 0usize;
    let mut count = 0usize;
    let mut mem_usage: u64 = 0;
    let mut cursor = 0usize;

    while cursor < list.len() {
        let entry = list[cursor];
        cursor += 1;

        mem_usage = mem_usage.saturating_sub(array[idx].release(objects));
        array[idx].at = Some(entry);

        // Evict from the tail until the window fits `pack.windowMemory` again.
        while options.window_memory_limit > 0 && mem_usage > options.window_memory_limit && count > 1 {
            let tail = (idx + window - count) % window;
            mem_usage = mem_usage.saturating_sub(array[tail].release(objects));
            count -= 1;
        }

        let max_depth = options.depth;
        let mut best_base: Option<usize> = None;
        let mut j = window;
        while j > 1 {
            j -= 1;
            let mut other = idx + j;
            if other >= window {
                other -= window;
            }
            if array[other].at.is_none() {
                break;
            }
            let outcome = try_delta(
                objects,
                islands,
                &mut array,
                idx,
                other,
                max_depth,
                &mut mem_usage,
                &mut found,
                options,
                state,
                read,
                cache_size,
            );
            if outcome < 0 {
                break;
            } else if outcome > 0 {
                best_base = Some(other);
            }
        }

        // An object that is both a delta and already as deep as the chain may
        // go can never serve as a base, so its slot is reused straight away.
        if found.contains_key(&entry) && max_depth as u32 <= array[idx].depth {
            continue;
        }

        // Keep the winning base around longer by moving it just past the object
        // it was the base for; it is the first candidate tried next time.
        if let Some(best) = best_base.filter(|_| found.contains_key(&entry)) {
            let mut dist = (window + idx - best) % window;
            let swap = std::mem::take(&mut array[best]);
            let mut dst = best;
            while dist > 0 {
                dist -= 1;
                let src = (dst + 1) % window;
                array[dst] = std::mem::take(&mut array[src]);
                dst = src;
            }
            array[dst] = swap;
        }

        idx += 1;
        if count + 1 < window {
            count += 1;
        }
        if idx >= window {
            idx = 0;
        }
    }

    found
}

/// git's `delta_cacheable()`: whether a delta earns a place in the in-memory
/// cache, given how much of the budget is already spent.
fn delta_cacheable(options: &Options, spent: u64, src_size: u64, trg_size: u64, delta_size: u64) -> bool {
    if options.max_delta_cache_size > 0 && spent + delta_size > options.max_delta_cache_size {
        return false;
    }
    if delta_size < options.cache_max_small_delta_size {
        return true;
    }
    // Cache it anyway if the objects are large compared to the delta.
    (src_size >> 20) + (trg_size >> 21) > (delta_size >> 10)
}

/// git's `try_delta()`. Returns `1` if `trg` now deltifies against `src`, `0` if
/// it does not, and `-1` when the pair is so mismatched that the rest of the
/// window is not worth trying either.
#[expect(clippy::too_many_arguments)]
fn try_delta<S, Read>(
    objects: &[Object],
    islands: &Islands,
    array: &mut [Unpacked],
    trg_slot: usize,
    src_slot: usize,
    max_depth: usize,
    mem_usage: &mut u64,
    found: &mut HashMap<usize, Delta>,
    options: &Options,
    state: &S,
    read: &Read,
    cache_size: &AtomicU64,
) -> i32
where
    Read: Fn(&S, &gix_hash::oid) -> Option<Vec<u8>>,
{
    let (trg, src) = two_mut(array, trg_slot, src_slot);
    let (trg_at, src_at) = match (trg.at, src.at) {
        (Some(t), Some(s)) => (t, s),
        _ => return 0,
    };
    let (trg_obj, src_obj) = (&objects[trg_at], &objects[src_at]);

    // Never diff between different types.
    if trg_obj.kind != src_obj.kind {
        return -1;
    }
    // Do not bust the allowed depth.
    if src.depth as usize >= max_depth {
        return 0;
    }

    // Size filtering, before anything is read from the object database.
    let trg_size = trg_obj.size;
    let existing = found.get(&trg_at);
    let (base_budget, ref_depth) = match existing {
        None => (
            (trg_size / 2).saturating_sub(trg_obj.id.kind().len_in_bytes() as u64),
            1u32,
        ),
        Some(delta) => (delta.size, trg.depth),
    };
    let max_size = base_budget * (max_depth as u64 - u64::from(src.depth))
        / (max_depth as u64 - u64::from(ref_depth) + 1);
    if max_size == 0 {
        return 0;
    }
    let src_size = src_obj.size;
    let sizediff = trg_size.saturating_sub(src_size);
    if sizediff >= max_size {
        return 0;
    }
    if trg_size < src_size / 32 {
        return 0;
    }

    // A base outside the target's islands would make serving the target depend
    // on an object the islands do not promise is there.
    if !islands.in_same_island(trg_at, src_at) {
        return 0;
    }

    // Load object data on first use, and index the source on first use as a
    // base; both stay in the slot so the window pays for them only once.
    if trg.data.is_none() {
        let Some(data) = read(state, &trg_obj.id) else {
            return -1;
        };
        *mem_usage += data.len() as u64;
        trg.data = Some(Shared(Arc::new(data)));
    }
    if src.data.is_none() {
        let Some(data) = read(state, &src_obj.id) else {
            return 0;
        };
        *mem_usage += data.len() as u64;
        src.data = Some(Shared(Arc::new(data)));
    }
    if src.index.is_none() {
        let source = src.data.clone().expect("just loaded");
        let Some(index) = Index::new(source) else {
            return 0;
        };
        *mem_usage += index.memory_usage();
        src.index = Some(index);
    }

    let target = trg.data.as_ref().expect("just loaded");
    let index = src.index.as_ref().expect("just built");
    let Some(delta) = index.create(target.as_ref(), max_size) else {
        return 0;
    };
    let delta_size = delta.len() as u64;

    if let Some(existing) = found.get(&trg_at) {
        // Prefer only shallower same-sized deltas.
        if delta_size == existing.size && src.depth + 1 >= trg.depth {
            return 0;
        }
    }

    if let Some(previous) = found.remove(&trg_at) {
        if previous.data.is_some() {
            cache_size.fetch_sub(previous.size, Ordering::Relaxed);
        }
    }
    let spent = cache_size.load(Ordering::Relaxed);
    let data = if delta_cacheable(options, spent, src_size, trg_size, delta_size) {
        cache_size.fetch_add(delta_size, Ordering::Relaxed);
        Some(delta)
    } else {
        None
    };

    trg.depth = src.depth + 1;
    found.insert(
        trg_at,
        Delta {
            base: src_at,
            size: delta_size,
            depth: trg.depth,
            data,
        },
    );
    1
}

#[cfg(test)]
mod tests {
    use super::{Object, Options, find_deltas, name_hash};
    use gix_hash::ObjectId;

    /// Build an object id that is unique per index but otherwise meaningless;
    /// the search never interprets it beyond using it as a read key.
    fn id(n: u32) -> ObjectId {
        let mut bytes = [0u8; 20];
        bytes[..4].copy_from_slice(&n.to_be_bytes());
        ObjectId::from_bytes_or_panic(&bytes)
    }

    /// Twenty revisions of one file, each a small edit of the last, plus one
    /// unrelated blob. Exactly the shape a delta search is supposed to collapse.
    fn corpus() -> (Vec<Object>, Vec<Vec<u8>>) {
        let mut bodies = Vec::new();
        let base: Vec<u8> = (0..2_000u32).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
        for revision in 0..20u32 {
            let mut body = base.clone();
            body.extend_from_slice(format!("revision {revision}\n").as_bytes());
            bodies.push(body);
        }
        bodies.push(vec![b'z'; 5_000]);
        let objects = (0..bodies.len())
            .map(|n| Object {
                id: id(n as u32),
                kind: gix_object::Kind::Blob,
                size: bodies[n].len() as u64,
                name_hash: name_hash(b"src/file.c"),
            })
            .collect();
        (objects, bodies)
    }

    fn search(objects: &[Object], bodies: &[Vec<u8>], options: &Options) -> Vec<Option<super::Delta>> {
        find_deltas(
            objects,
            &super::Islands::default(),
            options,
            &[],
            || (),
            |(), oid| {
                let n = u32::from_be_bytes(oid.as_bytes()[..4].try_into().expect("4 bytes")) as usize;
                bodies.get(n).cloned()
            },
        )
    }

    #[test]
    fn finds_deltas_and_respects_the_depth_limit() {
        let (objects, bodies) = corpus();
        for depth in [1usize, 2, 5, 50] {
            let options = Options {
                window: 10,
                depth,
                ..Options::default()
            };
            let deltas = search(&objects, &bodies, &options);
            let count = deltas.iter().filter(|d| d.is_some()).count();
            assert!(count > 0, "depth {depth} should still find deltas");
            for delta in deltas.iter().flatten() {
                assert!(
                    delta.depth as usize <= depth,
                    "chain of {} exceeds the configured depth of {depth}",
                    delta.depth
                );
            }
        }
    }

    #[test]
    fn a_wider_window_finds_at_least_as_many_deltas() {
        let (objects, bodies) = corpus();
        let narrow = search(
            &objects,
            &bodies,
            &Options {
                window: 1,
                threads: 1,
                ..Options::default()
            },
        );
        let wide = search(
            &objects,
            &bodies,
            &Options {
                window: 20,
                threads: 1,
                ..Options::default()
            },
        );
        let narrow_bytes: u64 = narrow.iter().flatten().map(|d| d.size).sum();
        let wide_bytes: u64 = wide.iter().flatten().map(|d| d.size).sum();
        assert!(
            wide.iter().filter(|d| d.is_some()).count() >= narrow.iter().filter(|d| d.is_some()).count(),
            "a wider window cannot find fewer deltas"
        );
        assert!(narrow_bytes > 0 && wide_bytes > 0, "both windows find something");
    }

    #[test]
    fn a_window_of_zero_disables_the_search() {
        let (objects, bodies) = corpus();
        let deltas = search(
            &objects,
            &bodies,
            &Options {
                window: 0,
                ..Options::default()
            },
        );
        assert!(deltas.iter().all(Option::is_none), "no delta is attempted");
    }

    #[test]
    fn the_delta_cache_limit_decides_what_is_kept_in_memory() {
        let (objects, bodies) = corpus();
        let cached = search(
            &objects,
            &bodies,
            &Options {
                threads: 1,
                ..Options::default()
            },
        );
        let uncached = search(
            &objects,
            &bodies,
            &Options {
                threads: 1,
                max_delta_cache_size: 1,
                cache_max_small_delta_size: 0,
                ..Options::default()
            },
        );
        assert!(
            cached.iter().flatten().any(|d| d.data.is_some()),
            "the default budget keeps deltas in memory"
        );
        assert!(
            uncached.iter().flatten().all(|d| d.data.is_none()),
            "a one-byte budget keeps none of them"
        );
        let with: Vec<_> = cached.iter().map(|d| d.as_ref().map(|d| (d.base, d.size))).collect();
        let without: Vec<_> = uncached.iter().map(|d| d.as_ref().map(|d| (d.base, d.size))).collect();
        assert_eq!(with, without, "caching changes memory use, never the chosen deltas");
    }

    #[test]
    fn islands_keep_a_delta_from_naming_a_narrower_base() {
        use std::sync::Arc;
        let (objects, bodies) = corpus();
        let options = Options {
            threads: 1,
            ..Options::default()
        };

        // Two islands. The even-numbered objects are reachable from both, the
        // odd ones only from the first, so an even object may never delta
        // against an odd one — the odd one is not guaranteed to be present
        // wherever the even one is served from.
        let both: Arc<[u32]> = Arc::from(vec![0b11u32]);
        let one: Arc<[u32]> = Arc::from(vec![0b01u32]);
        let islands = super::Islands {
            marks: (0..objects.len())
                .map(|at| Some(if at % 2 == 0 { both.clone() } else { one.clone() }))
                .collect(),
        };

        let unrestricted = search(&objects, &bodies, &options);
        let restricted = find_deltas(&objects, &islands, &options, &[], || (), |(), oid| {
            let n = u32::from_be_bytes(oid.as_bytes()[..4].try_into().expect("4 bytes")) as usize;
            bodies.get(n).cloned()
        });

        for (at, delta) in restricted.iter().enumerate() {
            let Some(delta) = delta else { continue };
            assert!(
                at % 2 == 1 || delta.base % 2 == 0,
                "object {at} is in both islands but was deltified against {}, which is in one",
                delta.base
            );
        }
        assert!(
            unrestricted.iter().filter(|d| d.is_some()).count() > 0
                && restricted.iter().filter(|d| d.is_some()).count() > 0,
            "both runs still find deltas; islands narrow the choice, they do not forbid it"
        );
    }

    #[test]
    fn an_unmarked_object_is_never_a_base_but_can_be_a_target() {
        use std::sync::Arc;
        let (objects, bodies) = corpus();
        let marked: Arc<[u32]> = Arc::from(vec![0b1u32]);
        // Only the first half is on an island at all.
        let islands = super::Islands {
            marks: (0..objects.len())
                .map(|at| (at < objects.len() / 2).then(|| marked.clone()))
                .collect(),
        };
        let deltas = find_deltas(
            &objects,
            &islands,
            &Options {
                threads: 1,
                ..Options::default()
            },
            &[],
            || (),
            |(), oid| {
                let n = u32::from_be_bytes(oid.as_bytes()[..4].try_into().expect("4 bytes")) as usize;
                bodies.get(n).cloned()
            },
        );
        for (at, delta) in deltas.iter().enumerate() {
            let Some(delta) = delta else { continue };
            if at < objects.len() / 2 {
                assert!(
                    delta.base < objects.len() / 2,
                    "a marked object at {at} may not rest on the unmarked object {}",
                    delta.base
                );
            }
        }
    }

    #[test]
    fn no_object_is_its_own_ancestor() {
        let (objects, bodies) = corpus();
        let deltas = search(&objects, &bodies, &Options::default());
        for start in 0..objects.len() {
            let mut at = start;
            for _ in 0..=objects.len() {
                match deltas[at].as_ref() {
                    Some(delta) => at = delta.base,
                    None => break,
                }
                assert_ne!(at, start, "delta chains must be acyclic");
            }
        }
    }
}
