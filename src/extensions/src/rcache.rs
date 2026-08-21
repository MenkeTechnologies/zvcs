//! Zero-copy caches for derived answers (`~/.zvcs/cache/`).
//!
//! Everything cached here is a pure function of immutable git objects: a
//! tree-to-tree diff, a `(commit, path)` blame, an object id's abbreviation at a
//! given hex length. Such an answer never expires and is valid in every clone
//! that holds the objects, so the store needs no invalidation — only fast reads.
//!
//! # Why not the SQLite ledger
//!
//! These rows used to live in `db.sqlite` beside the job/event ledger. The
//! ledger earns SQLite: sixteen concurrent shells mutate it, two triggers derive
//! the event feed from status updates, and event ids must be monotonic. A cache
//! of immutable answers earns none of that. It paid a query planner, row
//! decoding and a `String` allocation per column to hand back bytes that were
//! already sitting on disk in usable form.
//!
//! # Layout
//!
//! ```text
//! ~/.zvcs/cache/<name>.rkyv   base image: rkyv archive, mmap'd, read in place
//! ~/.zvcs/cache/<name>.log    append journal: [u32 klen][u32 vlen][key][val]
//! ~/.zvcs/cache/<name>.lock   flock target, held only by writers
//! ```
//!
//! The base image is one [`Table`]: a dense, sorted `index` of fixed-size
//! [`Entry`] records and a `heap` holding the key and value bytes. A lookup
//! binary-searches the index on a 64-bit key hash, then compares the full key
//! bytes out of the heap — a hash collision therefore costs one extra compare
//! and can never serve a wrong diff. Nothing is decoded, allocated or copied to
//! answer a hit; the returned slice points into the mapping.
//!
//! Splitting the index from the heap keeps validation cheap as well as lookups.
//! [`rkyv::check_archived_root`] on a `Vec` of pointer-free scalar records is a
//! bounds check plus a linear scan of plain integers — there is no pointer to
//! chase per entry, which is what makes validating a multi-megabyte image on
//! every `git log` affordable.
//!
//! # Why a journal
//!
//! The obvious rkyv cache (as in zshrs' script cache) reads the whole image,
//! mutates it and rewrites it per put. That suits a few hundred entries. The
//! tree-diff cache is written once per commit walked and is already megabytes,
//! so rewriting it per batch would be slower than the SQLite insert it replaces.
//! Instead writers append to a journal — an ordinary length-prefixed byte log,
//! where rkyv would buy nothing because nothing reads it in place — and fold it
//! into the base image once it crosses [`COMPACT_BYTES`]. Readers merge the two:
//! the journal is bounded by that threshold, so the read cost it adds is bounded
//! with it.
//!
//! # Concurrency
//!
//! Readers take no lock. Writers hold `flock(LOCK_EX)` on the lock file across
//! the append, and across a compaction, so appends never interleave and only one
//! process compacts. Compaction writes a temporary file, fsyncs it and renames
//! it over the base, so a reader either maps the whole old image or the whole new
//! one — never a partial write.
//!
//! Every remaining race degrades to a cache miss, never a wrong answer:
//!
//! * A reader that mapped the old base and then read the truncated journal misses
//!   the entries the compaction folded in.
//! * A reader that reads the journal mid-append sees a torn tail record, stops
//!   there, and misses that batch.
//!
//! A miss costs a recomputation, which is what the command would have done
//! anyway. That tolerance is why the read path can be lock-free.
//!
//! # No version pin
//!
//! Unlike zshrs' script cache, the header pins no crate version. Entries are keyed
//! by content, not by the binary that produced them, so they stay correct across
//! releases — pinning would discard the whole warm cache on every version bump.
//! [`FORMAT_VERSION`] covers a change to this file's layout or hash, which is the
//! only thing that can invalidate an *image*.
//!
//! What it does not cover is a change to what a key **means**, because it is
//! checked against the base image's header and the journal has none: a
//! `FORMAT_VERSION` bump discards `<name>.rkyv` and then merges every stale row of
//! `<name>.log` straight back in. A key whose meaning changed is retired by
//! versioning the key itself instead, so the old byte string can never be looked
//! up again — see [`ABBREV_KEY_VERSION`], which exists because one
//! `git -c core.abbrev=no log` used to poison every later `git log --oneline` on
//! the machine.

