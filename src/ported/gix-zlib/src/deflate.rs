//! A transcription of zlib's `deflate.c` and `trees.c`.
//!
//! The compressed byte stream a VCS writes is not an implementation detail: two
//! clones of the same history are expected to hold the same bytes on disk, and a
//! pack or bundle streamed to a peer is compared byte for byte against the one
//! `git` would have produced. `git` links zlib, so zlib — not "some correct
//! deflate" — is the specification.
//!
//! `zlib-rs`, which backs [`crate::Inflate`], descends from zlib-ng and uses
//! different match finders at levels 1 through 8; its output is a valid deflate
//! stream but a different one (measured against zlib 1.2.12, 1.3.1 and 1.3.2,
//! which all agree with each other). Decoding is unaffected — a deflate stream
//! decodes to exactly one byte sequence whoever wrote it — so `zlib-rs` stays on
//! the inflate side, where it is faster and nothing observable depends on the
//! encoder's choices.
//!
//! Everything observable in the output is reproduced here: the
//! `configuration_table` per level, `deflate_stored` / `deflate_fast` /
//! `deflate_slow`, `longest_match`'s unrolled eight-way compare and its chain
//! limits, the lazy-match `TOO_FAR` rule, the dynamic-versus-static-versus-stored
//! block decision, the bit-length overflow repair in `gen_bitlen`, and the
//! `high_water` window zeroing that makes matches past the end of the data
//! deterministic. This is a transcription rather than a reimplementation,
//! deliberately: anything "improved" along the way would be a different encoder.
//!
//! # Buffer sizes are part of the output at level 0
//!
//! At levels 1 through 9 the compressed bytes depend only on the level and the
//! input. At level 0 they do not: `deflate_stored()` sizes its blocks from
//! `avail_in` and `avail_out`, so a caller that wants to match `git` at level 0
//! has to feed input and drain output in the same sized pieces `git` does.
//! [`Deflate`] therefore exposes the `z_stream` buffer bookkeeping rather than
//! hiding it, so such a caller can.

#![allow(clippy::needless_range_loop)]

/// Which of the three framings deflate can wrap its blocks in.
///
/// `git` uses all three: `deflateInit()` (zlib) for loose objects, pack entries
/// and binary patches, `deflateInit2(..., -MAX_WBITS, ...)` (raw) for zip entry
/// payloads, and the gzip framing for `archive --format=tgz`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// No header and no trailer: bare deflate blocks, zlib's `windowBits = -15`.
    Raw,
    /// The two-byte zlib header and the big-endian Adler-32 trailer, `windowBits = 15`.
    Zlib,
    /// The gzip header and the little-endian CRC-32 and length trailer, `windowBits = 15 + 16`.
    Gzip,
}

impl Wrap {
    /// zlib's internal `s->wrap`.
    fn code(self) -> i32 {
        match self {
            Wrap::Raw => 0,
            Wrap::Zlib => 1,
            Wrap::Gzip => 2,
        }
    }
}

/// zlib's `Z_NO_FLUSH`: consume input, emit whatever completed blocks fall out.
pub const Z_NO_FLUSH: i32 = 0;
/// zlib's `Z_PARTIAL_FLUSH`: end the block and append an empty static block, so the
/// decompressor can reach every byte read so far without aligning to a byte boundary.
pub const Z_PARTIAL_FLUSH: i32 = 1;
/// zlib's `Z_SYNC_FLUSH`: end the block and append an empty stored block, aligning
/// the output on a byte boundary.
pub const Z_SYNC_FLUSH: i32 = 2;
/// zlib's `Z_FULL_FLUSH`: `Z_SYNC_FLUSH` plus forgetting the history, so
/// decompression can restart from this point.
pub const Z_FULL_FLUSH: i32 = 3;
/// zlib's `Z_FINISH`: no more input is coming, close the stream.
pub const Z_FINISH: i32 = 4;
/// zlib's `Z_OK`.
pub const Z_OK: i32 = 0;
/// zlib's `Z_STREAM_END`: the trailer has been written, the stream is complete.
pub const Z_STREAM_END: i32 = 1;
/// zlib's `Z_BUF_ERROR`: no progress was possible, which is not fatal.
pub const Z_BUF_ERROR: i32 = -5;

/// zlib's `adler32()`.
///
/// Split into `NMAX`-byte runs for the same reason zlib does: 5552 is the
/// largest number of bytes whose sums are guaranteed not to overflow 32 bits, so
/// the modulo only has to run once per run rather than once per byte.
fn adler32(adler: u32, data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    const NMAX: usize = 5552;

    let mut s1 = adler & 0xffff;
    let mut s2 = (adler >> 16) & 0xffff;
    for chunk in data.chunks(NMAX) {
        for b in chunk {
            s1 += u32::from(*b);
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
    }
    (s2 << 16) | s1
}


const MAX_MATCH: usize = 258;
const MIN_MATCH: usize = 3;
const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const BUF_SIZE: i32 = 16;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const WIN_INIT: usize = MAX_MATCH;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;
const TOO_FAR: usize = 4096;
const MAX_STORED: usize = 65535;

/// `deflateInit2(..., windowBits = 15, memLevel = 8, ...)`, as git uses.
const W_BITS: usize = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const HASH_BITS: usize = 8 + 7;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const HASH_SHIFT: usize = HASH_BITS.div_ceil(MIN_MATCH);
const LIT_BUFSIZE: usize = 1 << (8 + 6);
const PENDING_BUF_SIZE: usize = LIT_BUFSIZE * 4;
const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;
const WINDOW_SIZE: usize = 2 * W_SIZE;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;

static EXTRA_LBITS: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
static EXTRA_DBITS: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
static EXTRA_BLBITS: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// `configuration_table`: good_length, max_lazy, nice_length, max_chain.
const CONFIG: [(u16, u16, u16, u16); 10] = [
    (0, 0, 0, 0),
    (4, 4, 8, 4),
    (4, 5, 16, 8),
    (4, 6, 32, 32),
    (4, 4, 16, 16),
    (8, 16, 32, 32),
    (8, 16, 128, 128),
    (8, 32, 128, 256),
    (32, 128, 258, 1024),
    (32, 258, 258, 4096),
];

/// zlib's `ct_data`: a union of (`freq`, `code`) and (`dad`, `len`).
#[derive(Clone, Copy, Default)]
struct Ct {
    fc: u16,
    dl: u16,
}

/// The three tree kinds, indexed the way `deflate_state` names them.
const TREE_L: usize = 0;
const TREE_D: usize = 1;
const TREE_BL: usize = 2;

/// `tr_static_init()`: the tables zlib computes once per process.
struct Tables {
    static_ltree: Vec<Ct>,
    static_dtree: Vec<Ct>,
    dist_code: [u8; 512],
    length_code: [u8; 256],
    base_length: [i32; LENGTH_CODES],
    base_dist: [i32; D_CODES],
}

fn bi_reverse(mut code: u32, mut len: i32) -> u32 {
    let mut res = 0u32;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len <= 0 {
            break;
        }
    }
    res >> 1
}

