//! Delta *creation*, ported from git 2.55.0 `diff-delta.c`.
//!
//! The counterpart of [`crate::data::delta::apply`]: given a source ("base")
//! buffer and a target buffer, produce the git delta byte stream that rebuilds
//! the target from the base. The algorithm is Nicolas Pitre's Rabin-fingerprint
//! index, kept byte-for-byte compatible with git's so that the deltas written
//! here are the same shape git writes and are accepted by every git reader.
//!
//! The port is line-faithful; the only deliberate departures are the ones Rust
//! forces:
//!
//! * git indexes with raw pointers into the source buffer. Here every position
//!   is a `usize` offset into the same slice, so the walk that C writes as
//!   `data -= RABIN_WINDOW` down to `data >= buffer` is a descending index loop.
//! * git's hash buckets are singly-linked lists in a `malloc`'d arena, then
//!   flattened into a packed array. The arena is a `Vec<Node>` with `next` as an
//!   index, and the flattening is the same two-pass walk, so bucket *order* —
//!   which decides which of several equally good matches wins — is preserved.
//! * `malloc`/`realloc` of the output become `Vec::resize`, since git writes
//!   into the buffer at positions it has already passed (`out[outpos - inscnt - 1]`)
//!   and therefore needs random access rather than an append-only writer.

/// Maximum number of index entries git keeps per hash bucket, so that a
/// pathological input cannot turn the match search into O(m*n).
const HASH_LIMIT: usize = 64;

const RABIN_SHIFT: u32 = 23;
const RABIN_WINDOW: usize = 16;

/// The maximum size of any opcode sequence: the initial header, the Rabin
/// window, and the biggest copy instruction.
const MAX_OP_SIZE: usize = 5 + 5 + 1 + RABIN_WINDOW + 7;