use memmap2::Mmap;
use rkyv::{Archive, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `"ZVCA"` — identifies a base image written by this module.
pub const MAGIC: u32 = 0x5A56_4341;

/// Bumped when the on-disk layout or the key hash changes. An image carrying any
/// other value is ignored (and overwritten by the next compaction) rather than
/// migrated: it is a cache, so discarding it costs recomputation and nothing else.
pub const FORMAT_VERSION: u32 = 1;

/// Fold the journal into the base image once it reaches this size. The bound is
/// what caps the reader's journal scan, so it trades read cost against how often
/// a writer pays a full rewrite.
const COMPACT_BYTES: u64 = 1 << 20;

/// Fixed preamble, checked before any offset in the image is trusted.
#[derive(Archive, Serialize)]
#[archive(check_bytes)]
pub struct Header {
    pub magic: u32,
    pub format_version: u32,
    /// Width of the pointers the writing build used. Offsets here are explicit
    /// `u32`s, but rkyv's own relative pointers are not, so an image written by a
    /// 64-bit build is not readable by a 32-bit one.
    pub pointer_width: u32,
    pub entries: u32,
    pub built_at: i64,
}

/// One cached answer, as a pointer-free record: the key and value live in the
/// table's heap and are named by offset, so the index stays a dense array that
/// binary search can walk without dereferencing anything.
#[derive(Archive, Serialize, Clone, Copy)]
#[archive(check_bytes)]
pub struct Entry {
    /// [`hash_key`] of the full key, the binary-search discriminator.
    pub hash: u64,
    pub key_off: u32,
    pub key_len: u32,
    pub val_off: u32,
    pub val_len: u32,
}

/// The base image: an index sorted by `(hash, key)` over a byte heap.
#[derive(Archive, Serialize)]
#[archive(check_bytes)]
pub struct Table {
    pub header: Header,
    pub index: Vec<Entry>,
    pub heap: Vec<u8>,
}

/// FNV-1a, 64-bit.
///
/// The hash is persisted inside the image, so it must be identical on every
/// build and every architecture this ships to. `DefaultHasher` is explicitly
/// documented as unstable across releases and cannot be used for that. FNV is
/// specified by its constants, which is exactly the property needed here, and the
/// keys it is fed (hex object ids) are already high-entropy.
fn hash_key(key: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `~/.zvcs/cache` (honors `ZVCS_HOME`).
pub fn cache_dir() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("cache")
}

// ---------------------------------------------------------------------------
// reading
// ---------------------------------------------------------------------------

/// One named cache, opened once per process.
///
/// The read state — the mapped image and the journal's contents — is built on
/// the first lookup, not at open. A command that only *writes* (the writer
/// thread knows nothing but the three paths) would otherwise map a multi-megabyte
/// image and parse a journal for nothing, and it would pay for it inside
/// [`cache_flush`], which is the one place the command actually waits.
///
/// Both are then snapshots: a command's own writes land in the journal on disk
/// but are not merged back into its view, because every caller already holds the
/// value it just computed.
pub struct Cache {
    base: PathBuf,
    journal: PathBuf,
    lock: PathBuf,
    /// The mapped image, or `None` before the first compaction has written one.
    table: OnceLock<Option<&'static ArchivedTable>>,
    /// Entries appended since that image was built.
    recent: OnceLock<HashMap<Box<[u8]>, Box<[u8]>>>,
}

impl Cache {
    fn open(name: &str) -> Cache {
        Cache::in_dir(&cache_dir(), name)
    }

    /// Open a cache under an explicit directory. Tests drive this so they never
    /// have to move `ZVCS_HOME` out from under a concurrently running test.
    fn in_dir(dir: &Path, name: &str) -> Cache {
        let _ = std::fs::create_dir_all(dir);
        Cache {
            base: dir.join(format!("{name}.rkyv")),
            journal: dir.join(format!("{name}.log")),
            lock: dir.join(format!("{name}.lock")),
            table: OnceLock::new(),
            recent: OnceLock::new(),
        }
    }

    /// The cached value for `key`, borrowed from the mapping or the journal.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        if let Some(v) = self.recent.get_or_init(|| read_journal(&self.journal)).get(key) {
            return Some(v);
        }
        self.find(key, hash_key(key))
    }

    /// The image half of [`get`](Self::get): the value stored under `key` in the
    /// `want` bucket.
    ///
    /// Split out from `get` so the bucket walk can be exercised with keys forced
    /// into one bucket — a hash collision is what the full-key compare exists to
    /// survive, and it cannot be produced on demand through `get`, which derives
    /// the bucket from the key it is handed.
    fn find(&self, key: &[u8], want: u64) -> Option<&[u8]> {
        let table = (*self.table.get_or_init(|| map_base(&self.base)))?;
        let heap: &[u8] = &table.heap;
        let index: &[ArchivedEntry] = &table.index;
        // The index is sorted by hash, so equal-hash entries are contiguous:
        // find the first and walk them, comparing full keys.
        let mut i = index.partition_point(|e| u64::from(e.hash) < want);
        while let Some(e) = index.get(i) {
            if u64::from(e.hash) != want {
                break;
            }
            if slice_at(heap, e.key_off, e.key_len) == Some(key) {
                return slice_at(heap, e.val_off, e.val_len);
            }
            i += 1;
        }
        None
    }

    /// Number of entries in the mapped image (0 before the first compaction).
    pub fn base_len(&self) -> usize {
        self.table.get_or_init(|| map_base(&self.base)).map_or(0, |t| t.index.len())
    }

    /// Number of entries the journal held when it was first read.
    pub fn journal_len(&self) -> usize {
        self.recent.get_or_init(|| read_journal(&self.journal)).len()
    }
}

/// A heap slice named by an archived offset/length pair, or `None` if the pair
/// does not lie inside the heap.
///
/// rkyv validates that the heap itself is in bounds; these offsets are ordinary
/// integers it knows nothing about, so they are checked here on every access.
/// That check is two comparisons and it is what keeps a corrupt image a miss
/// rather than an out-of-bounds read.
fn slice_at(heap: &[u8], off: impl Into<u32>, len: impl Into<u32>) -> Option<&[u8]> {
    let off = usize::try_from(off.into()).ok()?;
    let len = usize::try_from(len.into()).ok()?;
    heap.get(off..off.checked_add(len)?)
}

