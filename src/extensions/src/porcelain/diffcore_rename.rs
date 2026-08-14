//! The diffcore rename/copy/break passes, ported from git 2.55.0.
//!
//! Three C files are reproduced here, function for function:
//!
//! * `diffcore-delta.c` — the *spanhash* similarity estimator. Content is cut into
//!   chunks delimited by `LF` or 64 bytes, whichever comes first; each chunk is hashed
//!   into one of `HASHBASE` buckets and the occurrences are counted. Comparing the two
//!   count tables yields "how many bytes of the destination were copied from the
//!   source" (`src_copied`) and "how many were added outright" (`literal_added`).
//!   This is the *only* thing that determines the `similarity index <n>%` git prints,
//!   so it is reproduced byte-for-byte rather than approximated.
//! * `diffcore-rename.c` — `diffcore_rename()`: exact-match detection, basename-hinted
//!   detection, then the `NUM_CANDIDATE_PER_DST`-wide inexact similarity matrix.
//! * `diffcore-break.c` — `diffcore_break()` / `diffcore_merge_broken()`, the `-B`
//!   rewrite splitter and the pass that glues an unmatched split back together.
//!
//! ### Deliberate omissions, and why they cannot change the output
//!
//! `diffcore_rename_extended()` takes four extra arguments (`relevant_sources`,
//! `dirs_removed`, `dir_rename_count`, `cached_pairs`) that exist purely for the
//! merge-ort caller. The `diffcore_rename()` entry point — the one `diffcore_std()`
//! uses, and the only one a diff command reaches — passes `NULL` for all four. With
//! all four `NULL`, `initialize_dir_rename_info()` sets `info->setup = 0` and returns
//! immediately, which makes `update_dir_rename_counts()`, `idx_possible_rename()` and
//! `handle_early_known_dir_renames()` no-ops for the whole run. Those functions are
//! therefore not ported; every code path that survives is.
//!
//! Likewise, the promisor-remote prefetch callbacks (`inexact_prefetch`,
//! `basename_prefetch`) only batch object fetches for a partial clone and never affect
//! which pairs are produced, and the `mem_pool` plumbing is C memory management.
//!
//! ### Arithmetic fidelity
//!
//! `MAX_SCORE` is `60000.0` — a *double* — in `diffcore.h`, so several of git's score
//! computations are floating point with a truncating cast at the end, not integer
//! division. Those are reproduced with `f64` here (see [`estimate_similarity`],
//! [`should_break`], [`similarity_index`]); using integer division instead would
//! disagree with stock git on the boundary cases where the exact quotient falls just
//! under an integer.

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// diffcore.h constants
// ---------------------------------------------------------------------------

/// `diffcore.h`: `#define MAX_SCORE 60000.0`. A perfect match scores this much.
pub const MAX_SCORE: f64 = 60000.0;
/// `diffcore.h`: rename/copy similarity minimum (50%).
pub const DEFAULT_RENAME_SCORE: u32 = 30000;
/// `diffcore.h`: minimum edit for a `-B` break to happen (50%).
pub const DEFAULT_BREAK_SCORE: u32 = 30000;
/// `diffcore.h`: maximum dissimilarity for a broken pair to be merged back (60%).
pub const DEFAULT_MERGE_SCORE: u32 = 36000;
/// `diffcore.h`: `-B` never breaks a filepair whose larger side is smaller than this.
pub const MINIMUM_BREAK_SIZE: u64 = 400;

/// `diff.h`: `#define DIFF_DETECT_RENAME 1`.
pub const DETECT_RENAME: u8 = 1;
/// `diff.h`: `#define DIFF_DETECT_COPY 2`.
pub const DETECT_COPY: u8 = 2;

/// `diff.c`: `diff_rename_limit_default`, the `diff.renameLimit` fallback.
pub const DEFAULT_RENAME_LIMIT: i64 = 1000;

/// `diffcore-rename.c`: how many candidate sources are kept per destination.
const NUM_CANDIDATE_PER_DST: usize = 4;

/// `diff.c`'s `similarity_index()`: `p->score * 100 / MAX_SCORE`, evaluated in
/// `double` (because `MAX_SCORE` is `60000.0`) and truncated by the cast to `int`.
pub fn similarity_index(score: u32) -> u32 {
    ((score as f64 * 100.0) / MAX_SCORE) as u32
}

/// `diff.c`'s `git_config_rename()`: the `diff.renames` value.  A missing value is
/// `DIFF_DETECT_RENAME`; `copies`/`copy` (case-blind) is `DIFF_DETECT_COPY`; anything
/// else goes through `git_config_bool()`.
pub fn config_rename(value: Option<&gix::bstr::BStr>) -> u8 {
    let Some(v) = value else {
        return DETECT_RENAME;
    };
    let s = v.to_str_lossy();
    if s.eq_ignore_ascii_case("copies") || s.eq_ignore_ascii_case("copy") {
        return DETECT_COPY;
    }
    if parse_bool(&s) == Some(true) {
        DETECT_RENAME
    } else {
        0
    }
}

/// `git_config_bool()` for the handful of spellings git's `git_parse_maybe_bool()`
/// accepts. An unparseable value is a config error in git; here it reads as false,
/// which is what `git_config_bool()` returns for `"0"`/`""`/`"false"` anyway.
fn parse_bool(s: &str) -> Option<bool> {
    if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("yes") || s.eq_ignore_ascii_case("on") {
        return Some(true);
    }
    if s.eq_ignore_ascii_case("false")
        || s.eq_ignore_ascii_case("no")
        || s.eq_ignore_ascii_case("off")
        || s.is_empty()
    {
        return Some(false);
    }
    s.parse::<i64>().ok().map(|n| n != 0)
}

/// `diff.c`'s `parse_rename_score()`: read `<n>`, `<n>%`, or `<n>.<m>` from the front
/// of `arg`, returning the score in `MAX_SCORE` units and the unconsumed remainder.
/// `-M50` and `-M50%` and `-M.5` all mean the same thing.
pub fn parse_rename_score(arg: &str) -> (u32, &str) {
    let bytes = arg.as_bytes();
    let mut num: u64 = 0;
    let mut scale: u64 = 1;
    let mut dot = false;
    let mut i = 0usize;
    loop {
        let ch = bytes.get(i).copied().unwrap_or(0);
        if !dot && ch == b'.' {
            scale = 1;
            dot = true;
        } else if ch == b'%' {
            scale = if dot { scale * 100 } else { 100 };
            i += 1; // '%' is always at the end
            break;
        } else if ch.is_ascii_digit() {
            if scale < 100_000 {
                scale *= 10;
                num = num * 10 + u64::from(ch - b'0');
            }
        } else {
            break;
        }
        i += 1;
    }
    // "user says num divided by scale and we say internally that is
    //  MAX_SCORE * num / scale."
    let score = if num >= scale {
        MAX_SCORE as u32
    } else {
        ((MAX_SCORE * num as f64) / scale as f64) as u32
    };
    (score, &arg[i..])
}

/// `diff.c`'s `diff_opt_break_rewrites()`: parse `-B[<n>][/<m>]` into git's packed
/// `break_opt` (`n | (m << 16)`). `Err` reproduces `"%s expects <n>/<m> form"`.
pub fn parse_break_opt(arg: &str) -> Result<i64, ()> {
    let (opt1, rest) = parse_rename_score(arg);
    let (opt2, rest) = if rest.is_empty() {
        (0u32, rest)
    } else if !rest.starts_with('/') {
        return Err(());
    } else {
        parse_rename_score(&rest[1..])
    };
    if !rest.is_empty() {
        return Err(());
    }
    Ok(i64::from(opt1) | (i64::from(opt2) << 16))
}