#[rustfmt::skip]
static T: [u32; 256] = [
    0x00000000, 0xab59b4d1, 0x56b369a2, 0xfdeadd73, 0x063f6795, 0xad66d344,
    0x508c0e37, 0xfbd5bae6, 0x0c7ecf2a, 0xa7277bfb, 0x5acda688, 0xf1941259,
    0x0a41a8bf, 0xa1181c6e, 0x5cf2c11d, 0xf7ab75cc, 0x18fd9e54, 0xb3a42a85,
    0x4e4ef7f6, 0xe5174327, 0x1ec2f9c1, 0xb59b4d10, 0x48719063, 0xe32824b2,
    0x1483517e, 0xbfdae5af, 0x423038dc, 0xe9698c0d, 0x12bc36eb, 0xb9e5823a,
    0x440f5f49, 0xef56eb98, 0x31fb3ca8, 0x9aa28879, 0x6748550a, 0xcc11e1db,
    0x37c45b3d, 0x9c9defec, 0x6177329f, 0xca2e864e, 0x3d85f382, 0x96dc4753,
    0x6b369a20, 0xc06f2ef1, 0x3bba9417, 0x90e320c6, 0x6d09fdb5, 0xc6504964,
    0x2906a2fc, 0x825f162d, 0x7fb5cb5e, 0xd4ec7f8f, 0x2f39c569, 0x846071b8,
    0x798aaccb, 0xd2d3181a, 0x25786dd6, 0x8e21d907, 0x73cb0474, 0xd892b0a5,
    0x23470a43, 0x881ebe92, 0x75f463e1, 0xdeadd730, 0x63f67950, 0xc8afcd81,
    0x354510f2, 0x9e1ca423, 0x65c91ec5, 0xce90aa14, 0x337a7767, 0x9823c3b6,
    0x6f88b67a, 0xc4d102ab, 0x393bdfd8, 0x92626b09, 0x69b7d1ef, 0xc2ee653e,
    0x3f04b84d, 0x945d0c9c, 0x7b0be704, 0xd05253d5, 0x2db88ea6, 0x86e13a77,
    0x7d348091, 0xd66d3440, 0x2b87e933, 0x80de5de2, 0x7775282e, 0xdc2c9cff,
    0x21c6418c, 0x8a9ff55d, 0x714a4fbb, 0xda13fb6a, 0x27f92619, 0x8ca092c8,
    0x520d45f8, 0xf954f129, 0x04be2c5a, 0xafe7988b, 0x5432226d, 0xff6b96bc,
    0x02814bcf, 0xa9d8ff1e, 0x5e738ad2, 0xf52a3e03, 0x08c0e370, 0xa39957a1,
    0x584ced47, 0xf3155996, 0x0eff84e5, 0xa5a63034, 0x4af0dbac, 0xe1a96f7d,
    0x1c43b20e, 0xb71a06df, 0x4ccfbc39, 0xe79608e8, 0x1a7cd59b, 0xb125614a,
    0x468e1486, 0xedd7a057, 0x103d7d24, 0xbb64c9f5, 0x40b17313, 0xebe8c7c2,
    0x16021ab1, 0xbd5bae60, 0x6cb54671, 0xc7ecf2a0, 0x3a062fd3, 0x915f9b02,
    0x6a8a21e4, 0xc1d39535, 0x3c394846, 0x9760fc97, 0x60cb895b, 0xcb923d8a,
    0x3678e0f9, 0x9d215428, 0x66f4eece, 0xcdad5a1f, 0x3047876c, 0x9b1e33bd,
    0x7448d825, 0xdf116cf4, 0x22fbb187, 0x89a20556, 0x7277bfb0, 0xd92e0b61,
    0x24c4d612, 0x8f9d62c3, 0x7836170f, 0xd36fa3de, 0x2e857ead, 0x85dcca7c,
    0x7e09709a, 0xd550c44b, 0x28ba1938, 0x83e3ade9, 0x5d4e7ad9, 0xf617ce08,
    0x0bfd137b, 0xa0a4a7aa, 0x5b711d4c, 0xf028a99d, 0x0dc274ee, 0xa69bc03f,
    0x5130b5f3, 0xfa690122, 0x0783dc51, 0xacda6880, 0x570fd266, 0xfc5666b7,
    0x01bcbbc4, 0xaae50f15, 0x45b3e48d, 0xeeea505c, 0x13008d2f, 0xb85939fe,
    0x438c8318, 0xe8d537c9, 0x153feaba, 0xbe665e6b, 0x49cd2ba7, 0xe2949f76,
    0x1f7e4205, 0xb427f6d4, 0x4ff24c32, 0xe4abf8e3, 0x19412590, 0xb2189141,
    0x0f433f21, 0xa41a8bf0, 0x59f05683, 0xf2a9e252, 0x097c58b4, 0xa225ec65,
    0x5fcf3116, 0xf49685c7, 0x033df00b, 0xa86444da, 0x558e99a9, 0xfed72d78,
    0x0502979e, 0xae5b234f, 0x53b1fe3c, 0xf8e84aed, 0x17bea175, 0xbce715a4,
    0x410dc8d7, 0xea547c06, 0x1181c6e0, 0xbad87231, 0x4732af42, 0xec6b1b93,
    0x1bc06e5f, 0xb099da8e, 0x4d7307fd, 0xe62ab32c, 0x1dff09ca, 0xb6a6bd1b,
    0x4b4c6068, 0xe015d4b9, 0x3eb80389, 0x95e1b758, 0x680b6a2b, 0xc352defa,
    0x3887641c, 0x93ded0cd, 0x6e340dbe, 0xc56db96f, 0x32c6cca3, 0x999f7872,
    0x6475a501, 0xcf2c11d0, 0x34f9ab36, 0x9fa01fe7, 0x624ac294, 0xc9137645,
    0x26459ddd, 0x8d1c290c, 0x70f6f47f, 0xdbaf40ae, 0x207afa48, 0x8b234e99,
    0x76c993ea, 0xdd90273b, 0x2a3b52f7, 0x8162e626, 0x7c883b55, 0xd7d18f84,
    0x2c043562, 0x875d81b3, 0x7ab75cc0, 0xd1eee811,
];