fn gen_codes(tree: &mut [Ct], max_code: i32, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    for bits in 1..=MAX_BITS {
        code = (code + u32::from(bl_count[bits - 1])) << 1;
        next_code[bits] = code as u16;
    }
    for n in 0..=max_code.max(-1) {
        let n = n as usize;
        let len = tree[n].dl as usize;
        if len == 0 {
            continue;
        }
        tree[n].fc = bi_reverse(u32::from(next_code[len]), len as i32) as u16;
        next_code[len] += 1;
    }
}

impl Tables {
    fn new() -> Self {
        let mut length_code = [0u8; 256];
        let mut base_length = [0i32; LENGTH_CODES];
        let mut length = 0usize;
        for code in 0..LENGTH_CODES - 1 {
            base_length[code] = length as i32;
            for _ in 0..(1 << EXTRA_LBITS[code]) {
                length_code[length] = code as u8;
                length += 1;
            }
        }
        // Match length 258 is cheaper as code 285 than as 284 + 5 extra bits.
        length_code[length - 1] = (LENGTH_CODES - 1) as u8;

        let mut dist_code = [0u8; 512];
        let mut base_dist = [0i32; D_CODES];
        let mut dist = 0usize;
        for code in 0..16 {
            base_dist[code] = dist as i32;
            for _ in 0..(1 << EXTRA_DBITS[code]) {
                dist_code[dist] = code as u8;
                dist += 1;
            }
        }
        dist >>= 7;
        for code in 16..D_CODES {
            base_dist[code] = (dist as i32) << 7;
            for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                dist_code[256 + dist] = code as u8;
                dist += 1;
            }
        }

        let mut bl_count = [0u16; MAX_BITS + 1];
        let mut static_ltree = vec![Ct::default(); L_CODES + 2];
        for n in 0..=143 {
            static_ltree[n].dl = 8;
            bl_count[8] += 1;
        }
        for n in 144..=255 {
            static_ltree[n].dl = 9;
            bl_count[9] += 1;
        }
        for n in 256..=279 {
            static_ltree[n].dl = 7;
            bl_count[7] += 1;
        }
        for n in 280..=287 {
            static_ltree[n].dl = 8;
            bl_count[8] += 1;
        }
        gen_codes(&mut static_ltree, (L_CODES + 1) as i32, &bl_count);

        let mut static_dtree = vec![Ct::default(); D_CODES];
        for n in 0..D_CODES {
            static_dtree[n].dl = 5;
            static_dtree[n].fc = bi_reverse(n as u32, 5) as u16;
        }

        Tables {
            static_ltree,
            static_dtree,
            dist_code,
            length_code,
            base_length,
            base_dist,
        }
    }

    /// zlib's `d_code()`.
    fn d_code(&self, dist: usize) -> usize {
        if dist < 256 {
            self.dist_code[dist] as usize
        } else {
            self.dist_code[256 + (dist >> 7)] as usize
        }
    }
}

/// `deflate_state` plus the parts of `z_stream` deflate actually reads.
struct State {
    level: i32,
    status_header: bool,
    status_finish: bool,
    last_flush: i32,
    wrap: i32,

    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    pending_buf: Vec<u8>,
    pending: usize,
    pending_out: usize,

    ins_h: usize,
    block_start: i64,
    match_length: usize,
    prev_match: usize,
    match_available: bool,
    strstart: usize,
    match_start: usize,
    lookahead: usize,
    prev_length: usize,
    max_chain_length: usize,
    max_lazy_match: usize,
    good_match: usize,
    nice_match: i32,
    insert: usize,
    matches: u32,
    high_water: usize,