// ---------------------------------------------------------------------------
// the queue: an arena of filespecs plus the pairs that reference them
// ---------------------------------------------------------------------------

/// One side of a file pair — git's `struct diff_filespec`.
///
/// A `mode` of zero is git's `!DIFF_FILE_VALID`: the path does not exist on this side.
/// Specs live in [`Queue::specs`] and are referenced by index, because
/// `record_rename_pair()` makes a destination pair *share* the source pair's `one`
/// spec (`dst->one = src->one`) and then relies on the shared `rename_used` counter.
#[derive(Clone)]
pub struct FileSpec {
    pub path: BString,
    /// The full mode (`0100644`, `0120000`, `0160000`, …); `0` means "not present".
    pub mode: u32,
    pub oid: ObjectId,
    /// git's `oid_valid`: `false` for a worktree side whose blob was never hashed.
    pub oid_valid: bool,
    /// git's `dirty_submodule`: the `DIRTY_SUBMODULE_*` bits of a gitlink whose
    /// worktree holds more than the commit it records. `diff_unmodified_pair()`
    /// (diff.c:6528) keeps a pair alive for them even when both ids match.
    pub dirty_submodule: u8,
    /// git's `rename_used`, the count of pairs that consume this spec as a source.
    pub rename_used: u32,
    /// Cached blob bytes (`diff_populate_filespec` with `check_size_only = 0`).
    data: Option<Vec<u8>>,
    /// Cached size (`diff_populate_filespec` with `check_size_only = 1`).
    size: Option<u64>,
    /// Cached spanhash table (git's `cnt_data`).
    cnt_data: Option<SpanHash>,
    /// Whether the caller could not produce the content at all — git's
    /// `diff_populate_filespec()` returning non-zero.
    unreadable: bool,
}

impl FileSpec {
    /// An absent side: git's `alloc_filespec()` without a following `fill_filespec()`.
    pub fn absent(path: BString) -> Self {
        FileSpec {
            path,
            mode: 0,
            oid: ObjectId::null(gix::hash::Kind::Sha1),
            oid_valid: false,
            dirty_submodule: 0,
            rename_used: 0,
            data: None,
            size: None,
            cnt_data: None,
            unreadable: false,
        }
    }

    /// A present side.
    pub fn new(path: BString, mode: u32, oid: ObjectId, oid_valid: bool) -> Self {
        FileSpec {
            path,
            mode,
            oid,
            oid_valid,
            dirty_submodule: 0,
            rename_used: 0,
            data: None,
            size: None,
            cnt_data: None,
            unreadable: false,
        }
    }

    /// git's `DIFF_FILE_VALID()`.
    pub fn valid(&self) -> bool {
        self.mode != 0
    }

    /// git's `S_ISREG(mode)`: a regular file (not a symlink, not a gitlink).
    fn is_reg(&self) -> bool {
        self.mode & 0o170000 == 0o100000
    }
}

/// One entry of git's `diff_queued_diff`, i.e. `struct diff_filepair`.
#[derive(Clone)]
pub struct Pair {
    /// Index into [`Queue::specs`] for the pre-image.
    pub one: usize,
    /// Index into [`Queue::specs`] for the post-image.
    pub two: usize,
    /// Similarity (rename/copy) or dissimilarity (broken pair) in `MAX_SCORE` units.
    pub score: u32,
    /// git's `renamed_pair`: this destination was matched to a different source path.
    pub renamed_pair: bool,
    /// git's `broken_pair`: produced by `diffcore_break()`.
    pub broken_pair: bool,
    /// The status letter assigned by `diff_resolve_rename_copy()`.
    pub status: u8,
}

/// The diff queue: an arena of filespecs plus the pair list that indexes into it.
pub struct Queue {
    pub specs: Vec<FileSpec>,
    pub pairs: Vec<Pair>,
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            specs: Vec::new(),
            pairs: Vec::new(),
        }
    }
}

impl Queue {
    /// Push a filespec into the arena and return its index.
    pub fn add_spec(&mut self, spec: FileSpec) -> usize {
        self.specs.push(spec);
        self.specs.len() - 1
    }

    /// git's `diff_queue()`: append a pair built from two arena indices.
    pub fn add_pair(&mut self, one: usize, two: usize) -> usize {
        self.pairs.push(Pair {
            one,
            two,
            score: 0,
            renamed_pair: false,
            broken_pair: false,
            status: 0,
        });
        self.pairs.len() - 1
    }

    fn one(&self, p: usize) -> &FileSpec {
        &self.specs[self.pairs[p].one]
    }

    fn two(&self, p: usize) -> &FileSpec {
        &self.specs[self.pairs[p].two]
    }

    /// git's `diff_unmodified_pair()`. Deletion, addition, mode/type change *and
    /// rename* are all interesting; only a same-path, same-mode, same-content pair is
    /// uninteresting. Two sides that both lack an object id "look at the same file on
    /// the filesystem" and count as unmodified too.
    fn unmodified_pair(&self, p: usize) -> bool {
        if self.unmerged(p) {
            return false; // unmerged is interesting
        }
        let (one, two) = (self.one(p), self.two(p));
        if one.valid() != two.valid() || one.mode != two.mode || one.path != two.path {
            return false;
        }
        if one.oid_valid
            && two.oid_valid
            && one.oid == two.oid
            && one.dirty_submodule == 0
            && two.dirty_submodule == 0
        {
            return true; // no change
        }
        !one.oid_valid && !two.oid_valid
    }

    /// git's `DIFF_PAIR_UNMERGED()`: neither side is valid.
    fn unmerged(&self, p: usize) -> bool {
        !self.one(p).valid() && !self.two(p).valid()
    }

    /// git's `DIFF_PAIR_TYPE_CHANGED()`: both sides valid but of different `S_IFMT`.
    fn type_changed(&self, p: usize) -> bool {
        let (one, two) = (self.one(p), self.two(p));
        one.valid() && two.valid() && (one.mode & 0o170000) != (two.mode & 0o170000)
    }
}

// ---------------------------------------------------------------------------
// content access
// ---------------------------------------------------------------------------

/// How the driver reads a filespec's content. This is git's
/// `diff_populate_filespec()`, split into its two modes: `check_size_only = 1`
/// (size alone, which is all `estimate_similarity()` needs for its cheap early-out)
/// and `check_size_only = 0` (the whole blob).
pub trait Content {
    /// The blob's size, or `None` when it cannot be read (git returns non-zero and the
    /// caller treats the pair as unmatchable).
    fn size(&mut self, spec: &FileSpec) -> Option<u64>;
    /// The blob's bytes.
    fn data(&mut self, spec: &FileSpec) -> Option<Vec<u8>>;
}

/// Populate `spec.size`, mirroring `diff_populate_filespec(..., check_size_only=1)`.
/// Returns `false` on the error path git takes when the object cannot be read.
fn populate_size(specs: &mut [FileSpec], idx: usize, c: &mut dyn Content) -> bool {
    if specs[idx].unreadable {
        return false;
    }
    if specs[idx].size.is_some() || specs[idx].data.is_some() {
        return true;
    }
    let probed = c.size(&specs[idx]);
    match probed {
        Some(n) => {
            specs[idx].size = Some(n);
            true
        }
        None => {
            specs[idx].unreadable = true;
            false
        }
    }
}