#[rustfmt::skip]
static U: [u32; 256] = [
    0x00000000, 0x7eb5200d, 0x5633f4cb, 0x2886d4c6, 0x073e5d47, 0x798b7d4a,
    0x510da98c, 0x2fb88981, 0x0e7cba8e, 0x70c99a83, 0x584f4e45, 0x26fa6e48,
    0x0942e7c9, 0x77f7c7c4, 0x5f711302, 0x21c4330f, 0x1cf9751c, 0x624c5511,
    0x4aca81d7, 0x347fa1da, 0x1bc7285b, 0x65720856, 0x4df4dc90, 0x3341fc9d,
    0x1285cf92, 0x6c30ef9f, 0x44b63b59, 0x3a031b54, 0x15bb92d5, 0x6b0eb2d8,
    0x4388661e, 0x3d3d4613, 0x39f2ea38, 0x4747ca35, 0x6fc11ef3, 0x11743efe,
    0x3eccb77f, 0x40799772, 0x68ff43b4, 0x164a63b9, 0x378e50b6, 0x493b70bb,
    0x61bda47d, 0x1f088470, 0x30b00df1, 0x4e052dfc, 0x6683f93a, 0x1836d937,
    0x250b9f24, 0x5bbebf29, 0x73386bef, 0x0d8d4be2, 0x2235c263, 0x5c80e26e,
    0x740636a8, 0x0ab316a5, 0x2b7725aa, 0x55c205a7, 0x7d44d161, 0x03f1f16c,
    0x2c4978ed, 0x52fc58e0, 0x7a7a8c26, 0x04cfac2b, 0x73e5d470, 0x0d50f47d,
    0x25d620bb, 0x5b6300b6, 0x74db8937, 0x0a6ea93a, 0x22e87dfc, 0x5c5d5df1,
    0x7d996efe, 0x032c4ef3, 0x2baa9a35, 0x551fba38, 0x7aa733b9, 0x041213b4,
    0x2c94c772, 0x5221e77f, 0x6f1ca16c, 0x11a98161, 0x392f55a7, 0x479a75aa,
    0x6822fc2b, 0x1697dc26, 0x3e1108e0, 0x40a428ed, 0x61601be2, 0x1fd53bef,
    0x3753ef29, 0x49e6cf24, 0x665e46a5, 0x18eb66a8, 0x306db26e, 0x4ed89263,
    0x4a173e48, 0x34a21e45, 0x1c24ca83, 0x6291ea8e, 0x4d29630f, 0x339c4302,
    0x1b1a97c4, 0x65afb7c9, 0x446b84c6, 0x3adea4cb, 0x1258700d, 0x6ced5000,
    0x4355d981, 0x3de0f98c, 0x15662d4a, 0x6bd30d47, 0x56ee4b54, 0x285b6b59,
    0x00ddbf9f, 0x7e689f92, 0x51d01613, 0x2f65361e, 0x07e3e2d8, 0x7956c2d5,
    0x5892f1da, 0x2627d1d7, 0x0ea10511, 0x7014251c, 0x5facac9d, 0x21198c90,
    0x099f5856, 0x772a785b, 0x4c921c31, 0x32273c3c, 0x1aa1e8fa, 0x6414c8f7,
    0x4bac4176, 0x3519617b, 0x1d9fb5bd, 0x632a95b0, 0x42eea6bf, 0x3c5b86b2,
    0x14dd5274, 0x6a687279, 0x45d0fbf8, 0x3b65dbf5, 0x13e30f33, 0x6d562f3e,
    0x506b692d, 0x2ede4920, 0x06589de6, 0x78edbdeb, 0x5755346a, 0x29e01467,
    0x0166c0a1, 0x7fd3e0ac, 0x5e17d3a3, 0x20a2f3ae, 0x08242768, 0x76910765,
    0x59298ee4, 0x279caee9, 0x0f1a7a2f, 0x71af5a22, 0x7560f609, 0x0bd5d604,
    0x235302c2, 0x5de622cf, 0x725eab4e, 0x0ceb8b43, 0x246d5f85, 0x5ad87f88,
    0x7b1c4c87, 0x05a96c8a, 0x2d2fb84c, 0x539a9841, 0x7c2211c0, 0x029731cd,
    0x2a11e50b, 0x54a4c506, 0x69998315, 0x172ca318, 0x3faa77de, 0x411f57d3,
    0x6ea7de52, 0x1012fe5f, 0x38942a99, 0x46210a94, 0x67e5399b, 0x19501996,
    0x31d6cd50, 0x4f63ed5d, 0x60db64dc, 0x1e6e44d1, 0x36e89017, 0x485db01a,
    0x3f77c841, 0x41c2e84c, 0x69443c8a, 0x17f11c87, 0x38499506, 0x46fcb50b,
    0x6e7a61cd, 0x10cf41c0, 0x310b72cf, 0x4fbe52c2, 0x67388604, 0x198da609,
    0x36352f88, 0x48800f85, 0x6006db43, 0x1eb3fb4e, 0x238ebd5d, 0x5d3b9d50,
    0x75bd4996, 0x0b08699b, 0x24b0e01a, 0x5a05c017, 0x728314d1, 0x0c3634dc,
    0x2df207d3, 0x534727de, 0x7bc1f318, 0x0574d315, 0x2acc5a94, 0x54797a99,
    0x7cffae5f, 0x024a8e52, 0x06852279, 0x78300274, 0x50b6d6b2, 0x2e03f6bf,
    0x01bb7f3e, 0x7f0e5f33, 0x57888bf5, 0x293dabf8, 0x08f998f7, 0x764cb8fa,
    0x5eca6c3c, 0x207f4c31, 0x0fc7c5b0, 0x7172e5bd, 0x59f4317b, 0x27411176,
    0x1a7c5765, 0x64c97768, 0x4c4fa3ae, 0x32fa83a3, 0x1d420a22, 0x63f72a2f,
    0x4b71fee9, 0x35c4dee4, 0x1400edeb, 0x6ab5cde6, 0x42331920, 0x3c86392d,
    0x133eb0ac, 0x6d8b90a1, 0x450d4467, 0x3bb8646a,
];