/// Map a base image and validate it, or `None` if it is absent, truncated, from
/// another format version, or fails rkyv's structural check.
///
/// The mapping is deliberately leaked. It has to outlive every slice handed out
/// of it, and it is wanted for the whole process anyway — three mappings, one per
/// cache, released when the command exits.
fn map_base(path: &Path) -> Option<&'static ArchivedTable> {
    let file = File::open(path).ok()?;
    // SAFETY: the image is replaced only by rename, never written in place, so an
    // existing mapping keeps pointing at the bytes it was opened on. A file
    // truncated or corrupted out from under us by something other than this
    // module is caught by the validation below, not by the map itself.
    let mmap = unsafe { Mmap::map(&file).ok()? };
    let leaked: &'static Mmap = Box::leak(Box::new(mmap));
    archived(leaked)
}

/// Validate a mapping and return its root table.
fn archived(bytes: &[u8]) -> Option<&ArchivedTable> {
    let table = rkyv::check_archived_root::<Table>(bytes).ok()?;
    let h = &table.header;
    let ok = u32::from(h.magic) == MAGIC
        && u32::from(h.format_version) == FORMAT_VERSION
        && u32::from(h.pointer_width) == usize::BITS;
    ok.then_some(table)
}

/// Read every intact record from the journal.
///
/// A record whose length prefix runs past the end of the file is a torn tail — a
/// batch that was still being appended, or one lost to a crash. Reading stops
/// there: the entries before it are complete and usable.
fn read_journal(path: &Path) -> HashMap<Box<[u8]>, Box<[u8]>> {
    let mut out = HashMap::new();
    let Ok(mut file) = File::open(path) else {
        return out;
    };
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return out;
    }
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let klen = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap()) as usize;
        let vlen = u32::from_le_bytes(buf[i + 4..i + 8].try_into().unwrap()) as usize;
        let key_at = i + 8;
        let Some(val_at) = key_at.checked_add(klen) else { break };
        let Some(end) = val_at.checked_add(vlen) else { break };
        if end > buf.len() {
            break;
        }
        // A later record for the same key supersedes an earlier one.
        out.insert(buf[key_at..val_at].into(), buf[val_at..end].into());
        i = end;
    }
    out
}

// ---------------------------------------------------------------------------
// the three caches
// ---------------------------------------------------------------------------

macro_rules! named_cache {
    ($fn_name:ident, $file:literal) => {
        pub fn $fn_name() -> &'static Cache {
            static CELL: OnceLock<Cache> = OnceLock::new();
            CELL.get_or_init(|| Cache::open($file))
        }
    };
}

named_cache!(treediff, "treediff");
named_cache!(abbrev, "abbrev");
named_cache!(blame, "blame");

/// `<old_tree>\0<new_tree>\0<counts>`.
///
/// `counts` is part of the key because the per-file line tallies require reading
/// each blob: a caller that does not need them stores the cheaper answer, and the
/// two must not be confused for one another.
fn treediff_key(old_tree: &str, new_tree: &str, counts: bool) -> Vec<u8> {
    let mut k = Vec::with_capacity(old_tree.len() + new_tree.len() + 3);
    k.extend_from_slice(old_tree.as_bytes());
    k.push(0);
    k.extend_from_slice(new_tree.as_bytes());
    k.push(0);
    k.push(u8::from(counts));
    k
}

/// The change list cached for a tree pair.
pub fn treediff_load(old_tree: &str, new_tree: &str, counts: bool) -> Option<&'static str> {
    let v = treediff().get(&treediff_key(old_tree, new_tree, counts))?;
    std::str::from_utf8(v).ok()
}

/// What an abbrev key's `hex_len` byte means, appended so a change to that
/// meaning retires every row written under the old one.
///
/// [`FORMAT_VERSION`] cannot do this job. It guards the *layout* of the base
/// image and is checked when that image is mapped, so a bump discards
/// `abbrev.rkyv` — but not `abbrev.log`, which carries no header and is merged
/// into every read regardless. It is also shared with the tree-diff and blame
/// caches, whose (much larger) rows have nothing wrong with them.
///
/// Versioning the key instead retires exactly the rows whose meaning changed:
/// a key written at another version is a different byte string, so it can never
/// be found again by any lookup. The retired rows stay in the image until it is
/// next rewritten and cost a lookup nothing — the index is binary-searched, not
/// scanned — which is the price of not deleting a file that concurrent readers
/// may have mapped.
///
/// * `1` — `hex_len` was `core.abbrev` read as an integer, so `auto`, `no`,
///   `off`, `false` and every unparsable value all keyed as `0`. `no` and its
///   spellings mean the *full* hash while `auto` means a derived width, so one
///   `git -c core.abbrev=no log` poisoned `auto` for every later command on the
///   machine. `AbbrevCache::hex_len` in `porcelain::log` carries the C.
/// * `2` — `hex_len` is the resolved width from
///   [`crate::abbrev::configured_abbrev`]: `auto`'s derived number, or the hash's
///   own hex width for the false-y words.
///
/// The one-shot import out of the old SQLite ledger ([`crate::db`]) stamps the
/// current version onto rows the ledger stored under the version-1 meaning. That
/// is safe rather than a second poisoning: a numeric `core.abbrev` meant the same
/// number under both, and the `0` rows — the poisoned ones — become keys nothing
/// asks for, because every reader now resolves to at least
/// [`crate::abbrev::MINIMUM_ABBREV`] and git itself refuses `core.abbrev = 0`
/// with `abbrev length out of range: 0` before any command runs.
pub const ABBREV_KEY_VERSION: u8 = 2;