/// Populate `spec.data`, mirroring `diff_populate_filespec(..., check_size_only=0)`.
fn populate_data(specs: &mut [FileSpec], idx: usize, c: &mut dyn Content) -> bool {
    if specs[idx].unreadable {
        return false;
    }
    if specs[idx].data.is_some() {
        return true;
    }
    let probed = c.data(&specs[idx]);
    match probed {
        Some(buf) => {
            specs[idx].size = Some(buf.len() as u64);
            specs[idx].data = Some(buf);
            true
        }
        None => {
            specs[idx].unreadable = true;
            false
        }
    }
}

/// git's `hash_filespec()`: a worktree side has no object id of its own, so the blob
/// is read and hashed before exact-rename matching can look it up.
fn hash_filespec(specs: &mut [FileSpec], idx: usize, kind: gix::hash::Kind, c: &mut dyn Content) {
    if specs[idx].oid_valid || !specs[idx].valid() {
        return;
    }
    if !populate_data(specs, idx, c) {
        return;
    }
    let data = specs[idx].data.as_deref().unwrap_or_default();
    if let Ok(id) = gix::objs::compute_hash(kind, gix::objs::Kind::Blob, data) {
        specs[idx].oid = id;
        specs[idx].oid_valid = true;
    }
}

/// The size of an already-populated spec.
fn spec_size(spec: &FileSpec) -> u64 {
    spec.size
        .or_else(|| spec.data.as_ref().map(|d| d.len() as u64))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// diffcore-delta.c — the spanhash similarity estimator
// ---------------------------------------------------------------------------

/// `diffcore-delta.c`: wild guess at the initial hash size (`1 << 9` buckets).
const INITIAL_HASH_SIZE: i32 = 9;

/// `diffcore-delta.c`: a prime carefully chosen between 2^16 and 2^17.
const HASHBASE: u32 = 107_927;

/// `diffcore-delta.c`: `INITIAL_FREE(sz_log2)` — leave more room in a smaller hash but
/// do not let it grow to have too much unused hole.
fn initial_free(sz_log2: i32) -> i32 {
    ((1i64 << sz_log2) * i64::from(sz_log2 - 3) / i64::from(sz_log2)) as i32
}

/// One bucket of git's `struct spanhash`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Span {
    hashval: u32,
    cnt: u32,
}

/// git's `struct spanhash_top`: an open-addressed table of chunk hashes and counts.
#[derive(Clone)]
struct SpanHash {
    alloc_log2: i32,
    free: i32,
    data: Vec<Span>,
}

impl SpanHash {
    fn with_log2(alloc_log2: i32) -> Self {
        SpanHash {
            alloc_log2,
            free: initial_free(alloc_log2),
            data: vec![Span::default(); 1usize << alloc_log2],
        }
    }

    /// The bucket at `i`, or a zero-count sentinel past the end. git walks off the end
    /// of the sorted array relying on the trailing zero-count entries; the table is
    /// never full (`INITIAL_FREE(n) + 1 < 2^n`), so at least one such entry exists.
    fn at(&self, i: usize) -> Span {
        self.data.get(i).copied().unwrap_or_default()
    }
}

/// git's `spanhash_rehash()`: double the table and reinsert every non-empty bucket.
fn spanhash_rehash(orig: &SpanHash) -> SpanHash {
    let osz = 1usize << orig.alloc_log2;
    let sz = osz << 1;
    let mut new = SpanHash::with_log2(orig.alloc_log2 + 1);
    for i in 0..osz {
        let o = orig.data[i];
        if o.cnt == 0 {
            continue;
        }
        let mut bucket = (o.hashval as usize) & (sz - 1);
        loop {
            let h = &mut new.data[bucket];
            bucket += 1;
            if h.cnt == 0 {
                h.hashval = o.hashval;
                h.cnt = o.cnt;
                new.free -= 1;
                break;
            }
            if sz <= bucket {
                bucket = 0;
            }
        }
    }
    new
}

/// git's `add_spanhash()`: accumulate `cnt` occurrences of `hashval`, growing the
/// table when the free budget is exhausted.
fn add_spanhash(top: SpanHash, hashval: u32, cnt: u32) -> SpanHash {
    let mut top = top;
    let lim = 1usize << top.alloc_log2;
    let mut bucket = (hashval as usize) & (lim - 1);
    loop {
        let h = &mut top.data[bucket];
        bucket += 1;
        if h.cnt == 0 {
            h.hashval = hashval;
            h.cnt = cnt;
            top.free -= 1;
            if top.free < 0 {
                return spanhash_rehash(&top);
            }
            return top;
        }
        if h.hashval == hashval {
            h.cnt += cnt;
            return top;
        }
        if lim <= bucket {
            bucket = 0;
        }
    }
}

/// git's `spanhash_cmp()`: ascending by `hashval`, with zero-count buckets last.
fn spanhash_cmp(a: &Span, b: &Span) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if a.cnt == 0 {
        return if b.cnt == 0 { Equal } else { Greater };
    }
    if b.cnt == 0 {
        return Less;
    }
    a.hashval.cmp(&b.hashval)
}

/// git's `buffer_is_binary()` (`xdiff-interface.c`): a NUL byte within the first
/// `FIRST_FEW_BYTES` (8000) marks the buffer binary.
pub fn buffer_is_binary(buf: &[u8]) -> bool {
    let n = buf.len().min(8000);
    buf[..n].contains(&0)
}

/// git's `hash_chars()`: cut the buffer into chunks delimited by `LF` or 64 bytes,
/// whichever comes first, hash each chunk and count its length into the table.
fn hash_chars(buf: &[u8]) -> SpanHash {
    let is_text = !buffer_is_binary(buf);
    let mut hash = SpanHash::with_log2(INITIAL_HASH_SIZE);

    let mut n: u32 = 0;
    let (mut accum1, mut accum2): (u32, u32) = (0, 0);
    let mut i = 0usize;
    while i < buf.len() {
        let c = u32::from(buf[i]);
        let old_1 = accum1;
        i += 1;
        let sz_left = buf.len() - i;

        // Ignore CR in a CRLF sequence if the content is text.
        if is_text && c == u32::from(b'\r') && sz_left > 0 && buf[i] == b'\n' {
            continue;
        }

        accum1 = (accum1 << 7) ^ (accum2 >> 25);
        accum2 = (accum2 << 7) ^ (old_1 >> 25);
        accum1 = accum1.wrapping_add(c);
        n += 1;
        if n < 64 && c != u32::from(b'\n') {
            continue;
        }
        let hashval = (accum1.wrapping_add(accum2.wrapping_mul(0x61))) % HASHBASE;
        hash = add_spanhash(hash, hashval, n);
        n = 0;
        accum1 = 0;
        accum2 = 0;
    }
    if n > 0 {
        let hashval = (accum1.wrapping_add(accum2.wrapping_mul(0x61))) % HASHBASE;
        hash = add_spanhash(hash, hashval, n);
    }
    hash.data.sort_by(spanhash_cmp);
    hash
}