/// One indexed position in the source buffer: the offset just past a Rabin
/// window, and that window's fingerprint. git's `struct index_entry`, with the
/// `const unsigned char *ptr` replaced by an offset into the same buffer.
#[derive(Clone, Copy)]
struct IndexEntry {
    /// Offset into the source buffer, one past the end of the hashed window.
    ptr: usize,
    val: u32,
}

/// A node of the temporary per-bucket list built while indexing; git's
/// `struct unpacked_index_entry`.
#[derive(Clone, Copy)]
struct Node {
    entry: IndexEntry,
    next: Option<usize>,
}

/// A Rabin-fingerprint index over one source buffer, git's `struct delta_index`.
///
/// Build it once per delta *base* and reuse it for every target tried against
/// that base — which is exactly what the sliding window does, and why git keeps
/// the index alive in the window slot rather than rebuilding it per pair.
/// `B` is whatever holds the source bytes: a borrowed slice for a one-shot
/// delta, or a shared buffer for the sliding window, which keeps one index alive
/// across many targets while the same bytes are also read as a delta target.
pub struct Index<B> {
    src: B,
    hash_mask: u32,
    /// `hash[i] .. hash[i + 1]` delimits bucket `i` inside `entries`; the array
    /// has `hash_size + 1` elements, the last being git's sentinel.
    hash: Vec<u32>,
    entries: Vec<IndexEntry>,
}