    trees: [Vec<Ct>; 3],
    max_code: [i32; 3],
    bl_count: [u16; MAX_BITS + 1],
    heap: [i32; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    sym_next: usize,
    opt_len: u64,
    static_len: u64,
    bi_buf: u16,
    bi_valid: i32,

    // z_stream
    avail_in: usize,
    next_in: usize,
    total_in: u64,
    avail_out: usize,
    next_out: usize,
    total_out: u64,
    check: u32,
}

/// zlib's `crc32()`: the IEEE CRC-32 used by the gzip wrapper and by zip entries.
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    // Table-driven CRC-32 (IEEE), the polynomial zlib's crc32() uses.
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for n in 0..256usize {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            t[n] = c;
        }
        t
    });
    let mut c = !crc;
    for b in data {
        c = table[((c ^ u32::from(*b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

impl State {
    fn new(level: i32, wrap: Wrap) -> Self {
        let level = if level == -1 { 6 } else { level };
        let cfg = CONFIG[level as usize];
        State {
            level,
            status_header: !matches!(wrap, Wrap::Raw),
            status_finish: false,
            last_flush: -2,
            wrap: wrap.code(),
            // The extra MAX_MATCH bytes keep `longest_match`'s scan in bounds;
            // zlib relies on the same slack past the window.
            window: vec![0; WINDOW_SIZE + MAX_MATCH],
            prev: vec![0; W_SIZE],
            head: vec![0; HASH_SIZE],
            pending_buf: vec![0; PENDING_BUF_SIZE],
            pending: 0,
            pending_out: 0,
            ins_h: 0,
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: MIN_MATCH - 1,
            max_chain_length: cfg.3 as usize,
            max_lazy_match: cfg.1 as usize,
            good_match: cfg.0 as usize,
            nice_match: i32::from(cfg.2),
            insert: 0,
            matches: 0,
            high_water: 0,
            trees: [
                vec![Ct::default(); HEAP_SIZE],
                vec![Ct::default(); 2 * D_CODES + 1],
                vec![Ct::default(); 2 * BL_CODES + 1],
            ],
            max_code: [0; 3],
            bl_count: [0; MAX_BITS + 1],
            heap: [0; HEAP_SIZE],
            heap_len: 0,
            heap_max: 0,
            depth: [0; HEAP_SIZE],
            sym_next: 0,
            opt_len: 0,
            static_len: 0,
            bi_buf: 0,
            bi_valid: 0,
            avail_in: 0,
            next_in: 0,
            total_in: 0,
            avail_out: 0,
            next_out: 0,
            total_out: 0,
            check: if matches!(wrap, Wrap::Zlib) { 1 } else { 0 },
        }
    }

    fn init_block(&mut self) {
        for n in 0..L_CODES {
            self.trees[TREE_L][n].fc = 0;
        }
        for n in 0..D_CODES {
            self.trees[TREE_D][n].fc = 0;
        }
        for n in 0..BL_CODES {
            self.trees[TREE_BL][n].fc = 0;
        }
        self.trees[TREE_L][END_BLOCK].fc = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym_next = 0;
        self.matches = 0;
    }

    fn put_byte(&mut self, b: u8) {
        self.pending_buf[self.pending] = b;
        self.pending += 1;
    }

    fn put_short(&mut self, w: u16) {
        self.put_byte((w & 0xff) as u8);
        self.put_byte((w >> 8) as u8);
    }

    fn send_bits(&mut self, value: i32, len: i32) {
        if self.bi_valid > BUF_SIZE - len {
            let val = value as u16 as u32;
            self.bi_buf |= (val << self.bi_valid) as u16;
            let b = self.bi_buf;
            self.put_short(b);
            self.bi_buf = (val >> (BUF_SIZE - self.bi_valid)) as u16;
            self.bi_valid += len - BUF_SIZE;
        } else {
            self.bi_buf |= ((value as u16 as u32) << self.bi_valid) as u16;
            self.bi_valid += len;
        }
    }

    fn send_code(&mut self, c: usize, tree: usize) {
        let t = self.trees[tree][c];
        self.send_bits(i32::from(t.fc), i32::from(t.dl));
    }

    fn bi_flush(&mut self) {
        if self.bi_valid == 16 {
            let b = self.bi_buf;
            self.put_short(b);
            self.bi_buf = 0;
            self.bi_valid = 0;
        } else if self.bi_valid >= 8 {
            let b = self.bi_buf as u8;
            self.put_byte(b);
            self.bi_buf >>= 8;
            self.bi_valid -= 8;
        }
    }

    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let b = self.bi_buf;
            self.put_short(b);
        } else if self.bi_valid > 0 {
            let b = self.bi_buf as u8;
            self.put_byte(b);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
    }

    fn smaller(&self, k: usize, n: i32, m: i32) -> bool {
        let t = &self.trees[k];
        let (n, m) = (n as usize, m as usize);
        t[n].fc < t[m].fc || (t[n].fc == t[m].fc && self.depth[n] <= self.depth[m])
    }

    fn pqdownheap(&mut self, k: usize, mut node: usize) {
        let v = self.heap[node];
        let mut j = node << 1;
        while j <= self.heap_len {
            if j < self.heap_len && self.smaller(k, self.heap[j + 1], self.heap[j]) {
                j += 1;
            }
            if self.smaller(k, v, self.heap[j]) {
                break;
            }
            self.heap[node] = self.heap[j];
            node = j;
            j <<= 1;
        }
        self.heap[node] = v;
    }

    /// The `static_tree`, `extra_bits`, `extra_base` and `max_length` of a tree.
    fn desc<'t>(&self, t: &'t Tables, k: usize) -> (Option<&'t [Ct]>, &'static [i32], usize, usize) {
        match k {
            TREE_L => (
                Some(&t.static_ltree),
                &EXTRA_LBITS,
                LITERALS + 1,
                MAX_BITS,
            ),
            TREE_D => (Some(&t.static_dtree), &EXTRA_DBITS, 0, MAX_BITS),
            _ => (None, &EXTRA_BLBITS, 0, MAX_BL_BITS),
        }
    }

    fn elems(k: usize) -> usize {
        match k {
            TREE_L => L_CODES,
            TREE_D => D_CODES,
            _ => BL_CODES,
        }
    }

    fn gen_bitlen(&mut self, t: &Tables, k: usize) {
        let (stree, extra, base, max_length) = self.desc(t, k);
        let max_code = self.max_code[k];
        let mut overflow = 0i32;

        self.bl_count = [0; MAX_BITS + 1];

        let root = self.heap[self.heap_max] as usize;
        self.trees[k][root].dl = 0;

        for h in self.heap_max + 1..HEAP_SIZE {
            let n = self.heap[h] as usize;
            let dad = self.trees[k][n].dl as usize;
            let mut bits = self.trees[k][dad].dl as usize + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            self.trees[k][n].dl = bits as u16;

            if n as i32 > max_code {
                continue;
            }
            self.bl_count[bits] += 1;
            let mut xbits = 0i32;
            if n >= base {
                xbits = extra[n - base];
            }
            let f = u64::from(self.trees[k][n].fc);
            self.opt_len = self.opt_len.wrapping_add(f * (bits as u64 + xbits as u64));
            if let Some(s) = stree {
                self.static_len = self
                    .static_len
                    .wrapping_add(f * (u64::from(s[n].dl) + xbits as u64));
            }
        }
        if overflow == 0 {
            return;
        }

        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }

        let mut h = HEAP_SIZE;
        for bits in (1..=max_length).rev() {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = self.heap[h] as usize;
                if m as i32 > max_code {
                    continue;
                }
                if usize::from(self.trees[k][m].dl) != bits {
                    self.opt_len = self.opt_len.wrapping_add(
                        (bits as u64)
                            .wrapping_sub(u64::from(self.trees[k][m].dl))
                            .wrapping_mul(u64::from(self.trees[k][m].fc)),
                    );
                    self.trees[k][m].dl = bits as u16;
                }
                n -= 1;
            }
        }
    }

    fn build_tree(&mut self, t: &Tables, k: usize) {
        let (stree, _, _, _) = self.desc(t, k);
        let elems = Self::elems(k);
        let mut max_code: i32 = -1;

        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;

        for n in 0..elems {
            if self.trees[k][n].fc != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n as i32;
                max_code = n as i32;
                self.depth[n] = 0;
            } else {
                self.trees[k][n].dl = 0;
            }
        }

        while self.heap_len < 2 {
            self.heap_len += 1;
            let node = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap[self.heap_len] = node as i32;
            self.trees[k][node].fc = 1;
            self.depth[node] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if let Some(s) = stree {
                self.static_len = self.static_len.wrapping_sub(u64::from(s[node].dl));
            }
        }
        self.max_code[k] = max_code;

        for n in (1..=self.heap_len / 2).rev() {
            self.pqdownheap(k, n);
        }

        let mut node = elems;
        loop {
            // pqremove
            let n = self.heap[1];
            self.heap[1] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(k, 1);

            let m = self.heap[1];

            self.heap_max -= 1;
            self.heap[self.heap_max] = n;
            self.heap_max -= 1;
            self.heap[self.heap_max] = m;

            let (nu, mu) = (n as usize, m as usize);
            self.trees[k][node].fc = self.trees[k][nu].fc + self.trees[k][mu].fc;
            self.depth[node] = self.depth[nu].max(self.depth[mu]) + 1;
            self.trees[k][nu].dl = node as u16;
            self.trees[k][mu].dl = node as u16;

            self.heap[1] = node as i32;
            node += 1;
            self.pqdownheap(k, 1);

            if self.heap_len < 2 {
                break;
            }
        }

        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[1];

        self.gen_bitlen(t, k);
        let max_code = self.max_code[k];
        let bl = self.bl_count;
        gen_codes(&mut self.trees[k], max_code, &bl);
    }

    fn scan_tree(&mut self, k: usize) {
        let max_code = self.max_code[k];
        let mut prevlen: i32 = -1;
        let mut nextlen = self.trees[k][0].dl as i32;
        let mut count = 0i32;
        let mut max_count = 7i32;
        let mut min_count = 4i32;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        self.trees[k][(max_code + 1) as usize].dl = 0xffff;

        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = self.trees[k][(n + 1) as usize].dl as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                self.trees[TREE_BL][curlen as usize].fc += count as u16;
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.trees[TREE_BL][curlen as usize].fc += 1;
                }
                self.trees[TREE_BL][REP_3_6].fc += 1;
            } else if count <= 10 {
                self.trees[TREE_BL][REPZ_3_10].fc += 1;
            } else {
                self.trees[TREE_BL][REPZ_11_138].fc += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_tree(&mut self, k: usize) {
        let max_code = self.max_code[k];
        let mut prevlen: i32 = -1;
        let mut nextlen = self.trees[k][0].dl as i32;
        let mut count = 0i32;
        let mut max_count = 7i32;
        let mut min_count = 4i32;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }

        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = self.trees[k][(n + 1) as usize].dl as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                loop {
                    self.send_code(curlen as usize, TREE_BL);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_code(curlen as usize, TREE_BL);
                    count -= 1;
                }
                self.send_code(REP_3_6, TREE_BL);
                self.send_bits(count - 3, 2);
            } else if count <= 10 {
                self.send_code(REPZ_3_10, TREE_BL);
                self.send_bits(count - 3, 3);
            } else {
                self.send_code(REPZ_11_138, TREE_BL);
                self.send_bits(count - 11, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn build_bl_tree(&mut self, t: &Tables) -> usize {
        self.scan_tree(TREE_L);
        self.scan_tree(TREE_D);
        self.build_tree(t, TREE_BL);

        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.trees[TREE_BL][BL_ORDER[max_blindex]].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.opt_len = self
            .opt_len
            .wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
        max_blindex
    }

    fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
        self.send_bits(lcodes as i32 - 257, 5);
        self.send_bits(dcodes as i32 - 1, 5);
        self.send_bits(blcodes as i32 - 4, 4);
        for rank in 0..blcodes {
            let len = self.trees[TREE_BL][BL_ORDER[rank]].dl;
            self.send_bits(i32::from(len), 3);
        }
        self.max_code[TREE_L] = lcodes as i32 - 1;
        self.send_tree(TREE_L);
        self.max_code[TREE_D] = dcodes as i32 - 1;
        self.send_tree(TREE_D);
    }

    /// `_tr_align()`: end the block with an empty static block, which is what
    /// `Z_PARTIAL_FLUSH` appends.
    fn tr_align(&mut self, t: &Tables) {
        self.send_bits(1 << 1, 3);
        self.send_sym(t, END_BLOCK, false, false);
        self.bi_flush();
    }

    fn tr_stored_block(&mut self, buf: Option<usize>, stored_len: usize, last: bool) {
        self.send_bits(i32::from(last), 3);
        self.bi_windup();
        self.put_short(stored_len as u16);
        self.put_short(!(stored_len as u16));
        if stored_len != 0 {
            let start = buf.expect("stored block without a buffer");
            let (dst, src) = (self.pending, start);
            for i in 0..stored_len {
                self.pending_buf[dst + i] = self.window[src + i];
            }
        }
        self.pending += stored_len;
    }

    /// One entry of the literal/length or the distance tree, taken from the
    /// dynamic trees or from the static ones depending on what the block header
    /// announced.
    fn send_sym(&mut self, t: &Tables, c: usize, dynamic: bool, dist_tree: bool) {
        let cd = if dynamic {
            self.trees[if dist_tree { TREE_D } else { TREE_L }][c]
        } else if dist_tree {
            t.static_dtree[c]
        } else {
            t.static_ltree[c]
        };
        self.send_bits(i32::from(cd.fc), i32::from(cd.dl));
    }

    fn compress_block(&mut self, t: &Tables, dynamic: bool) {
        let mut sx = 0usize;
        if self.sym_next != 0 {
            loop {
                let mut dist = usize::from(self.pending_buf[LIT_BUFSIZE + sx]);
                sx += 1;
                dist += usize::from(self.pending_buf[LIT_BUFSIZE + sx]) << 8;
                sx += 1;
                let lc = self.pending_buf[LIT_BUFSIZE + sx];
                sx += 1;
                if dist == 0 {
                    self.send_sym(t, lc as usize, dynamic, false);
                } else {
                    let mut lc = i32::from(lc);
                    let code = usize::from(t.length_code[lc as usize]);
                    self.send_sym(t, code + LITERALS + 1, dynamic, false);
                    let extra = EXTRA_LBITS[code];
                    if extra != 0 {
                        lc -= t.base_length[code];
                        self.send_bits(lc, extra);
                    }
                    let mut dist = dist - 1;
                    let code = t.d_code(dist);
                    self.send_sym(t, code, dynamic, true);
                    let extra = EXTRA_DBITS[code];
                    if extra != 0 {
                        dist -= t.base_dist[code] as usize;
                        self.send_bits(dist as i32, extra);
                    }
                }
                if sx >= self.sym_next {
                    break;
                }
            }
        }
        self.send_sym(t, END_BLOCK, dynamic, false);
    }

    fn tr_flush_block(&mut self, t: &Tables, buf: Option<usize>, stored_len: usize, last: bool) {
        let opt_lenb;
        let static_lenb;
        let mut max_blindex = 0usize;

        if self.level > 0 {
            self.build_tree(t, TREE_L);
            self.build_tree(t, TREE_D);
            max_blindex = self.build_bl_tree(t);

            let mut o = (self.opt_len.wrapping_add(3 + 7)) >> 3;
            let s = (self.static_len.wrapping_add(3 + 7)) >> 3;
            if s <= o {
                o = s;
            }
            opt_lenb = o;
            static_lenb = s;
        } else {
            opt_lenb = stored_len as u64 + 5;
            static_lenb = opt_lenb;
        }

        if stored_len as u64 + 4 <= opt_lenb && buf.is_some() {
            self.tr_stored_block(buf, stored_len, last);
        } else if static_lenb == opt_lenb {
            self.send_bits((1 << 1) + i32::from(last), 3);
            self.compress_block(t, false);
        } else {
            self.send_bits((2 << 1) + i32::from(last), 3);
            let (lmax, dmax) = (self.max_code[TREE_L], self.max_code[TREE_D]);
            self.send_all_trees((lmax + 1) as usize, (dmax + 1) as usize, max_blindex + 1);
            self.compress_block(t, true);
        }

        self.init_block();
        if last {
            self.bi_windup();
        }
    }

    /// `_tr_tally()`: record one symbol, returning true when the block is full.
    fn tr_tally(&mut self, t: &Tables, dist: usize, lc: usize) -> bool {
        self.pending_buf[LIT_BUFSIZE + self.sym_next] = dist as u8;
        self.sym_next += 1;
        self.pending_buf[LIT_BUFSIZE + self.sym_next] = (dist >> 8) as u8;
        self.sym_next += 1;
        self.pending_buf[LIT_BUFSIZE + self.sym_next] = lc as u8;
        self.sym_next += 1;
        if dist == 0 {
            self.trees[TREE_L][lc].fc += 1;
        } else {
            self.matches += 1;
            let d = dist - 1;
            let li = usize::from(t.length_code[lc]) + LITERALS + 1;
            self.trees[TREE_L][li].fc += 1;
            let di = t.d_code(d);
            self.trees[TREE_D][di].fc += 1;
        }
        self.sym_next == SYM_END
    }

    fn slide_hash(&mut self) {
        for p in self.head.iter_mut() {
            let m = *p as usize;
            *p = if m >= W_SIZE { (m - W_SIZE) as u16 } else { 0 };
        }
        for p in self.prev.iter_mut() {
            let m = *p as usize;
            *p = if m >= W_SIZE { (m - W_SIZE) as u16 } else { 0 };
        }
    }

    fn update_hash(&mut self, c: u8) {
        self.ins_h = ((self.ins_h << HASH_SHIFT) ^ usize::from(c)) & HASH_MASK;
    }

    /// `INSERT_STRING`, returning the previous head of the chain.
    fn insert_string(&mut self, str_: usize) -> usize {
        let c = self.window[str_ + MIN_MATCH - 1];
        self.update_hash(c);
        let head = self.head[self.ins_h];
        self.prev[str_ & W_MASK] = head;
        self.head[self.ins_h] = str_ as u16;
        head as usize
    }

    fn read_buf_into_window(&mut self, input: &[u8], at: usize, size: usize) -> usize {
        let mut len = self.avail_in;
        if len > size {
            len = size;
        }
        if len == 0 {
            return 0;
        }
        self.avail_in -= len;
        let src = &input[self.next_in..self.next_in + len];
        self.window[at..at + len].copy_from_slice(src);
        self.update_check(src);
        self.next_in += len;
        self.total_in += len as u64;
        len
    }

    fn read_buf_into_out(&mut self, input: &[u8], out: &mut [u8], size: usize) -> usize {
        let mut len = self.avail_in;
        if len > size {
            len = size;
        }
        if len == 0 {
            return 0;
        }
        self.avail_in -= len;
        let src = &input[self.next_in..self.next_in + len];
        out[self.next_out..self.next_out + len].copy_from_slice(src);
        self.update_check(src);
        self.next_in += len;
        self.total_in += len as u64;
        len
    }

    /// zlib's `read_buf()` running check: Adler-32 for the zlib wrapper, CRC-32
    /// for gzip, and nothing at all for raw deflate.
    fn update_check(&mut self, data: &[u8]) {
        match self.wrap {
            1 => self.check = adler32(self.check, data),
            2 => self.check = crc32(self.check, data),
            _ => {}
        }
    }

    fn flush_pending(&mut self, out: &mut [u8]) {
        self.bi_flush();
        let mut len = self.pending;
        if len > self.avail_out {
            len = self.avail_out;
        }
        if len == 0 {
            return;
        }
        out[self.next_out..self.next_out + len]
            .copy_from_slice(&self.pending_buf[self.pending_out..self.pending_out + len]);
        self.next_out += len;
        self.pending_out += len;
        self.total_out += len as u64;
        self.avail_out -= len;
        self.pending -= len;
        if self.pending == 0 {
            self.pending_out = 0;
        }
    }

    fn fill_window(&mut self, input: &[u8]) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;

            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..2 * W_SIZE - more, 0);
                self.match_start -= W_SIZE;
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as i64;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            if self.avail_in == 0 {
                break;
            }

            let at = self.strstart + self.lookahead;
            let n = self.read_buf_into_window(input, at, more);
            self.lookahead += n;

            if self.lookahead + self.insert >= MIN_MATCH {
                let mut str_ = self.strstart - self.insert;
                self.ins_h = usize::from(self.window[str_]);
                let c = self.window[str_ + 1];
                self.update_hash(c);
                while self.insert != 0 {
                    let c = self.window[str_ + MIN_MATCH - 1];
                    self.update_hash(c);
                    self.prev[str_ & W_MASK] = self.head[self.ins_h];
                    self.head[self.ins_h] = str_ as u16;
                    str_ += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }

            if !(self.lookahead < MIN_LOOKAHEAD && self.avail_in != 0) {
                break;
            }
        }

        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let mut init = WINDOW_SIZE - curr;
                if init > WIN_INIT {
                    init = WIN_INIT;
                }
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let mut init = curr + WIN_INIT - self.high_water;
                if init > WINDOW_SIZE - self.high_water {
                    init = WINDOW_SIZE - self.high_water;
                }
                let hw = self.high_water;
                self.window[hw..hw + init].fill(0);
                self.high_water += init;
            }
        }
    }

    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = self.max_chain_length;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = self.nice_match as usize;
        let limit = self.strstart.saturating_sub(MAX_DIST);

        let strend = self.strstart + MAX_MATCH;
        let mut scan_end1 = self.window[scan + best_len - 1];
        let mut scan_end = self.window[scan + best_len];

        if self.prev_length >= self.good_match {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }

        loop {
            let m = cur_match;
            if self.window[m + best_len] == scan_end
                && self.window[m + best_len - 1] == scan_end1
                && self.window[m] == self.window[scan]
                && self.window[m + 1] == self.window[scan + 1]
            {
                // zlib compares in unrolled groups of eight and only then
                // rechecks `scan < strend`; MAX_MATCH-2 is a multiple of 8, so
                // the scan lands exactly on `strend` when everything matches.
                let mut sp = scan + 2;
                let mut mp = m + 2;
                'outer: loop {
                    for _ in 0..8 {
                        sp += 1;
                        mp += 1;
                        if self.window[sp] != self.window[mp] {
                            break 'outer;
                        }
                    }
                    if sp >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - sp);

                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = self.window[scan + best_len - 1];
                    scan_end = self.window[scan + best_len];
                }
            }
            cur_match = self.prev[cur_match & W_MASK] as usize;
            chain_length -= 1;
            if cur_match <= limit || chain_length == 0 {
                break;
            }
        }

        if best_len <= self.lookahead {
            best_len
        } else {
            self.lookahead
        }
    }
}