/// git's `diffcore_count_changes()`: walk the two sorted tables in lockstep and
/// accumulate how many bytes of the destination came from the source (`src_copied`)
/// and how many were added outright (`literal_added`).
fn count_changes(src_count: &SpanHash, dst_count: &SpanHash) -> (u64, u64) {
    let mut sc: u64 = 0;
    let mut la: u64 = 0;
    let mut s = 0usize;
    let mut d = 0usize;
    loop {
        let sp = src_count.at(s);
        if sp.cnt == 0 {
            break; // we checked all in src
        }
        loop {
            let dp = dst_count.at(d);
            if dp.cnt == 0 || dp.hashval >= sp.hashval {
                break;
            }
            la += u64::from(dp.cnt);
            d += 1;
        }
        let src_cnt = sp.cnt;
        let mut dst_cnt = 0u32;
        let dp = dst_count.at(d);
        if dp.cnt != 0 && dp.hashval == sp.hashval {
            dst_cnt = dp.cnt;
            d += 1;
        }
        if src_cnt < dst_cnt {
            la += u64::from(dst_cnt - src_cnt);
            sc += u64::from(src_cnt);
        } else {
            sc += u64::from(dst_cnt);
        }
        s += 1;
    }
    loop {
        let dp = dst_count.at(d);
        if dp.cnt == 0 {
            break;
        }
        la += u64::from(dp.cnt);
        d += 1;
    }
    (sc, la)
}

/// Compute (and cache) both spanhash tables, then run [`count_changes`].
fn counted_changes(specs: &mut [FileSpec], src: usize, dst: usize) -> (u64, u64) {
    if specs[src].cnt_data.is_none() {
        let h = hash_chars(specs[src].data.as_deref().unwrap_or_default());
        specs[src].cnt_data = Some(h);
    }
    if specs[dst].cnt_data.is_none() {
        let h = hash_chars(specs[dst].data.as_deref().unwrap_or_default());
        specs[dst].cnt_data = Some(h);
    }
    let (a, b) = if src < dst {
        let (lo, hi) = specs.split_at(dst);
        (lo[src].cnt_data.as_ref().unwrap(), hi[0].cnt_data.as_ref().unwrap())
    } else {
        let (lo, hi) = specs.split_at(src);
        (hi[0].cnt_data.as_ref().unwrap(), lo[dst].cnt_data.as_ref().unwrap())
    };
    count_changes(a, b)
}

// ---------------------------------------------------------------------------
// diffcore-rename.c
// ---------------------------------------------------------------------------

/// git's `estimate_similarity()`: what percentage (in `MAX_SCORE` units) of the
/// destination's material came from the source.
fn estimate_similarity(
    specs: &mut Vec<FileSpec>,
    src: usize,
    dst: usize,
    minimum_score: u32,
    c: &mut dyn Content,
) -> u32 {
    // We deal only with regular files. Symlink renames are handled only when they are
    // exact matches --- in other words, no edits after renaming.
    if !specs[src].is_reg() || !specs[dst].is_reg() {
        return 0;
    }

    // check_size_only = 1: sizes are enough for the cheap early-out below.
    if specs[src].cnt_data.is_none() && !populate_size(specs, src, c) {
        return 0;
    }
    if specs[dst].cnt_data.is_none() && !populate_size(specs, dst, c) {
        return 0;
    }

    let src_size = spec_size(&specs[src]);
    let dst_size = spec_size(&specs[dst]);
    let max_size = src_size.max(dst_size);
    let base_size = src_size.min(dst_size);
    let delta_size = max_size - base_size;

    // We would not consider edits that change the file size so drastically:
    // delta_size must be smaller than (MAX_SCORE-minimum_score)/MAX_SCORE * base_size.
    // Evaluated in `double`, exactly as in C, because MAX_SCORE is 60000.0.
    if (max_size as f64) * (MAX_SCORE - f64::from(minimum_score)) < (delta_size as f64) * MAX_SCORE {
        return 0;
    }

    // check_size_only = 0: now we really need the bytes.
    if specs[src].cnt_data.is_none() && !populate_data(specs, src, c) {
        return 0;
    }
    if specs[dst].cnt_data.is_none() && !populate_data(specs, dst, c) {
        return 0;
    }

    let (src_copied, _literal_added) = counted_changes(specs, src, dst);

    // How similar are they? What percentage of material in dst is from source?
    if dst_size == 0 {
        0 // should not happen
    } else {
        ((src_copied as f64) * MAX_SCORE / (max_size as f64)) as u32
    }
}

/// git's `basename_same()`: do the two paths end in the same final component?
fn basename_same(a: &[u8], b: &[u8]) -> bool {
    let (mut src_len, mut dst_len) = (a.len(), b.len());
    while src_len != 0 && dst_len != 0 {
        src_len -= 1;
        dst_len -= 1;
        let c1 = a[src_len];
        let c2 = b[dst_len];
        if c1 != c2 {
            return false;
        }
        if c1 == b'/' {
            return true;
        }
    }
    (src_len == 0 || a[src_len - 1] == b'/') && (dst_len == 0 || b[dst_len - 1] == b'/')
}

/// git's `get_basename()`: everything after the last `/`.
fn get_basename(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// git's `struct diff_score`: one cell of the rename similarity matrix.
#[derive(Clone, Copy)]
struct Score {
    src: usize,
    dst: isize,
    score: u32,
    name_score: i16,
}

impl Default for Score {
    fn default() -> Self {
        Score {
            src: 0,
            dst: -1,
            score: 0,
            name_score: 0,
        }
    }
}

/// git's `score_compare()`: descending by score, ties broken by descending
/// `name_score`, with unused cells (`dst < 0`) sunk to the bottom.
fn score_compare(a: &Score, b: &Score) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if a.dst < 0 {
        return if b.dst >= 0 { Greater } else { Equal };
    } else if b.dst < 0 {
        return Less;
    }
    if a.score == b.score {
        return b.name_score.cmp(&a.name_score);
    }
    b.score.cmp(&a.score)
}

/// git's `record_if_better()`: keep the best `NUM_CANDIDATE_PER_DST` candidates.
fn record_if_better(m: &mut [Score], o: &Score) {
    let mut worst = 0usize;
    for i in 1..NUM_CANDIDATE_PER_DST {
        if score_compare(&m[i], &m[worst]) == std::cmp::Ordering::Greater {
            worst = i;
        }
    }
    if score_compare(&m[worst], o) == std::cmp::Ordering::Greater {
        m[worst] = *o;
    }
}

/// One row of git's `rename_dst` table.
struct RenameDst {
    /// Index into [`Queue::pairs`].
    pair: usize,
    /// Whether this destination has been claimed by a rename or copy.
    is_rename: bool,
}

/// One row of git's `rename_src` table.
#[derive(Clone, Copy)]
struct RenameSrc {
    /// Index into [`Queue::pairs`].
    pair: usize,
    /// The break score remembered from `diffcore_break()`.
    score: u32,
}

/// Everything `diffcore_rename()` needs from `struct diff_options`.
#[derive(Clone, Copy)]
pub struct Options {
    /// `0`, [`DETECT_RENAME`] or [`DETECT_COPY`].
    pub detect_rename: u8,
    /// `-M<n>`/`-C<n>` in `MAX_SCORE` units; `0` means [`DEFAULT_RENAME_SCORE`].
    pub rename_score: u32,
    /// `-l<n>` / `diff.renameLimit`; `<= 0` is unlimited.
    pub rename_limit: i64,
    /// `--find-copies-harder`.
    pub find_copies_harder: bool,
    /// `--rename-empty` (git's default) / `--no-rename-empty`.
    pub rename_empty: bool,
    /// `-B<n>[/<m>]` packed as `n | (m << 16)`; `-1` means break detection is off.
    pub break_opt: i64,
    /// The repository's object hash, used to give a worktree side an object id before
    /// exact-rename matching (git's `hash_filespec()`).
    pub hash_kind: gix::hash::Kind,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            detect_rename: 0,
            rename_score: 0,
            rename_limit: -1,
            find_copies_harder: false,
            rename_empty: true,
            break_opt: -1,
            hash_kind: gix::hash::Kind::Sha1,
        }
    }
}