/// Bytes an abbrev key can take: the largest hash gix supports (32) plus the hex
/// length and [`ABBREV_KEY_VERSION`].
pub const ABBREV_KEY_MAX: usize = 34;

/// Raw object id bytes, the hex length, then [`ABBREV_KEY_VERSION`], written into
/// `buf`.
///
/// The key is the id's bytes rather than its hex text because `log --oneline`
/// looks up every commit and every parent: formatting a 40-byte string per lookup
/// just to hash it was most of what abbreviation cost.
///
/// The hex length is part of the key because the correct abbreviation grows with
/// the repository — a prefix that was unique when it was computed must not be
/// served once the repo needs a longer one — and because two different
/// `core.abbrev` settings must not answer for each other.
pub fn abbrev_key_into(
    buf: &mut [u8; ABBREV_KEY_MAX],
    oid: &[u8],
    hex_len: usize,
) -> Option<usize> {
    if oid.len() > 32 || hex_len > u8::MAX as usize {
        return None;
    }
    buf[..oid.len()].copy_from_slice(oid);
    buf[oid.len()] = hex_len as u8;
    buf[oid.len() + 1] = ABBREV_KEY_VERSION;
    Some(oid.len() + 2)
}

/// The abbreviation cached for an object id at `hex_len`.
pub fn abbrev_load(oid: &[u8], hex_len: usize) -> Option<&'static str> {
    let mut buf = [0u8; ABBREV_KEY_MAX];
    let n = abbrev_key_into(&mut buf, oid, hex_len)?;
    let v = abbrev().get(&buf[..n])?;
    std::str::from_utf8(v).ok()
}

/// `<commit>\0<path>\0<algo>`. A path cannot contain NUL, and neither the commit
/// hex nor the algorithm key can, so the parts stay unambiguous.
fn blame_key(commit: &str, path: &str, algo: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(commit.len() + path.len() + algo.len() + 2);
    k.extend_from_slice(commit.as_bytes());
    k.push(0);
    k.extend_from_slice(path.as_bytes());
    k.push(0);
    k.extend_from_slice(algo.as_bytes());
    k
}

/// The blamed blob and the run-length encoded attribution cached for
/// `(commit, path, algo)`.
///
/// The blob is stored with the runs so the caller can reject an entry whose file
/// content is not what it is about to print — the value is `<blob>\0<runs>`, and
/// NUL is used rather than a newline because the run encoding contains newlines.
pub fn blame_load(commit: &str, path: &str, algo: &str) -> Option<(&'static str, &'static str)> {
    let v = blame().get(&blame_key(commit, path, algo))?;
    let split = v.iter().position(|b| *b == 0)?;
    let blob = std::str::from_utf8(&v[..split]).ok()?;
    let runs = std::str::from_utf8(&v[split + 1..]).ok()?;
    Some((blob, runs))
}

// ---------------------------------------------------------------------------
// off-thread writes
// ---------------------------------------------------------------------------

/// A row a read-only verb computed and wants remembered.
///
/// Filling a cache must never be something the user waits for. The value is
/// already in hand — the command can print it and go — so the write belongs off
/// the critical path entirely.
pub enum CacheWrite {
    /// A tree pair's change list, with or without the per-file line counts.
    TreeDiff { old_tree: String, new_tree: String, counts: bool, files: String },
    /// Abbreviations computed at one `core.abbrev` length, keyed by raw id bytes.
    Abbrev { hex_len: usize, rows: Vec<(Vec<u8>, String)> },
    /// One `(commit, path)` blame, run-length encoded.
    Blame { commit: String, path: String, algo: String, blob: String, runs: String },
}

impl CacheWrite {
    /// Which cache this row belongs to.
    fn cache(&self) -> &'static Cache {
        match self {
            CacheWrite::TreeDiff { .. } => treediff(),
            CacheWrite::Abbrev { .. } => abbrev(),
            CacheWrite::Blame { .. } => blame(),
        }
    }

    /// Encode into journal records. Only `Abbrev` yields more than one.
    fn rows(self) -> Vec<(Vec<u8>, Vec<u8>)> {
        match self {
            CacheWrite::TreeDiff { old_tree, new_tree, counts, files } => {
                vec![(treediff_key(&old_tree, &new_tree, counts), files.into_bytes())]
            }
            CacheWrite::Abbrev { hex_len, rows } => rows
                .into_iter()
                .filter_map(|(oid, short)| {
                    let mut buf = [0u8; ABBREV_KEY_MAX];
                    let n = abbrev_key_into(&mut buf, &oid, hex_len)?;
                    Some((buf[..n].to_vec(), short.into_bytes()))
                })
                .collect(),
            CacheWrite::Blame { commit, path, algo, blob, runs } => {
                let mut val = Vec::with_capacity(blob.len() + runs.len() + 1);
                val.extend_from_slice(blob.as_bytes());
                val.push(0);
                val.extend_from_slice(runs.as_bytes());
                vec![(blame_key(&commit, &path, &algo), val)]
            }
        }
    }
}