/// `deflate()`'s block-function return codes.
#[derive(PartialEq, Clone, Copy)]
enum BState {
    NeedMore,
    BlockDone,
    FinishStarted,
    FinishDone,
}

/// The tables `tr_static_init()` computes once per process. They depend on
/// nothing, so one copy is shared by every stream.
fn tables() -> &'static Tables {
    static TABLES: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    TABLES.get_or_init(Tables::new)
}

/// A deflate stream: zlib's `z_stream` and `deflate_state` together.
///
/// The buffer bookkeeping is deliberately explicit, mirroring `z_stream`, so a
/// caller can reproduce `git`'s framing exactly (see the module docs on level 0).
/// The usual shape is:
///
/// ```ignore
/// let mut z = Deflate::new(6, Wrap::Zlib);
/// z.set_input(input.len());
/// z.set_output(out.len());
/// let status = z.step(input, &mut out, Z_FINISH);
/// let produced = z.out_pos();
/// ```
pub struct Deflate {
    state: State,
    level: i32,
    wrap: Wrap,
}

impl Deflate {
    /// `deflateInit2(strm, level, Z_DEFLATED, windowBits, 8, Z_DEFAULT_STRATEGY)`
    /// with the `windowBits` implied by `wrap`, which is what every one of `git`'s
    /// `git_deflate_init*` wrappers does.
    ///
    /// `level` is 0 to 9, or -1 for zlib's default of 6.
    pub fn new(level: i32, wrap: Wrap) -> Self {
        let mut state = State::new(level, wrap);
        state.init_block();
        Deflate { state, level, wrap }
    }