/// What `diff_warn_rename_limit()` needs after the run.
#[derive(Default, Clone, Copy)]
pub struct Warnings {
    /// git's `needed_rename_limit`: the `-l` value that would have sufficed.
    pub needed_rename_limit: usize,
    /// git's `degraded_cc_to_c`: `-C -C` was silently downgraded to `-C`.
    pub degraded_cc_to_c: bool,
}

impl Warnings {
    /// git's `diff_warn_rename_limit()`, rendered onto stderr.
    pub fn emit(&self, varname: &str) {
        if self.degraded_cc_to_c {
            eprintln!("warning: only found copies from modified paths due to too many files.");
        } else if self.needed_rename_limit != 0 {
            eprintln!("warning: exhaustive rename detection was skipped due to too many files.");
        } else {
            return;
        }
        if self.needed_rename_limit > 0 {
            eprintln!(
                "warning: you may want to set your {} variable to at least {} and retry the command.",
                varname, self.needed_rename_limit
            );
        }
    }
}

/// The rename/copy detection driver: git's `diffcore_rename()`.
///
/// Rewrites `q.pairs` in place, exactly as the C rewrites `diff_queued_diff`.
pub fn diffcore_rename(q: &mut Queue, opts: &Options, c: &mut dyn Content) -> Warnings {
    let mut warn = Warnings::default();
    let detect_rename = opts.detect_rename;
    let mut minimum_score = opts.rename_score;
    let want_copies = detect_rename == DETECT_COPY;
    if minimum_score == 0 {
        minimum_score = DEFAULT_RENAME_SCORE;
    }

    let mut rename_dst: Vec<RenameDst> = Vec::new();
    let mut rename_src: Vec<RenameSrc> = Vec::new();
    // git's `break_idx`: break-source pathname -> index into `rename_dst`.
    let mut break_idx: Option<HashMap<BString, usize>> = None;
    let mut rename_count = 0usize;

    for i in 0..q.pairs.len() {
        if !q.one(i).valid() {
            if !q.two(i).valid() {
                continue; // unmerged
            } else if !opts.rename_empty && is_empty_blob(q.two(i)) {
                continue;
            } else {
                rename_dst.push(RenameDst {
                    pair: i,
                    is_rename: false,
                });
            }
        } else if !opts.rename_empty && is_empty_blob(q.one(i)) {
            continue;
        } else if !q.unmerged(i) && !q.two(i).valid() {
            // If the source is a broken "delete", and they did not really want to get
            // broken, that means the source actually stays. Increment "rename_used"
            // to indicate ourselves as a user.
            if q.pairs[i].broken_pair && q.pairs[i].score == 0 {
                let one = q.pairs[i].one;
                q.specs[one].rename_used += 1;
            }
            register_rename_src(q, i, &mut rename_src, &mut break_idx, rename_dst.len());
        } else if want_copies {
            let one = q.pairs[i].one;
            q.specs[one].rename_used += 1;
            register_rename_src(q, i, &mut rename_src, &mut break_idx, rename_dst.len());
        }
    }

    if !rename_dst.is_empty() && !rename_src.is_empty() {
        // git's `hash_filespec()` gives a worktree side (which carries no object id)
        // one before the exact-match table is built.
        for d in &rename_dst {
            let two = q.pairs[d.pair].two;
            hash_filespec(&mut q.specs, two, opts.hash_kind, c);
        }
        for s in &rename_src {
            let one = q.pairs[s.pair].one;
            hash_filespec(&mut q.specs, one, opts.hash_kind, c);
        }

        // ---- exact renames -------------------------------------------------
        rename_count = find_exact_renames(q, &mut rename_dst, &rename_src, detect_rename);

        // Did we only want exact renames?
        if minimum_score < MAX_SCORE as u32 {
            if want_copies || break_idx.is_some() {
                remove_unneeded_paths_from_src(q, &mut rename_src, want_copies, &break_idx);
            } else {
                // Determine minimum score to match basenames (GIT_BASENAME_FACTOR).
                let factor = std::env::var("GIT_BASENAME_FACTOR")
                    .ok()
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .map(|n| n as f64 / 100.0)
                    .unwrap_or(0.5);
                let min_basename_score = minimum_score
                    + (factor * (MAX_SCORE - f64::from(minimum_score))) as u32;

                remove_unneeded_paths_from_src(q, &mut rename_src, want_copies, &break_idx);
                rename_count += find_basename_matches(
                    q,
                    min_basename_score,
                    &mut rename_dst,
                    &rename_src,
                    c,
                );
                remove_unneeded_paths_from_src(q, &mut rename_src, want_copies, &break_idx);
            }

            let num_destinations = rename_dst.len() - rename_count;
            let num_sources = rename_src.len();

            if num_destinations != 0 && num_sources != 0 {
                let mut skip_unmodified = false;
                let verdict = too_many_rename_candidates(
                    q,
                    num_destinations,
                    num_sources,
                    &rename_src,
                    opts,
                    &mut warn,
                );
                let bail = match verdict {
                    1 => true,
                    2 => {
                        warn.degraded_cc_to_c = true;
                        skip_unmodified = true;
                        false
                    }
                    _ => false,
                };

                if !bail {
                    // ---- the inexact similarity matrix -------------------------
                    let mut mx: Vec<Score> =
                        vec![Score::default(); NUM_CANDIDATE_PER_DST * num_destinations];
                    let mut dst_cnt = 0usize;
                    for i in 0..rename_dst.len() {
                        if rename_dst[i].is_rename {
                            continue; // exact or basename match already handled
                        }
                        let two = q.pairs[rename_dst[i].pair].two;
                        let base = dst_cnt * NUM_CANDIDATE_PER_DST;
                        for j in 0..NUM_CANDIDATE_PER_DST {
                            mx[base + j] = Score::default();
                        }
                        for j in 0..rename_src.len() {
                            if skip_unmodified && q.unmodified_pair(rename_src[j].pair) {
                                continue;
                            }
                            let one = q.pairs[rename_src[j].pair].one;
                            let score =
                                estimate_similarity(&mut q.specs, one, two, minimum_score, c);
                            let name_score = i16::from(basename_same(
                                q.specs[one].path.as_slice(),
                                q.specs[two].path.as_slice(),
                            ));
                            let this_src = Score {
                                src: j,
                                dst: i as isize,
                                score,
                                name_score,
                            };
                            record_if_better(
                                &mut mx[base..base + NUM_CANDIDATE_PER_DST],
                                &this_src,
                            );
                            // Once we ran estimate_similarity we no longer need the text.
                            free_blob(&mut q.specs[one]);
                            free_blob(&mut q.specs[two]);
                        }
                        dst_cnt += 1;
                    }

                    // Cost matrix sorted by most to least similar pair (stable).
                    mx.truncate(dst_cnt * NUM_CANDIDATE_PER_DST);
                    mx.sort_by(score_compare);

                    rename_count +=
                        find_renames(q, &mx, minimum_score, false, &mut rename_dst, &rename_src);
                    if want_copies {
                        rename_count += find_renames(
                            q,
                            &mx,
                            minimum_score,
                            true,
                            &mut rename_dst,
                            &rename_src,
                        );
                    }
                }
            }
        }
    }
    let _ = rename_count;

    // ---- write back to the queue -------------------------------------------
    let mut outq: Vec<Pair> = Vec::new();
    for i in 0..q.pairs.len() {
        let keep = if q.unmerged(i) {
            true
        } else if !q.one(i).valid() && q.two(i).valid() {
            true // creation
        } else if q.one(i).valid() && !q.two(i).valid() {
            // Deletion. We keep it if it is a broken delete whose counterpart broken
            // create remains, or a plain delete whose path was not renamed away.
            if q.pairs[i].broken_pair {
                let dst = break_idx
                    .as_ref()
                    .and_then(|m| m.get(&q.one(i).path).copied())
                    .filter(|&idx| idx != rename_dst.len())
                    .map(|idx| &rename_dst[idx]);
                !matches!(dst, Some(d) if d.is_rename)
            } else {
                q.one(i).rename_used == 0
            }
        } else {
            // All the usual ones need to be kept; unmodified pairs do not.
            !q.unmodified_pair(i)
        };
        if keep {
            outq.push(q.pairs[i].clone());
        }
    }
    q.pairs = outq;
    warn
}