impl<B: AsRef<[u8]>> Index<B> {
    /// Index `src`, or `None` for an empty buffer — git's `create_delta_index()`
    /// returning `NULL` for `!bufsize`.
    pub fn new(holder: B) -> Option<Self> {
        let src = holder.as_ref();
        if src.is_empty() {
            return None;
        }

        // Indexing skips the first byte so that `create()` can optimize the
        // Rabin polynomial's initialization.
        let mut entries = (src.len() - 1) / RABIN_WINDOW;
        if src.len() >= 0xffff_ffff {
            // The delta format cannot encode a base offset wider than 32 bits.
            entries = 0xffff_fffe / RABIN_WINDOW;
        }
        let mut hash_size = entries / 4;
        let mut shift = 4;
        while (1usize << shift) < hash_size {
            shift += 1;
        }
        hash_size = 1 << shift;
        let hash_mask = (hash_size - 1) as u32;

        // Populate the index back to front, so that the head of each bucket
        // list ends up being its lowest offset.
        let mut nodes: Vec<Node> = Vec::with_capacity(entries);
        let mut heads: Vec<Option<usize>> = vec![None; hash_size];
        let mut counts: Vec<usize> = vec![0; hash_size];
        let mut prev_val = u32::MAX;
        for block in (1..=entries).rev() {
            let at = (block - 1) * RABIN_WINDOW;
            let mut val = 0u32;
            for i in 1..=RABIN_WINDOW {
                val = ((val << 8) | u32::from(src[at + i])) ^ T[(val >> RABIN_SHIFT) as usize];
            }
            if val == prev_val {
                // Keep the lowest of consecutive identical blocks.
                if let Some(last) = nodes.last_mut() {
                    last.entry.ptr = at + RABIN_WINDOW;
                }
            } else {
                prev_val = val;
                let bucket = (val & hash_mask) as usize;
                nodes.push(Node {
                    entry: IndexEntry {
                        ptr: at + RABIN_WINDOW,
                        val,
                    },
                    next: heads[bucket],
                });
                heads[bucket] = Some(nodes.len() - 1);
                counts[bucket] += 1;
            }
        }

        // Cap each bucket at `HASH_LIMIT`, culling uniformly so that what stays
        // is still spread across the whole source buffer. git's accumulator
        // walk, node for node.
        for bucket in 0..hash_size {
            let count = counts[bucket];
            if count <= HASH_LIMIT {
                continue;
            }
            let step = (count - HASH_LIMIT) as isize;
            let mut acc: isize = 0;
            let mut cursor = heads[bucket];
            while let Some(keep) = cursor {
                acc += step;
                if acc > 0 {
                    // Skip forward until the accumulator is spent, then splice
                    // out everything walked past. git's comment proves the walk
                    // never runs off the end of the list.
                    let mut last = keep;
                    loop {
                        last = nodes[last].next.expect("the bucket is long enough by construction");
                        acc -= HASH_LIMIT as isize;
                        if acc <= 0 {
                            break;
                        }
                    }
                    let after = nodes[last].next;
                    nodes[keep].next = after;
                    cursor = after;
                } else {
                    cursor = nodes[keep].next;
                }
            }
        }

        // Flatten the lists into one array with a per-bucket start table, so
        // the match search is a contiguous scan.
        let mut hash: Vec<u32> = Vec::with_capacity(hash_size + 1);
        let mut flat: Vec<IndexEntry> = Vec::with_capacity(nodes.len());
        for bucket in 0..hash_size {
            hash.push(flat.len() as u32);
            let mut cursor = heads[bucket];
            while let Some(at) = cursor {
                flat.push(nodes[at].entry);
                cursor = nodes[at].next;
            }
        }
        hash.push(flat.len() as u32);

        Some(Index {
            src: holder,
            hash_mask,
            hash,
            entries: flat,
        })
    }

    /// How much memory this index occupies, used by the delta search to honour
    /// `pack.windowMemory`. git's `sizeof_delta_index()`.
    pub fn memory_usage(&self) -> u64 {
        (self.hash.len() * size_of::<u32>() + self.entries.len() * size_of::<IndexEntry>()) as u64
    }

    /// The source buffer this index describes.
    pub fn source(&self) -> &[u8] {
        self.src.as_ref()
    }