    /// `deflateReset()`: begin a new stream with the same level and framing.
    pub fn reset(&mut self) {
        *self = Deflate::new(self.level, self.wrap);
    }

    /// Point the stream at `len` bytes of input, as `z_stream.avail_in`.
    ///
    /// The slice later handed to [`Deflate::step`] must be exactly the input that
    /// has not been consumed yet, and must be at least this long.
    pub fn set_input(&mut self, len: usize) {
        self.state.avail_in = len;
        self.state.next_in = 0;
    }

    /// Offer `cap` bytes of output space, as `z_stream.avail_out`, and rewind the
    /// write position to the start of the buffer.
    pub fn set_output(&mut self, cap: usize) {
        self.state.avail_out = cap;
        self.state.next_out = 0;
    }

    /// How many bytes of the output buffer the last [`Deflate::step`] filled.
    pub fn out_pos(&self) -> usize {
        self.state.next_out
    }

    /// The unconsumed part of the input offered by [`Deflate::set_input`].
    pub fn avail_in(&self) -> usize {
        self.state.avail_in
    }

    /// The unused part of the output space offered by [`Deflate::set_output`].
    pub fn avail_out(&self) -> usize {
        self.state.avail_out
    }

    /// Bytes of input consumed since the stream began.
    pub fn total_in(&self) -> u64 {
        self.state.total_in
    }