/// git's `is_empty_blob_oid()`.
fn is_empty_blob(spec: &FileSpec) -> bool {
    spec.oid_valid && spec.oid == ObjectId::empty_blob(spec.oid.kind())
}

/// Drop cached blob bytes, git's `diff_free_filespec_blob()`. The spanhash table is
/// kept, exactly as git keeps `cnt_data` across the matrix.
fn free_blob(spec: &mut FileSpec) {
    spec.data = None;
}

/// git's `register_rename_src()`.
fn register_rename_src(
    q: &Queue,
    pair: usize,
    rename_src: &mut Vec<RenameSrc>,
    break_idx: &mut Option<HashMap<BString, usize>>,
    rename_dst_nr: usize,
) {
    if q.pairs[pair].broken_pair {
        break_idx
            .get_or_insert_with(HashMap::new)
            .insert(q.one(pair).path.clone(), rename_dst_nr);
    }
    rename_src.push(RenameSrc {
        pair,
        score: q.pairs[pair].score,
    });
}

/// git's `record_rename_pair()`: hand the source's pre-image spec to the destination
/// pair and mark the destination as claimed.
fn record_rename_pair(
    q: &mut Queue,
    rename_dst: &mut [RenameDst],
    rename_src: &[RenameSrc],
    dst_index: usize,
    src_index: usize,
    score: u32,
) {
    let src_pair = rename_src[src_index].pair;
    let dst_pair = rename_dst[dst_index].pair;
    let src_one = q.pairs[src_pair].one;

    q.specs[src_one].rename_used += 1;
    rename_dst[dst_index].is_rename = true;

    q.pairs[dst_pair].one = src_one;
    q.pairs[dst_pair].renamed_pair = true;
    let same = q.specs[src_one].path == q.specs[q.pairs[dst_pair].two].path;
    q.pairs[dst_pair].score = if same { rename_src[src_index].score } else { score };
}

/// git's `find_exact_renames()` plus `find_identical_files()`: match destinations to
/// sources with an identical object id, preferring unused sources and equal basenames.
fn find_exact_renames(
    q: &mut Queue,
    rename_dst: &mut Vec<RenameDst>,
    rename_src: &[RenameSrc],
    detect_rename: u8,
) -> usize {
    // git inserts sources in reverse so that the hashmap yields them LIFO, i.e. in
    // ascending index order; a plain ascending scan is the same traversal.
    let mut by_oid: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (i, s) in rename_src.iter().enumerate() {
        let one = q.pairs[s.pair].one;
        if !q.specs[one].oid_valid {
            continue;
        }
        by_oid.entry(q.specs[one].oid).or_default().push(i);
    }

    let mut renames = 0usize;
    for dst_index in 0..rename_dst.len() {
        let target = q.pairs[rename_dst[dst_index].pair].two;
        if !q.specs[target].oid_valid {
            continue;
        }
        let Some(candidates) = by_oid.get(&q.specs[target].oid) else {
            continue;
        };
        let mut best: Option<usize> = None;
        let mut best_score: i32 = -1;
        let mut i = 100i32;
        for &sidx in candidates {
            let source = q.pairs[rename_src[sidx].pair].one;
            // Non-regular files? If so, the modes must match!
            if (!q.specs[source].is_reg() || !q.specs[target].is_reg())
                && q.specs[source].mode != q.specs[target].mode
            {
                continue;
            }
            // Give higher scores to sources that haven't been used already.
            let used = q.specs[source].rename_used != 0;
            if used && detect_rename != DETECT_COPY {
                continue;
            }
            let mut score = i32::from(!used);
            score += i32::from(basename_same(
                q.specs[source].path.as_slice(),
                q.specs[target].path.as_slice(),
            ));
            if score > best_score {
                best = Some(sidx);
                best_score = score;
                if score == 2 {
                    break;
                }
            }
            // Too many identical alternatives? Pick one.
            i -= 1;
            if i == 0 {
                break;
            }
        }
        if let Some(b) = best {
            record_rename_pair(q, rename_dst, rename_src, dst_index, b, MAX_SCORE as u32);
            renames += 1;
        }
    }
    renames
}

/// git's `remove_unneeded_paths_from_src()`: cull sources already consumed by a
/// rename. Culling is incompatible with break detection and is skipped there.
fn remove_unneeded_paths_from_src(
    q: &Queue,
    rename_src: &mut Vec<RenameSrc>,
    detecting_copies: bool,
    break_idx: &Option<HashMap<BString, usize>>,
) {
    if detecting_copies {
        return; // nothing to remove (`interesting` is always NULL here)
    }
    if break_idx.is_some() {
        return; // culling incompatible with break detection
    }
    rename_src.retain(|s| q.specs[q.pairs[s.pair].one].rename_used == 0);
}

/// git's `find_basename_matches()`: over three quarters of real-world renames keep the
/// basename, so try those pairings first and keep the survivors out of the NxM matrix.
fn find_basename_matches(
    q: &mut Queue,
    minimum_score: u32,
    rename_dst: &mut Vec<RenameDst>,
    rename_src: &[RenameSrc],
    c: &mut dyn Content,
) -> usize {
    // basename -> unique index, or `None` once the basename is seen twice.
    let mut sources: HashMap<Vec<u8>, Option<usize>> = HashMap::new();
    let mut dests: HashMap<Vec<u8>, Option<usize>> = HashMap::new();
    for (i, s) in rename_src.iter().enumerate() {
        let base = get_basename(q.specs[q.pairs[s.pair].one].path.as_slice()).to_vec();
        match sources.entry(base) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.insert(None);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Some(i));
            }
        }
    }
    for i in 0..rename_dst.len() {
        if rename_dst[i].is_rename {
            continue; // involved in an exact match already
        }
        let base = get_basename(q.specs[q.pairs[rename_dst[i].pair].two].path.as_slice()).to_vec();
        match dests.entry(base) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.insert(None);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Some(i));
            }
        }
    }

    let mut renames = 0usize;
    for src_index in 0..rename_src.len() {
        let one = q.pairs[rename_src[src_index].pair].one;
        let base = get_basename(q.specs[one].path.as_slice()).to_vec();
        let Some(dst_slot) = dests.get(&base) else {
            continue;
        };
        // With no directory-rename info (`info->setup == 0`), a non-unique basename on
        // either side leaves `idx_possible_rename()` returning -1, so the pairing is
        // simply skipped.
        let (Some(src_unique), Some(dst_index)) = (sources.get(&base).copied().flatten(), *dst_slot)
        else {
            continue;
        };
        if src_unique != src_index {
            continue;
        }
        if rename_dst[dst_index].is_rename {
            continue; // already used previously
        }

        let two = q.pairs[rename_dst[dst_index].pair].two;
        let score = estimate_similarity(&mut q.specs, one, two, minimum_score, c);
        if score < minimum_score {
            continue;
        }
        record_rename_pair(q, rename_dst, rename_src, dst_index, src_index, score);
        renames += 1;
        free_blob(&mut q.specs[one]);
        free_blob(&mut q.specs[two]);
    }
    renames
}