/// The writer thread and the channel feeding it, started on first use.
static WRITER: std::sync::Mutex<
    Option<(std::sync::mpsc::Sender<CacheWrite>, std::thread::JoinHandle<()>)>,
> = std::sync::Mutex::new(None);

/// Queue a cache row and return immediately.
///
/// Every caller here is a read-only verb whose answer is already computed, so the
/// write is pure bookkeeping. Doing it inline made the first (cold) run of
/// `log --stat` measurably slower than plain git — exactly the run where the user
/// has the least patience for it. The rows go to a single writer thread that
/// batches them into one append, and [`cache_flush`] waits for it once, at the
/// very end of the command, after the output is already on its way.
pub fn cache_write(job: CacheWrite) {
    let mut guard = match WRITER.lock() {
        Ok(g) => g,
        // A poisoned lock means a previous writer panicked; losing cache rows is
        // not worth failing a user's command over.
        Err(_) => return,
    };
    if guard.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<CacheWrite>();
        let handle = std::thread::spawn(move || writer_loop(rx));
        *guard = Some((tx, handle));
    }
    if let Some((tx, _)) = guard.as_ref() {
        let _ = tx.send(job);
    }
}

/// Wait for queued cache rows to land. Call once, as late as possible.
///
/// A detached thread does not outlive the process, so the queue does have to be
/// drained before exit — but by then the command has done all of its real work,
/// so what is waited on is only whatever the writer has not already absorbed
/// while the command was running.
pub fn cache_flush() {
    let taken = match WRITER.lock() {
        Ok(mut g) => g.take(),
        Err(_) => return,
    };
    if let Some((tx, handle)) = taken {
        // Dropping the sender ends the writer's loop once the queue is empty.
        drop(tx);
        let _ = handle.join();
    }
    ensure_imported();
}

/// Marker recording that the one-time import out of the SQLite ledger has run.
fn imported_marker() -> PathBuf {
    cache_dir().join(".imported")
}

/// Carry the ledger's old cache tables over, once per machine.
///
/// The import itself lives in the ledger's schema migration, but nothing on a
/// read-only path opens the ledger read-write any more — that is the whole point
/// of the move — so a machine that only ever ran `log` would leave megabytes of
/// already-computed diffs stranded in SQLite and recompute them instead. This is
/// the trigger: a single `exists` check at the end of a command, after the output
/// is out, and one ledger open the first time it finds no marker.
fn ensure_imported() {
    let marker = imported_marker();
    if marker.exists() {
        return;
    }
    // No ledger means nothing to carry over — mark it done and never look again.
    if crate::db::db_path().exists() && crate::db::open_rw().is_err() {
        return; // leave the marker off so a transient failure retries
    }
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = File::create(&marker);
}