    /// Bytes of output produced since the stream began.
    pub fn total_out(&self) -> u64 {
        self.state.total_out
    }

    /// zlib's `deflate()`.
    ///
    /// `input` is the not-yet-consumed input and `out` the output buffer, whose
    /// lengths must match the last [`Deflate::set_input`] and
    /// [`Deflate::set_output`]. Returns [`Z_OK`], [`Z_STREAM_END`] or
    /// [`Z_BUF_ERROR`].
    pub fn step(&mut self, input: &[u8], out: &mut [u8], flush: i32) -> i32 {
        let t = tables();
        let s = &mut self.state;
        if s.avail_out == 0 {
            return Z_BUF_ERROR;
        }
        let old_flush = s.last_flush;
        s.last_flush = flush;

        if s.pending != 0 {
            s.flush_pending(out);
            if s.avail_out == 0 {
                s.last_flush = -1;
                return Z_OK;
            }
        } else if s.avail_in == 0 && flush <= old_flush && flush != Z_FINISH {
            return Z_BUF_ERROR;
        }

        if s.status_finish && s.avail_in != 0 {
            return Z_BUF_ERROR;
        }

        if s.status_header {
            s.status_header = false;
            if s.wrap == 2 {
                s.check = 0;
                s.put_byte(31);
                s.put_byte(139);
                s.put_byte(8);
                s.put_byte(0); // no extra, name, comment or header CRC
                for _ in 0..4 {
                    s.put_byte(0); // gzhead.time == 0
                }
                let xfl = if s.level == 9 {
                    2
                } else if s.level < 2 {
                    4
                } else {
                    0
                };
                s.put_byte(xfl);
                s.put_byte(3); // gzhead.os == 3 (Unix), as git sets it
            } else {
                // The zlib header: CM = 8 and CINFO = W_BITS - 8 in the first byte,
                // then the level hint, then a check value making the pair a
                // multiple of 31. No preset dictionary, so no PRESET_DICT bit.
                let mut header = ((8 + ((W_BITS as u32 - 8) << 4)) << 8) as u32;
                let level_flags: u32 = if s.level < 2 {
                    0
                } else if s.level < 6 {
                    1
                } else if s.level == 6 {
                    2
                } else {
                    3
                };
                header |= level_flags << 6;
                header += 31 - (header % 31);
                s.put_byte((header >> 8) as u8);
                s.put_byte(header as u8);
                s.check = 1;
            }
            s.flush_pending(out);
            if s.pending != 0 {
                s.last_flush = -1;
                return Z_OK;
            }
        }

        if s.avail_in != 0 || s.lookahead != 0 || (flush != Z_NO_FLUSH && !s.status_finish) {
            let bstate = if s.level == 0 {
                deflate_stored(s, t, input, out, flush)
            } else if s.level <= 3 {
                deflate_fast(s, t, input, out, flush)
            } else {
                deflate_slow(s, t, input, out, flush)
            };

            if bstate == BState::FinishStarted || bstate == BState::FinishDone {
                s.status_finish = true;
            }
            if bstate == BState::NeedMore || bstate == BState::FinishStarted {
                if s.avail_out == 0 {
                    s.last_flush = -1;
                }
                return Z_OK;
            }
            if bstate == BState::BlockDone {
                if flush == Z_PARTIAL_FLUSH {
                    s.tr_align(t);
                } else if flush != Z_NO_FLUSH && flush != Z_FINISH {
                    // Z_SYNC_FLUSH or Z_FULL_FLUSH: an empty stored block, which for a
                    // full flush is the marker `inflate_sync()` looks for.
                    s.tr_stored_block(None, 0, false);
                    if flush == Z_FULL_FLUSH {
                        s.head.fill(0); // CLEAR_HASH: forget the history
                        if s.lookahead == 0 {
                            s.strstart = 0;
                            s.block_start = 0;
                            s.insert = 0;
                        }
                    }
                }
                s.flush_pending(out);
                if s.avail_out == 0 {
                    s.last_flush = -1;
                    return Z_OK;
                }
            }
        }

        if flush != Z_FINISH {
            return Z_OK;
        }
        if s.wrap <= 0 {
            return Z_STREAM_END;
        }

        if s.wrap == 2 {
            let crc = s.check;
            let total = s.total_in as u32;
            for shift in [0, 8, 16, 24] {
                s.put_byte((crc >> shift) as u8);
            }
            for shift in [0, 8, 16, 24] {
                s.put_byte((total >> shift) as u8);
            }
        } else {
            let adler = s.check;
            for shift in [24, 16, 8, 0] {
                s.put_byte((adler >> shift) as u8);
            }
        }
        s.flush_pending(out);
        s.wrap = -s.wrap;
        if s.pending != 0 {
            Z_OK
        } else {
            Z_STREAM_END
        }
    }
}