/// git's `too_many_rename_candidates()`.
///
/// Returns `0` when under the limit, `1` when inexact detection must be disabled, and
/// `2` when `-C -C` would fit if it were only `-C`.
fn too_many_rename_candidates(
    q: &Queue,
    num_destinations: usize,
    num_sources: usize,
    rename_src: &[RenameSrc],
    opts: &Options,
    warn: &mut Warnings,
) -> u8 {
    let rename_limit = opts.rename_limit;
    warn.needed_rename_limit = 0;

    if rename_limit <= 0 {
        return 0; // treat as unlimited
    }
    let limit_sq = (rename_limit as u128) * (rename_limit as u128);
    if (num_destinations as u128) * (num_sources as u128) <= limit_sq {
        return 0;
    }

    warn.needed_rename_limit = num_sources.max(num_destinations);

    // Are we running under -C -C?
    if !opts.find_copies_harder {
        return 1;
    }

    // Would we bust the limit if we were running under -C?
    let limited_sources = rename_src
        .iter()
        .filter(|s| !q.unmodified_pair(s.pair))
        .count();
    if (num_destinations as u128) * (limited_sources as u128) <= limit_sq {
        return 2;
    }
    1
}

/// git's `find_renames()`: walk the sorted cost matrix and claim pairings.
fn find_renames(
    q: &mut Queue,
    mx: &[Score],
    minimum_score: u32,
    copies: bool,
    rename_dst: &mut Vec<RenameDst>,
    rename_src: &[RenameSrc],
) -> usize {
    let mut count = 0usize;
    for cell in mx {
        if cell.dst < 0 || cell.score < minimum_score {
            break; // there is no more usable pair
        }
        let dst_index = cell.dst as usize;
        if rename_dst[dst_index].is_rename {
            continue; // already done, either exact or fuzzy
        }
        if !copies && q.specs[q.pairs[rename_src[cell.src].pair].one].rename_used != 0 {
            continue;
        }
        record_rename_pair(q, rename_dst, rename_src, dst_index, cell.src, cell.score);
        count += 1;
    }
    count
}

/// git's `diff_resolve_rename_copy()`: assign every pair its final status letter.
pub fn resolve_rename_copy(q: &mut Queue) {
    for i in 0..q.pairs.len() {
        let status = if q.unmerged(i) {
            b'U'
        } else if !q.one(i).valid() {
            b'A'
        } else if !q.two(i).valid() {
            b'D'
        } else if q.type_changed(i) {
            b'T'
        } else if q.pairs[i].renamed_pair {
            // A rename might have re-connected a broken pair, making the pathnames the
            // same again — that is a modification, not a rename. Otherwise, a source
            // used for multiple renames means all but the last are copies.
            if q.one(i).path == q.two(i).path {
                b'M'
            } else {
                let one = q.pairs[i].one;
                q.specs[one].rename_used -= 1;
                if q.specs[one].rename_used > 0 {
                    b'C'
                } else {
                    b'R'
                }
            }
        } else {
            b'M'
        };
        q.pairs[i].status = status;
    }
}

// ---------------------------------------------------------------------------
// diffcore-break.c
// ---------------------------------------------------------------------------

/// git's `should_break()`: is this in-place edit so large that recording it as a
/// delete plus a create would serve rename/copy detection better?
///
/// Leaves the "how much of the source was removed" score in the returned tuple's
/// second element (git's `*merge_score_p`).
fn should_break(
    specs: &mut Vec<FileSpec>,
    src: usize,
    dst: usize,
    break_score: u32,
    c: &mut dyn Content,
) -> (bool, u32) {
    // Assume no deletion --- "do not break" is the default.
    if specs[src].is_reg() != specs[dst].is_reg() {
        return (true, MAX_SCORE as u32); // even their types are different
    }
    if specs[src].oid_valid && specs[dst].oid_valid && specs[src].oid == specs[dst].oid {
        return (false, 0); // they are the same
    }
    if !populate_data(specs, src, c) || !populate_data(specs, dst, c) {
        return (false, 0); // error but caught downstream
    }

    let src_size = spec_size(&specs[src]);
    let dst_size = spec_size(&specs[dst]);
    let max_size = src_size.max(dst_size);
    if max_size < MINIMUM_BREAK_SIZE {
        return (false, 0); // we do not break too small filepair
    }
    if src_size == 0 {
        return (false, 0); // we do not let empty files get renamed
    }

    let (mut src_copied, mut literal_added) = counted_changes(specs, src, dst);

    // sanity
    if src_size < src_copied {
        src_copied = src_size;
    }
    if dst_size < literal_added + src_copied {
        literal_added = if src_copied < dst_size {
            dst_size - src_copied
        } else {
            0
        };
    }
    let src_removed = src_size - src_copied;

    // "how much is removed from the source material" — the clean-up stage merges the
    // surviving pair back together when this is below the merge score.
    let merge_score = ((src_removed as f64) * MAX_SCORE / (src_size as f64)) as u32;
    if merge_score > break_score {
        return (true, merge_score);
    }

    // Extent of damage, counting both inserts and deletes.
    let delta_size = src_removed + literal_added;
    if (delta_size as f64) * MAX_SCORE / (max_size as f64) < f64::from(break_score) {
        return (false, merge_score);
    }

    // If you removed a lot without adding new material, that is not really a rewrite.
    // The left-hand side is integer (`unsigned long * int`) in C; the right-hand side
    // is `double`, so the comparison happens in `double`.
    if (src_size.wrapping_mul(u64::from(break_score)) as f64) < (src_removed as f64) * MAX_SCORE
        && literal_added * 20 < src_removed
        && literal_added * 20 < src_copied
    {
        return (false, merge_score);
    }

    (true, merge_score)
}

/// git's `diffcore_break()`: split every sufficiently-rewritten in-place edit into a
/// delete plus a create so rename/copy detection can re-pair the pieces.
pub fn diffcore_break(q: &mut Queue, break_opt: i64, c: &mut dyn Content) {
    let mut merge_score = ((break_opt >> 16) & 0xFFFF) as u32;
    let mut break_score = (break_opt & 0xFFFF) as u32;
    if break_score == 0 {
        break_score = DEFAULT_BREAK_SCORE;
    }
    if merge_score == 0 {
        merge_score = DEFAULT_MERGE_SCORE;
    }

    let mut outq: Vec<Pair> = Vec::new();
    for i in 0..q.pairs.len() {
        let (one, two) = (q.pairs[i].one, q.pairs[i].two);
        // We deal only with in-place edit of blobs; we do not break anything else.
        let breakable = q.specs[one].valid()
            && q.specs[two].valid()
            && is_blob_mode(q.specs[one].mode)
            && is_blob_mode(q.specs[two].mode)
            && q.specs[one].path == q.specs[two].path;
        if breakable {
            let (yes, mut score) = should_break(&mut q.specs, one, two, break_score, c);
            if yes {
                // Set score to 0 for the pair that needs to be merged back together
                // should it survive rename/copy.
                if score < merge_score {
                    score = 0;
                }
                let null_one = q.add_spec(FileSpec::absent(q.specs[one].path.clone()));
                outq.push(Pair {
                    one,
                    two: null_one,
                    score,
                    renamed_pair: false,
                    broken_pair: true,
                    status: 0,
                });
                let null_two = q.add_spec(FileSpec::absent(q.specs[two].path.clone()));
                outq.push(Pair {
                    one: null_two,
                    two,
                    score,
                    renamed_pair: false,
                    broken_pair: true,
                    status: 0,
                });
                free_blob(&mut q.specs[one]);
                free_blob(&mut q.specs[two]);
                continue;
            }
        }
        outq.push(q.pairs[i].clone());
    }
    q.pairs = outq;
}