/// Drain the queue into batched appends until the sender is dropped.
fn writer_loop(rx: std::sync::mpsc::Receiver<CacheWrite>) {
    /// Bounds how much one append holds when a long walk produces rows faster
    /// than they can be written.
    const BATCH: usize = 512;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while batch.len() < BATCH {
            match rx.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        // Group by cache so each file is opened and locked once per batch.
        let mut per_cache: Vec<(&'static Cache, Vec<(Vec<u8>, Vec<u8>)>)> = Vec::new();
        for job in batch {
            let cache = job.cache();
            let rows = job.rows();
            match per_cache.iter_mut().find(|(c, _)| std::ptr::eq(*c, cache)) {
                Some((_, acc)) => acc.extend(rows),
                None => per_cache.push((cache, rows)),
            }
        }
        for (cache, rows) in per_cache {
            append(cache, &rows);
        }
    }
}

/// An `flock`ed file, unlocked when dropped.
///
/// Writers serialize on this: appends must not interleave, and only one process
/// may rewrite the base image at a time. Readers never take it.
struct Lock(File);

impl Lock {
    fn acquire(path: &Path) -> Option<Lock> {
        let file = OpenOptions::new().create(true).write(true).truncate(false).open(path).ok()?;
        // SAFETY: `file` owns the descriptor for the whole call and outlives it.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        (rc == 0).then_some(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: same descriptor, still owned by `self`.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Append a batch to the journal, then compact if it has grown past the bound.
///
/// Nothing here reports an error: the value being recorded is already computed
/// and already on its way to the user, so a cache that cannot be written costs a
/// recomputation next time and nothing more.
fn append(cache: &Cache, rows: &[(Vec<u8>, Vec<u8>)]) {
    if rows.is_empty() {
        return;
    }
    let Some(_lock) = Lock::acquire(&cache.lock) else { return };
    let mut buf = Vec::new();
    for (key, val) in rows {
        let (Ok(klen), Ok(vlen)) = (u32::try_from(key.len()), u32::try_from(val.len())) else {
            continue; // a key or value past 4 GiB is not something to record
        };
        buf.extend_from_slice(&klen.to_le_bytes());
        buf.extend_from_slice(&vlen.to_le_bytes());
        buf.extend_from_slice(key);
        buf.extend_from_slice(val);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&cache.journal) else {
        return;
    };
    // One write for the whole batch: a partial write is a torn tail, which the
    // reader stops at, so keeping batches atomic keeps them readable.
    if file.write_all(&buf).is_err() {
        return;
    }
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len >= COMPACT_BYTES {
        compact(cache);
    }
}

/// Fold the journal into a new base image. The caller must hold the lock.
fn compact(cache: &Cache) {
    // Start from what the current image holds, then let the journal overwrite it:
    // a journal record is by definition newer than the image it was appended to.
    let mut merged: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let existing = File::open(&cache.base).ok().and_then(|f| {
        // SAFETY: the image is only ever replaced by rename, so these bytes are
        // stable for as long as the mapping lives (this function).
        unsafe { Mmap::map(&f).ok() }
    });
    if let Some(map) = &existing {
        if let Some(table) = archived(map) {
            let heap: &[u8] = &table.heap;
            for e in table.index.iter() {
                if let (Some(k), Some(v)) =
                    (slice_at(heap, e.key_off, e.key_len), slice_at(heap, e.val_off, e.val_len))
                {
                    merged.insert(k.to_vec(), v.to_vec());
                }
            }
        }
    }
    for (k, v) in read_journal(&cache.journal) {
        merged.insert(k.into_vec(), v.into_vec());
    }

    let mut rows: Vec<(u64, Vec<u8>, Vec<u8>)> =
        merged.into_iter().map(|(k, v)| (hash_key(&k), k, v)).collect();
    // Sorted by hash first so `get` can binary-search it, then by key so the
    // order of equal-hash entries is stable across rewrites.
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut index = Vec::with_capacity(rows.len());
    let mut heap = Vec::new();
    for (hash, key, val) in &rows {
        let (Ok(key_off), Ok(key_len)) = (u32::try_from(heap.len()), u32::try_from(key.len()))
        else {
            return; // heap past 4 GiB: leave the image alone rather than truncate it
        };
        heap.extend_from_slice(key);
        let (Ok(val_off), Ok(val_len)) = (u32::try_from(heap.len()), u32::try_from(val.len()))
        else {
            return;
        };
        heap.extend_from_slice(val);
        index.push(Entry { hash: *hash, key_off, key_len, val_off, val_len });
    }

    let table = Table {
        header: Header {
            magic: MAGIC,
            format_version: FORMAT_VERSION,
            pointer_width: usize::BITS,
            entries: u32::try_from(index.len()).unwrap_or(u32::MAX),
            built_at: now(),
        },
        index,
        heap,
    };
    let Ok(bytes) = rkyv::to_bytes::<_, 4096>(&table) else { return };

    // Rename is atomic, so a reader maps either the whole old image or the whole
    // new one. Writing in place would let one map a half-written file.
    let tmp = cache.base.with_extension(format!("tmp.{}", std::process::id()));
    let written = File::create(&tmp).and_then(|mut f| {
        f.write_all(&bytes)?;
        f.sync_all()
    });
    if written.is_err() || std::fs::rename(&tmp, &cache.base).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Only now is the journal redundant. Truncating before the rename landed
    // would drop entries that the new image does not yet contain.
    if let Ok(f) = OpenOptions::new().write(true).open(&cache.journal) {
        let _ = f.set_len(0);
        let _ = f.sync_all();
    }
}

/// Take a known-in-bulk set of rows and fold it straight into the base images.
///
/// This is the one-time import of what the SQLite ledger accumulated before
/// these caches moved here. It goes through [`CacheWrite`] like every other write
/// so there is only one encoding of a key, and compacts once per cache at the end
/// rather than leaving megabytes in a journal for the next reader to scan.
pub fn import(jobs: Vec<CacheWrite>) {
    let mut per_cache: Vec<(&'static Cache, Vec<(Vec<u8>, Vec<u8>)>)> = Vec::new();
    for job in jobs {
        let cache = job.cache();
        let rows = job.rows();
        match per_cache.iter_mut().find(|(c, _)| std::ptr::eq(*c, cache)) {
            Some((_, acc)) => acc.extend(rows),
            None => per_cache.push((cache, rows)),
        }
    }
    for (cache, rows) in per_cache {
        if rows.is_empty() {
            continue;
        }
        append(cache, &rows);
        // `append` only compacts once the journal crosses the threshold; an
        // import is a one-off, so fold it in regardless of how much it was.
        if let Some(_lock) = Lock::acquire(&cache.lock) {
            compact(cache);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zvcs-rcache-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn journal_reader_stops_at_a_torn_tail() {
        let dir = temp_dir("torn");
        let path = dir.join("t.log");
        let mut buf = Vec::new();
        for (k, v) in [(&b"alpha"[..], &b"one"[..]), (&b"beta"[..], &b"two"[..])] {
            buf.extend_from_slice(&(k.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
            buf.extend_from_slice(k);
            buf.extend_from_slice(v);
        }
        // A third record whose payload never made it to disk.
        buf.extend_from_slice(&9u32.to_le_bytes());
        buf.extend_from_slice(&9u32.to_le_bytes());
        buf.extend_from_slice(b"gam");
        std::fs::write(&path, &buf).unwrap();

        let got = read_journal(&path);
        assert_eq!(got.len(), 2, "the torn record must not be read");
        assert_eq!(got.get(&b"alpha"[..]).map(|v| &v[..]), Some(&b"one"[..]));
        assert_eq!(got.get(&b"beta"[..]).map(|v| &v[..]), Some(&b"two"[..]));
    }

    #[test]
    fn a_later_journal_record_supersedes_an_earlier_one() {
        let dir = temp_dir("supersede");
        let path = dir.join("t.log");
        let mut buf = Vec::new();
        for v in [&b"old"[..], &b"new"[..]] {
            buf.extend_from_slice(&3u32.to_le_bytes());
            buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
            buf.extend_from_slice(b"key");
            buf.extend_from_slice(v);
        }
        std::fs::write(&path, &buf).unwrap();
        assert_eq!(read_journal(&path).get(&b"key"[..]).map(|v| &v[..]), Some(&b"new"[..]));
    }

    #[test]
    fn hash_collisions_do_not_serve_the_wrong_value() {
        // Two keys forced into one hash bucket by construction: the index is
        // searched by hash, so only the full-key compare separates them.
        let a = b"first-key".to_vec();
        let b = b"second-key".to_vec();
        let rows = vec![(a.clone(), b"A".to_vec()), (b.clone(), b"B".to_vec())];
        let mut index = Vec::new();
        let mut heap = Vec::new();
        for (key, val) in &rows {
            let key_off = heap.len() as u32;
            heap.extend_from_slice(key);
            let val_off = heap.len() as u32;
            heap.extend_from_slice(val);
            index.push(Entry {
                hash: 42, // same bucket for both
                key_off,
                key_len: key.len() as u32,
                val_off,
                val_len: val.len() as u32,
            });
        }
        let table = Table {
            header: Header {
                magic: MAGIC,
                format_version: FORMAT_VERSION,
                pointer_width: usize::BITS,
                entries: 2,
                built_at: 0,
            },
            index,
            heap,
        };
        let bytes = rkyv::to_bytes::<_, 4096>(&table).unwrap();
        let archived = archived(&bytes).expect("image must validate");
        let cache = Cache {
            base: PathBuf::new(),
            journal: PathBuf::new(),
            lock: PathBuf::new(),
            table: OnceLock::new(),
            recent: OnceLock::new(),
        };
        // The reference is confined to this test, which owns `bytes` for its
        // whole body; the store itself only ever holds one from a leaked mapping.
        let _ = cache.table.set(Some(unsafe {
            std::mem::transmute::<&ArchivedTable, &'static ArchivedTable>(archived)
        }));
        let _ = cache.recent.set(HashMap::new());
        // Both keys sit in bucket 42, so only the full-key compare can tell them
        // apart — which is the guarantee that lets the index be searched by hash.
        assert_eq!(cache.find(&a, 42), Some(&b"A"[..]));
        assert_eq!(cache.find(&b, 42), Some(&b"B"[..]));
        assert_eq!(cache.find(b"absent", 42), None, "a colliding miss must stay a miss");
    }

    #[test]
    fn round_trips_through_journal_and_compaction() {
        let dir = temp_dir("roundtrip");
        let cache = Cache::in_dir(&dir, "rt");
        let rows: Vec<(Vec<u8>, Vec<u8>)> = (0..2000u32)
            .map(|i| (format!("key{i}").into_bytes(), format!("val{i}").into_bytes()))
            .collect();
        append(&cache, &rows);
        // Re-open: whether the batch compacted or stayed in the journal, every row
        // must read back through the same lookup path.
        let reopened = Cache::in_dir(&dir, "rt");
        assert_eq!(reopened.base_len() + reopened.journal_len(), 2000);
        for (k, v) in &rows {
            assert_eq!(reopened.get(k), Some(&v[..]), "missing {:?}", String::from_utf8_lossy(k));
        }
    }

    #[test]
    fn compaction_keeps_every_entry_and_empties_the_journal() {
        let dir = temp_dir("compact");
        let cache = Cache::in_dir(&dir, "cp");
        let rows: Vec<(Vec<u8>, Vec<u8>)> =
            (0..500u32).map(|i| (format!("k{i}").into_bytes(), vec![b'x'; 64])).collect();
        append(&cache, &rows);
        compact(&cache);
        let journal_len = std::fs::metadata(&cache.journal).map(|m| m.len()).unwrap_or(0);
        assert_eq!(journal_len, 0, "compaction must truncate the journal");
        let reopened = Cache::in_dir(&dir, "cp");
        assert_eq!(reopened.base_len(), 500);
        assert_eq!(reopened.journal_len(), 0);
        assert_eq!(reopened.get(b"k499"), Some(&vec![b'x'; 64][..]));
    }

    #[test]
    fn a_journal_written_after_compaction_still_reads_back() {
        // The two halves of a lookup have to compose: what the image holds and
        // what has been appended since must both be visible, with the newer
        // journal value winning where they overlap.
        let dir = temp_dir("layered");
        let cache = Cache::in_dir(&dir, "ly");
        append(&cache, &[(b"old".to_vec(), b"1".to_vec()), (b"both".to_vec(), b"image".to_vec())]);
        compact(&cache);
        append(&cache, &[(b"new".to_vec(), b"2".to_vec()), (b"both".to_vec(), b"journal".to_vec())]);

        let reopened = Cache::in_dir(&dir, "ly");
        assert_eq!(reopened.base_len(), 2, "the image must survive the later append");
        assert_eq!(reopened.get(b"old"), Some(&b"1"[..]));
        assert_eq!(reopened.get(b"new"), Some(&b"2"[..]));
        assert_eq!(reopened.get(b"both"), Some(&b"journal"[..]), "the journal must win");
    }

    #[test]
    fn concurrent_writers_lose_nothing_across_a_compaction() {
        // Sixteen shells run against this cache at once. Writers serialize on the
        // lock, and each batch is large enough that compactions fire mid-run, so
        // this covers the case the lock exists for: one writer rewriting the image
        // while others are appending to the journal it is about to truncate.
        let dir = temp_dir("concurrent");
        const WRITERS: u32 = 8;
        const PER_WRITER: u32 = 400;
        std::thread::scope(|scope| {
            for w in 0..WRITERS {
                let dir = dir.clone();
                scope.spawn(move || {
                    let cache = Cache::in_dir(&dir, "cc");
                    let rows: Vec<(Vec<u8>, Vec<u8>)> = (0..PER_WRITER)
                        .map(|i| (format!("w{w}-k{i}").into_bytes(), vec![b'v'; 512]))
                        .collect();
                    // Several batches per writer, so appends interleave rather
                    // than each writer holding the lock once.
                    for chunk in rows.chunks(50) {
                        append(&cache, chunk);
                    }
                });
            }
        });

        let cache = Cache::in_dir(&dir, "cc");
        assert_eq!(
            cache.base_len() + cache.journal_len(),
            (WRITERS * PER_WRITER) as usize,
            "no row may be lost to a compaction that ran under an append"
        );
        for w in 0..WRITERS {
            for i in 0..PER_WRITER {
                let key = format!("w{w}-k{i}").into_bytes();
                assert_eq!(cache.get(&key), Some(&vec![b'v'; 512][..]), "lost w{w}-k{i}");
            }
        }
    }

    #[test]
    fn a_corrupt_image_reads_as_empty_rather_than_wrong() {
        let dir = temp_dir("corrupt");
        let cache = Cache::in_dir(&dir, "bad");
        append(&cache, &[(b"k".to_vec(), b"v".to_vec())]);
        compact(&cache);
        // Truncate the image mid-archive: validation must reject it.
        let bytes = std::fs::read(&cache.base).unwrap();
        std::fs::write(&cache.base, &bytes[..bytes.len() / 2]).unwrap();
        assert!(map_base(&cache.base).is_none(), "a truncated image must not validate");
    }

    #[test]
    fn an_image_from_another_format_version_is_rejected() {
        // The header is what stops a layout change from being read as if it were
        // the current one, so flipping the version must make the image invisible.
        let table = Table {
            header: Header {
                magic: MAGIC,
                format_version: FORMAT_VERSION + 1,
                pointer_width: usize::BITS,
                entries: 0,
                built_at: 0,
            },
            index: Vec::new(),
            heap: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<_, 4096>(&table).unwrap();
        assert!(archived(&bytes).is_none(), "a future format version must not be read");
    }

    #[test]
    fn keys_stay_distinct_across_their_parts() {
        // The separator is what keeps a key from being read two ways.
        assert_ne!(treediff_key("ab", "cd", false), treediff_key("abc", "d", false));
        assert_ne!(treediff_key("ab", "cd", false), treediff_key("ab", "cd", true));
        assert_ne!(blame_key("a", "b/c", "x"), blame_key("a/b", "c", "x"));
        let mut one = [0u8; ABBREV_KEY_MAX];
        let mut two = [0u8; ABBREV_KEY_MAX];
        let n1 = abbrev_key_into(&mut one, &[0xaa; 20], 7).unwrap();
        let n2 = abbrev_key_into(&mut two, &[0xaa; 20], 8).unwrap();
        assert_ne!(one[..n1], two[..n2], "hex length must be part of the key");
    }

    /// A key carries [`ABBREV_KEY_VERSION`], so a row written under the previous
    /// meaning of `hex_len` cannot be found by a lookup made under this one. That
    /// is what makes an already-poisoned `abbrev` cache recover rather than keep
    /// serving 40 characters where `auto` asked for seven.
    #[test]
    fn an_abbrev_key_from_the_retired_scheme_can_never_be_found_again() {
        let oid = [0xbb; 20];
        let mut buf = [0u8; ABBREV_KEY_MAX];
        let n = abbrev_key_into(&mut buf, &oid, 7).unwrap();
        assert_eq!(n, oid.len() + 2, "id, hex length, key version");
        assert_eq!(buf[n - 1], ABBREV_KEY_VERSION);

        // Version 1's key was the id and the length byte, and nothing else.
        let mut retired = oid.to_vec();
        retired.push(7);
        assert_ne!(&buf[..n], retired.as_slice(), "a v1 key must not still match");

        // The collision the version retires: `auto` and `core.abbrev=no` both keyed
        // as 0 under v1, so one wrote the other's answer. Under v2 they resolve to
        // different widths and therefore to different keys.
        let mut auto = [0u8; ABBREV_KEY_MAX];
        let mut full = [0u8; ABBREV_KEY_MAX];
        let a = abbrev_key_into(&mut auto, &oid, 7).unwrap();
        let f = abbrev_key_into(&mut full, &oid, 40).unwrap();
        assert_ne!(auto[..a], full[..f]);
    }
}