/// Compress `data` in one shot, the way `git` does when it has the whole object
/// in memory. Returns the complete stream including header and trailer.
pub fn compress(data: &[u8], level: i32, wrap: Wrap) -> Vec<u8> {
    let mut z = Deflate::new(level, wrap);
    // deflateBound()'s worst case, plus room for the largest wrapper.
    let cap = data.len() + (data.len() >> 12) + (data.len() >> 14) + (data.len() >> 25) + 32;
    let mut out = vec![0u8; cap];
    z.set_input(data.len());
    z.set_output(cap);
    let status = z.step(data, &mut out, Z_FINISH);
    debug_assert_eq!(status, Z_STREAM_END, "deflateBound() undersized the output buffer");
    out.truncate(z.out_pos());
    out
}
fn flush_block_only(
    s: &mut State,
    t: &Tables,
    out: &mut [u8],
    last: bool,
) {
    let buf = if s.block_start >= 0 {
        Some(s.block_start as usize)
    } else {
        None
    };
    let len = (s.strstart as i64 - s.block_start) as usize;
    s.tr_flush_block(t, buf, len, last);
    s.block_start = s.strstart as i64;
    s.flush_pending(out);
}

fn deflate_fast(
    s: &mut State,
    t: &Tables,
    input: &[u8],
    out: &mut [u8],
    flush: i32,
) -> BState {
    loop {
        if s.lookahead < MIN_LOOKAHEAD {
            s.fill_window(input);
            if s.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BState::NeedMore;
            }
            if s.lookahead == 0 {
                break;
            }
        }

        let mut hash_head = 0usize;
        if s.lookahead >= MIN_MATCH {
            hash_head = s.insert_string(s.strstart);
        }

        if hash_head != 0 && s.strstart - hash_head <= MAX_DIST {
            s.match_length = s.longest_match(hash_head);
        }

        let bflush;
        if s.match_length >= MIN_MATCH {
            let dist = s.strstart - s.match_start;
            let lc = s.match_length - MIN_MATCH;
            bflush = s.tr_tally(t, dist, lc);
            s.lookahead -= s.match_length;

            if s.match_length <= s.max_lazy_match && s.lookahead >= MIN_MATCH {
                s.match_length -= 1;
                loop {
                    s.strstart += 1;
                    s.insert_string(s.strstart);
                    s.match_length -= 1;
                    if s.match_length == 0 {
                        break;
                    }
                }
                s.strstart += 1;
            } else {
                s.strstart += s.match_length;
                s.match_length = 0;
                s.ins_h = usize::from(s.window[s.strstart]);
                let c = s.window[s.strstart + 1];
                s.update_hash(c);
            }
        } else {
            let lit = usize::from(s.window[s.strstart]);
            bflush = s.tr_tally(t, 0, lit);
            s.lookahead -= 1;
            s.strstart += 1;
        }
        if bflush {
            flush_block_only(s, t, out, false);
            if s.avail_out == 0 {
                return BState::NeedMore;
            }
        }
    }
    s.insert = if s.strstart < MIN_MATCH - 1 {
        s.strstart
    } else {
        MIN_MATCH - 1
    };
    if flush == Z_FINISH {
        flush_block_only(s, t, out, true);
        if s.avail_out == 0 {
            return BState::FinishStarted;
        }
        return BState::FinishDone;
    }
    if s.sym_next != 0 {
        flush_block_only(s, t, out, false);
        if s.avail_out == 0 {
            return BState::NeedMore;
        }
    }
    BState::BlockDone
}