/// git's `object_type(mode) == OBJ_BLOB`: a regular file or a symlink.
fn is_blob_mode(mode: u32) -> bool {
    let fmt = mode & 0o170000;
    fmt == 0o100000 || fmt == 0o120000
}

/// git's `diffcore_merge_broken()`: a broken pair whose halves both survived
/// rename/copy detection is glued back into a single modification.
pub fn diffcore_merge_broken(q: &mut Queue) {
    let mut outq: Vec<Pair> = Vec::new();
    let mut taken = vec![false; q.pairs.len()];
    for i in 0..q.pairs.len() {
        if taken[i] {
            continue; // we already merged this with its peer
        }
        let p = q.pairs[i].clone();
        if p.broken_pair && q.specs[p.one].path == q.specs[p.two].path {
            let mut merged = false;
            for j in i + 1..q.pairs.len() {
                let pp = q.pairs[j].clone();
                if pp.broken_pair
                    && q.specs[pp.one].path == q.specs[pp.two].path
                    && q.specs[p.one].path == q.specs[pp.two].path
                {
                    // Peer survived — merge them.
                    let (d, cc) = if q.specs[p.one].valid() { (&p, &pp) } else { (&pp, &p) };
                    q.specs[d.one].rename_used += 1;
                    outq.push(Pair {
                        one: d.one,
                        two: cc.two,
                        score: p.score,
                        renamed_pair: false,
                        broken_pair: true,
                        status: 0,
                    });
                    taken[j] = true;
                    merged = true;
                    break;
                }
            }
            if !merged {
                outq.push(p);
            }
        } else {
            outq.push(p);
        }
    }
    q.pairs = outq;
}

/// The `diffcore_std()` slice this module owns: break, then rename, then merge-broken.
pub fn run(q: &mut Queue, opts: &Options, c: &mut dyn Content) -> Warnings {
    let mut warn = Warnings::default();
    if opts.break_opt != -1 {
        diffcore_break(q, opts.break_opt, c);
    }
    if opts.detect_rename != 0 {
        warn = diffcore_rename(q, opts, c);
    }
    if opts.break_opt != -1 {
        diffcore_merge_broken(q);
    }
    warn
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_rename_score()` maps the three spellings git accepts onto the same
    /// internal `MAX_SCORE` units.
    #[test]
    fn rename_score_spellings() {
        assert_eq!(parse_rename_score("50").0, 30000);
        assert_eq!(parse_rename_score("50%").0, 30000);
        assert_eq!(parse_rename_score(".5").0, 30000);
        assert_eq!(parse_rename_score("").0, 0);
        // Without a `%`, `parse_num` divides by the scale it accumulated while
        // reading the digits, so `100` is 100/1000 — ten percent, not a hundred.
        // Verified against stock git: `-M100` detects a 50%-similar rename while
        // `-M100%` does not.
        assert_eq!(parse_rename_score("100").0, 6000);
        assert_eq!(parse_rename_score("100%").0, 60000);
        // Anything past the number is handed back for the caller to reject.
        assert_eq!(parse_rename_score("50/70").1, "/70");
    }

    /// `-B<n>/<m>` packs the two scores into one int the way `diff_opt_break_rewrites`
    /// does, and rejects a trailing garbage suffix.
    #[test]
    fn break_opt_packing() {
        assert_eq!(parse_break_opt("").unwrap(), 0);
        assert_eq!(parse_break_opt("50").unwrap(), 30000);
        assert_eq!(parse_break_opt("50/60").unwrap(), 30000 | (36000 << 16));
        assert!(parse_break_opt("50x").is_err());
    }

    /// Identical content is a perfect copy: every chunk of dst came from src.
    #[test]
    fn identical_content_scores_max() {
        let body = b"alpha\nbeta\ngamma\ndelta\n".repeat(20);
        let a = hash_chars(&body);
        let b = hash_chars(&body);
        let (copied, added) = count_changes(&a, &b);
        assert_eq!(copied, body.len() as u64);
        assert_eq!(added, 0);
    }

    /// Disjoint content shares nothing.
    #[test]
    fn disjoint_content_scores_zero() {
        let a = hash_chars(&b"one\ntwo\nthree\n".repeat(30));
        let b = hash_chars(&b"xxx\nyyy\nzzz\n".repeat(30));
        let (copied, _) = count_changes(&a, &b);
        assert_eq!(copied, 0);
    }

    /// Appending to a file leaves every original byte "copied" and counts only the
    /// appended bytes as literally added — the property the whole estimator rests on.
    #[test]
    fn appended_tail_is_literal_added() {
        let base: Vec<u8> = b"line\n".repeat(50);
        let mut grown = base.clone();
        grown.extend_from_slice(&b"new\n".repeat(10));
        let (copied, added) = count_changes(&hash_chars(&base), &hash_chars(&grown));
        assert_eq!(copied, base.len() as u64);
        assert_eq!(added, 40);
    }

    /// The table must grow past its initial 512 buckets without losing counts.
    #[test]
    fn spanhash_rehashes_without_losing_counts() {
        let mut body = Vec::new();
        for i in 0..5000 {
            body.extend_from_slice(format!("unique line number {i}\n").as_bytes());
        }
        let h = hash_chars(&body);
        assert!(h.alloc_log2 > INITIAL_HASH_SIZE);
        let total: u64 = h.data.iter().map(|s| u64::from(s.cnt)).sum();
        assert_eq!(total, body.len() as u64);
    }

    /// CR is skipped inside CRLF for text, so a CRLF file and its LF twin hash alike.
    #[test]
    fn crlf_is_normalized_for_text() {
        let lf: Vec<u8> = b"alpha\nbeta\n".repeat(20);
        let crlf: Vec<u8> = b"alpha\r\nbeta\r\n".repeat(20);
        let (copied, added) = count_changes(&hash_chars(&lf), &hash_chars(&crlf));
        assert_eq!(copied, lf.len() as u64);
        assert_eq!(added, 0);
    }

    /// `similarity_index` is what the `similarity index <n>%` header prints.
    #[test]
    fn similarity_index_percentages() {
        assert_eq!(similarity_index(60000), 100);
        assert_eq!(similarity_index(30000), 50);
        assert_eq!(similarity_index(0), 0);
        assert_eq!(similarity_index(59999), 99);
    }

    /// `basename_same` compares only the final path component.
    #[test]
    fn basename_comparison() {
        assert!(basename_same(b"a/b/foo.c", b"x/y/foo.c"));
        assert!(basename_same(b"foo.c", b"x/foo.c"));
        assert!(!basename_same(b"a/foo.c", b"a/bar.c"));
        assert!(!basename_same(b"a/xfoo.c", b"a/foo.c"));
    }
}