    /// Produce a delta rebuilding `target` from this index's source buffer, or
    /// `None` if no delta at most `max_size` bytes long exists.
    ///
    /// `max_size` of zero means "no limit", matching git's `!max_size` test.
    /// The caller uses the limit to reject a delta that would not pay for
    /// itself; a `None` here is the ordinary outcome for unrelated objects, not
    /// an error.
    pub fn create(&self, target: &[u8], max_size: u64) -> Option<Vec<u8>> {
        if target.is_empty() {
            return None;
        }
        let src = self.src.as_ref();
        let max_size = usize::try_from(max_size).unwrap_or(usize::MAX);

        let mut outsize = 8192;
        if max_size != 0 && outsize >= max_size {
            outsize = max_size + MAX_OP_SIZE + 1;
        }
        let mut out = vec![0u8; outsize];
        let mut outpos = 0usize;

        // Header: the base buffer size, then the target buffer size, both as
        // little-endian varints.
        for mut size in [src.len(), target.len()] {
            while size >= 0x80 {
                out[outpos] = (size as u8) | 0x80;
                outpos += 1;
                size >>= 7;
            }
            out[outpos] = size as u8;
            outpos += 1;
        }

        let mut data = 0usize;
        let top = target.len();

        // Reserve the first insert-count slot, then prime the rolling hash with
        // the first window of target bytes, which are inserted literally.
        outpos += 1;
        let mut val = 0u32;
        let mut i = 0usize;
        while i < RABIN_WINDOW && data < top {
            out[outpos] = target[data];
            outpos += 1;
            val = ((val << 8) | u32::from(target[data])) ^ T[(val >> RABIN_SHIFT) as usize];
            i += 1;
            data += 1;
        }
        let mut inscnt: isize = i as isize;

        let mut moff = 0usize;
        let mut msize = 0usize;
        while data < top {
            if msize < 4096 {
                val ^= U[usize::from(target[data - RABIN_WINDOW])];
                val = ((val << 8) | u32::from(target[data])) ^ T[(val >> RABIN_SHIFT) as usize];
                let bucket = (val & self.hash_mask) as usize;
                let (from, to) = (self.hash[bucket] as usize, self.hash[bucket + 1] as usize);
                for entry in &self.entries[from..to] {
                    let mut r = entry.ptr;
                    let mut s = data;
                    let mut ref_size = src.len() - r;
                    if entry.val != val {
                        continue;
                    }
                    if ref_size > top - s {
                        ref_size = top - s;
                    }
                    if ref_size <= msize {
                        break;
                    }
                    while ref_size > 0 && target[s] == src[r] {
                        ref_size -= 1;
                        s += 1;
                        r += 1;
                    }
                    if msize < r - entry.ptr {
                        // This is our best match so far.
                        msize = r - entry.ptr;
                        moff = entry.ptr;
                        if msize >= 4096 {
                            break; // good enough
                        }
                    }
                }
            }

            if msize < 4 {
                if inscnt == 0 {
                    outpos += 1;
                }
                out[outpos] = target[data];
                outpos += 1;
                data += 1;
                inscnt += 1;
                if inscnt == 0x7f {
                    out[outpos - inscnt as usize - 1] = inscnt as u8;
                    inscnt = 0;
                }
                msize = 0;
            } else {
                if inscnt != 0 {
                    while moff > 0 && src[moff - 1] == target[data - 1] {
                        // We can match one byte further back, which shortens
                        // the literal run by one.
                        msize += 1;
                        moff -= 1;
                        data -= 1;
                        outpos -= 1;
                        inscnt -= 1;
                        if inscnt != 0 {
                            continue;
                        }
                        outpos -= 1; // remove the count slot
                        inscnt -= 1; // make it -1
                        break;
                    }
                    if inscnt >= 0 {
                        out[outpos - inscnt as usize - 1] = inscnt as u8;
                    }
                    inscnt = 0;
                }

                // A copy op is limited to 64 KiB in pack v2; the rest becomes a
                // second copy on the next iteration.
                let left = if msize < 0x10000 { 0 } else { msize - 0x10000 };
                msize -= left;

                let op = outpos;
                outpos += 1;
                let mut cmd = 0x80u8;

                if moff & 0x0000_00ff != 0 {
                    out[outpos] = moff as u8;
                    outpos += 1;
                    cmd |= 0x01;
                }
                if moff & 0x0000_ff00 != 0 {
                    out[outpos] = (moff >> 8) as u8;
                    outpos += 1;
                    cmd |= 0x02;
                }
                if moff & 0x00ff_0000 != 0 {
                    out[outpos] = (moff >> 16) as u8;
                    outpos += 1;
                    cmd |= 0x04;
                }
                if moff & 0xff00_0000 != 0 {
                    out[outpos] = (moff >> 24) as u8;
                    outpos += 1;
                    cmd |= 0x08;
                }

                if msize & 0x00ff != 0 {
                    out[outpos] = msize as u8;
                    outpos += 1;
                    cmd |= 0x10;
                }
                if msize & 0xff00 != 0 {
                    out[outpos] = (msize >> 8) as u8;
                    outpos += 1;
                    cmd |= 0x20;
                }

                out[op] = cmd;

                data += msize;
                moff += msize;
                msize = left;

                if moff > 0xffff_ffff {
                    msize = 0;
                }

                if msize < 4096 {
                    val = 0;
                    for j in (1..=RABIN_WINDOW).rev() {
                        val = ((val << 8) | u32::from(target[data - j])) ^ T[(val >> RABIN_SHIFT) as usize];
                    }
                }
            }

            if outpos >= outsize - MAX_OP_SIZE {
                outsize = outsize * 3 / 2;
                if max_size != 0 && outsize >= max_size {
                    outsize = max_size + MAX_OP_SIZE + 1;
                }
                if max_size != 0 && outpos > max_size {
                    break;
                }
                out.resize(outsize, 0);
            }
        }

        if inscnt > 0 {
            out[outpos - inscnt as usize - 1] = inscnt as u8;
        }

        if max_size != 0 && outpos > max_size {
            return None;
        }

        out.truncate(outpos);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::Index;

    /// Rebuild `target` from `base` through the crate's own delta applier, the
    /// same code path a git reader takes, so a delta that round-trips here is
    /// one git can decode.
    fn round_trip(base: &[u8], target: &[u8]) {
        let index = Index::new(base).expect("non-empty base");
        let delta = index.create(target, 0).expect("a delta always exists without a size cap");

        let (base_size, consumed) = crate::data::delta::decode_header_size(&delta).expect("base size");
        assert_eq!(base_size as usize, base.len(), "delta names the base size");
        let (result_size, consumed2) =
            crate::data::delta::decode_header_size(&delta[consumed..]).expect("result size");
        assert_eq!(result_size as usize, target.len(), "delta names the result size");

        let mut out = vec![0u8; target.len()];
        crate::data::delta::apply(base, &mut out, &delta[consumed + consumed2..]).expect("delta applies");
        assert_eq!(out, target, "the delta rebuilds the target exactly");
    }

    #[test]
    fn round_trips_related_buffers() {
        let base: Vec<u8> = (0..40_000u32).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
        let mut target = base.clone();
        target.splice(1000..1000, b"an inserted paragraph\n".iter().copied());
        target.truncate(target.len() - 5_000);
        target.extend_from_slice(b"a tail that has no counterpart in the base at all\n");
        round_trip(&base, &target);
    }

    #[test]
    fn round_trips_unrelated_buffers() {
        round_trip(b"the quick brown fox", b"jumps over the lazy dog, repeatedly and at length");
    }

    #[test]
    fn round_trips_identical_buffers() {
        let base: Vec<u8> = (0..5_000u32).flat_map(|n| n.to_le_bytes()).collect();
        round_trip(&base, &base);
    }

    #[test]
    fn round_trips_copies_longer_than_64k() {
        // Forces the `left` split, where one match spills into a second copy op.
        let base: Vec<u8> = (0..300_000u32).map(|n| (n % 251) as u8).collect();
        let mut target = base.clone();
        target.extend_from_slice(b"tail");
        round_trip(&base, &target);
    }

    #[test]
    fn round_trips_highly_repetitive_buffers() {
        // Every window hashes the same, exercising the HASH_LIMIT culling.
        let base = vec![b'x'; 200_000];
        let mut target = vec![b'x'; 200_000];
        target[100_000] = b'y';
        round_trip(&base, &target);
    }

    #[test]
    fn round_trips_short_buffers() {
        for len in 0..64usize {
            let base: Vec<u8> = (0..len).map(|n| n as u8).collect();
            let target: Vec<u8> = (0..len).map(|n| (n as u8).wrapping_add(1)).collect();
            if base.is_empty() {
                assert!(Index::new(&base).is_none(), "an empty base cannot be indexed");
                continue;
            }
            if target.is_empty() {
                continue;
            }
            round_trip(&base, &target);
        }
    }

    #[test]
    fn respects_the_size_cap() {
        let base = b"nothing in common".as_slice();
        let target: Vec<u8> = (0..10_000u32).flat_map(|n| n.to_le_bytes()).collect();
        let index = Index::new(base).expect("non-empty base");
        assert!(
            index.create(&target, 64).is_none(),
            "a delta that cannot fit the cap is rejected rather than truncated"
        );
    }
}