fn deflate_slow(
    s: &mut State,
    t: &Tables,
    input: &[u8],
    out: &mut [u8],
    flush: i32,
) -> BState {
    loop {
        if s.lookahead < MIN_LOOKAHEAD {
            s.fill_window(input);
            if s.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BState::NeedMore;
            }
            if s.lookahead == 0 {
                break;
            }
        }

        let mut hash_head = 0usize;
        if s.lookahead >= MIN_MATCH {
            hash_head = s.insert_string(s.strstart);
        }

        s.prev_length = s.match_length;
        s.prev_match = s.match_start;
        s.match_length = MIN_MATCH - 1;

        if hash_head != 0 && s.prev_length < s.max_lazy_match && s.strstart - hash_head <= MAX_DIST
        {
            s.match_length = s.longest_match(hash_head);
            if s.match_length <= 5
                && s.match_length == MIN_MATCH
                && s.strstart - s.match_start > TOO_FAR
            {
                s.match_length = MIN_MATCH - 1;
            }
        }

        if s.prev_length >= MIN_MATCH && s.match_length <= s.prev_length {
            let max_insert = s.strstart + s.lookahead - MIN_MATCH;
            let dist = s.strstart - 1 - s.prev_match;
            let lc = s.prev_length - MIN_MATCH;
            let bflush = s.tr_tally(t, dist, lc);

            s.lookahead -= s.prev_length - 1;
            s.prev_length -= 2;
            loop {
                s.strstart += 1;
                if s.strstart <= max_insert {
                    s.insert_string(s.strstart);
                }
                s.prev_length -= 1;
                if s.prev_length == 0 {
                    break;
                }
            }
            s.match_available = false;
            s.match_length = MIN_MATCH - 1;
            s.strstart += 1;

            if bflush {
                flush_block_only(s, t, out, false);
                if s.avail_out == 0 {
                    return BState::NeedMore;
                }
            }
        } else if s.match_available {
            let lit = usize::from(s.window[s.strstart - 1]);
            let bflush = s.tr_tally(t, 0, lit);
            if bflush {
                flush_block_only(s, t, out, false);
            }
            s.strstart += 1;
            s.lookahead -= 1;
            if s.avail_out == 0 {
                return BState::NeedMore;
            }
        } else {
            s.match_available = true;
            s.strstart += 1;
            s.lookahead -= 1;
        }
    }

    if s.match_available {
        let lit = usize::from(s.window[s.strstart - 1]);
        s.tr_tally(t, 0, lit);
        s.match_available = false;
    }
    s.insert = if s.strstart < MIN_MATCH - 1 {
        s.strstart
    } else {
        MIN_MATCH - 1
    };
    if flush == Z_FINISH {
        flush_block_only(s, t, out, true);
        if s.avail_out == 0 {
            return BState::FinishStarted;
        }
        return BState::FinishDone;
    }
    if s.sym_next != 0 {
        flush_block_only(s, t, out, false);
        if s.avail_out == 0 {
            return BState::NeedMore;
        }
    }
    BState::BlockDone
}

fn deflate_stored(
    s: &mut State,
    _t: &Tables,
    input: &[u8],
    out: &mut [u8],
    flush: i32,
) -> BState {
    let mut min_block = (PENDING_BUF_SIZE - 5).min(W_SIZE);

    let mut last = false;
    let used = s.avail_in;
    loop {
        let mut len = MAX_STORED;
        let mut have = ((s.bi_valid + 42) >> 3) as usize;
        if s.avail_out < have {
            break;
        }
        have = s.avail_out - have;
        let mut left = (s.strstart as i64 - s.block_start) as usize;
        if len > left + s.avail_in {
            len = left + s.avail_in;
        }
        if len > have {
            len = have;
        }

        if len < min_block
            && ((len == 0 && flush != Z_FINISH)
                || flush == Z_NO_FLUSH
                || len != left + s.avail_in)
        {
            break;
        }

        last = flush == Z_FINISH && len == left + s.avail_in;
        s.tr_stored_block(None, 0, last);

        s.pending_buf[s.pending - 4] = len as u8;
        s.pending_buf[s.pending - 3] = (len >> 8) as u8;
        s.pending_buf[s.pending - 2] = !(len as u8);
        s.pending_buf[s.pending - 1] = !((len >> 8) as u8);

        s.flush_pending(out);

        if left != 0 {
            if left > len {
                left = len;
            }
            let from = s.block_start as usize;
            out[s.next_out..s.next_out + left].copy_from_slice(&s.window[from..from + left]);
            s.next_out += left;
            s.avail_out -= left;
            s.total_out += left as u64;
            s.block_start += left as i64;
            len -= left;
        }
        if len != 0 {
            let n = s.read_buf_into_out(input, out, len);
            s.next_out += n;
            s.avail_out -= n;
            s.total_out += n as u64;
        }
        if last {
            break;
        }
    }

    let used = used - s.avail_in;
    if used != 0 {
        if used >= W_SIZE {
            s.matches = 2;
            let start = s.next_in - W_SIZE;
            s.window[..W_SIZE].copy_from_slice(&input[start..start + W_SIZE]);
            s.strstart = W_SIZE;
            s.insert = s.strstart;
        } else {
            if WINDOW_SIZE - s.strstart <= used {
                s.strstart -= W_SIZE;
                s.window.copy_within(W_SIZE..W_SIZE + s.strstart, 0);
                if s.matches < 2 {
                    s.matches += 1;
                }
                if s.insert > s.strstart {
                    s.insert = s.strstart;
                }
            }
            let start = s.next_in - used;
            let at = s.strstart;
            s.window[at..at + used].copy_from_slice(&input[start..start + used]);
            s.strstart += used;
            s.insert += used.min(W_SIZE - s.insert);
        }
        s.block_start = s.strstart as i64;
    }
    if s.high_water < s.strstart {
        s.high_water = s.strstart;
    }

    if last {
        return BState::FinishDone;
    }
    if flush != Z_NO_FLUSH && flush != Z_FINISH && s.avail_in == 0 && s.strstart as i64 == s.block_start
    {
        return BState::BlockDone;
    }

    let mut have = WINDOW_SIZE - s.strstart;
    if s.avail_in > have && s.block_start >= W_SIZE as i64 {
        s.block_start -= W_SIZE as i64;
        s.strstart -= W_SIZE;
        s.window.copy_within(W_SIZE..W_SIZE + s.strstart, 0);
        if s.matches < 2 {
            s.matches += 1;
        }
        have += W_SIZE;
        if s.insert > s.strstart {
            s.insert = s.strstart;
        }
    }
    if have > s.avail_in {
        have = s.avail_in;
    }
    if have != 0 {
        let at = s.strstart;
        let n = s.read_buf_into_window(input, at, have);
        s.strstart += n;
        s.insert += n.min(W_SIZE - s.insert);
    }
    if s.high_water < s.strstart {
        s.high_water = s.strstart;
    }

    have = ((s.bi_valid + 42) >> 3) as usize;
    have = (PENDING_BUF_SIZE - have).min(MAX_STORED);
    min_block = have.min(W_SIZE);
    let left = (s.strstart as i64 - s.block_start) as usize;
    if left >= min_block
        || ((left != 0 || flush == Z_FINISH)
            && flush != Z_NO_FLUSH
            && s.avail_in == 0
            && left <= have)
    {
        let len = left.min(have);
        let last2 = flush == Z_FINISH && s.avail_in == 0 && len == left;
        let from = s.block_start as usize;
        s.tr_stored_block(Some(from), len, last2);
        s.block_start += len as i64;
        s.flush_pending(out);
        if last2 {
            return BState::FinishStarted;
        }
    }

    BState::NeedMore
}
