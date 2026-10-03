//! `stack.c` (with the file primitives of `system.c`): a mutable sequence of
//! tables in one directory, listed oldest first in `tables.list`.
//!
//! Writers lock `tables.list`, write a new table under a temporary name,
//! rename it into place and rewrite the list. After each addition the stack is
//! compacted so that table sizes keep a geometric progression.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crate::{
    Error, HashId, Result,
    basics::parse_names,
    block::header_size,
    blocksource::BlockSource,
    iter::Iterator,
    merged::MergedTable,
    record::{LogRecord, RefRecord},
    table::Table,
    writer::{Sink, WriteOptions, Writer},
};

/// `struct reftable_stack_options` (`reftable-stack.h:29-48`): options related
/// to opening a stack. The options for writing to it are passed to each
/// operation that writes, as [`WriteOptions`].
#[derive(Clone, Default)]
pub struct StackOptions {
    /// The hash of the object IDs in the tables.
    pub hash_id: HashId,
    /// Called whenever the stack is being reloaded, to discard cached
    /// information that relies on the old stack's data. C's `on_reload` with
    /// its `on_reload_payload` captured by the closure.
    pub on_reload: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Hide deletion records from iterators over the merged stack.
    pub suppress_deletions: bool,
}

impl std::fmt::Debug for StackOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackOptions")
            .field("hash_id", &self.hash_id)
            .field("on_reload", &self.on_reload.is_some())
            .field("suppress_deletions", &self.suppress_deletions)
            .finish()
    }
}

/// C's `st->list_lock` (`stack.h:20-24`): the lock on `tables.list` that an
/// [`Addition`] holds, kept with the stack so that [`Stack::reload()`] can tell
/// that the stack is locked. Shared with the addition, which releases it when
/// it is committed or dropped.
type ListLock = Arc<Mutex<Option<gix_lock::File>>>;

fn lock_list(l: &ListLock) -> MutexGuard<'_, Option<gix_lock::File>> {
    l.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `struct reftable_log_expiry_config`: which reflog entries compaction drops.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogExpiryConfig {
    /// Drop entries older than this timestamp.
    pub time: u64,
    /// Drop entries with a lower update index.
    pub min_update_index: u64,
}

/// `struct reftable_compaction_stats`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompactionStats {
    /// Total number of bytes written.
    pub bytes: u64,
    /// Total number of entries written, including failures.
    pub entries_written: u64,
    /// How often compaction was attempted.
    pub attempts: usize,
    /// How often it failed on a concurrent update.
    pub failures: usize,
}

/// The open `tables.list` of the last reload and the identity of the file it
/// was, C's `list_fd` and `list_st`. Keeping the file open keeps its inode
/// number from being recycled (`stack.c:463-492`).
struct ListHandle {
    _file: std::fs::File,
    dev: u64,
    ino: u64,
}

/// `struct reftable_stack`.
pub struct Stack {
    list_file: PathBuf,
    list_handle: Option<ListHandle>,
    list_lock: ListLock,
    reftable_dir: PathBuf,
    opts: StackOptions,
    /// The loaded tables; C's `st->tables` and `st->merged` share them.
    merged: MergedTable,
    stats: CompactionStats,
}

/// `read_lines()` (`stack.c:111-128`): the names in `filename`, none if it
/// does not exist.
fn read_lines(filename: &Path) -> Result<Vec<String>> {
    match std::fs::read(filename) {
        Ok(buf) => parse_names(&buf),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(Error::Io),
    }
}

/// `fd_read_lines()` (`stack.c:68-109`).
fn fd_read_lines(file: &mut std::fs::File) -> Result<Vec<String>> {
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|_| Error::Io)?;
    parse_names(&buf)
}

/// `reftable_rand()`.
fn rand_u32() -> u32 {
    gix_utils::rng::usize(0..=u32::MAX as usize) as u32
}

/// `format_name()` (`stack.c:756-764`): `0x<min>-0x<max>-<random>`.
fn format_name(min: u64, max: u64) -> String {
    format!("0x{min:012x}-0x{max:012x}-{:08x}", rand_u32())
}

#[cfg(unix)]
fn file_identity(md: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn file_identity(_md: &std::fs::Metadata) -> (u64, u64) {
    // Without inode numbers the stat cache cannot tell files apart; the
    // secondary check of comparing the list's contents is used instead.
    (0, 0)
}

fn set_permissions(path: &Path, mode: Option<u32>) -> Result<()> {
    let Some(mode) = mode else { return Ok(()) };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|_| Error::Io)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// `flock_acquire()` (`system.c:61-85`): lock `target` by creating
/// `<target>.lock`, waiting up to `timeout_ms` (negative: indefinitely).
fn flock_acquire(target: &Path, timeout_ms: i64) -> Result<gix_lock::File> {
    let mode = match timeout_ms {
        0 => gix_lock::acquire::Fail::Immediately,
        t if t < 0 => gix_lock::acquire::Fail::AfterDurationWithBackoff(Duration::from_secs(u64::from(u32::MAX))),
        t => gix_lock::acquire::Fail::AfterDurationWithBackoff(Duration::from_millis(t as u64)),
    };
    gix_lock::File::acquire_to_update_resource(target, mode, None).map_err(|err| match err {
        gix_lock::acquire::Error::PermanentlyLocked { .. } => Error::Lock,
        gix_lock::acquire::Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Error::Lock,
        gix_lock::acquire::Error::Io(_) => Error::Io,
    })
}

/// `tmpfile_from_pattern()` (`system.c:16-29`) for a `…XXXXXX` pattern: git's
/// `mks_tempfile()` replaces the six `X` with random alphanumerics.
fn tmpfile_from_pattern(prefix: &Path) -> Result<gix_tempfile::Handle<gix_tempfile::handle::Writable>> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for _ in 0..100 {
        let suffix: String = (0..6)
            .map(|_| LETTERS[gix_utils::rng::usize(0..LETTERS.len())] as char)
            .collect();
        let mut path = prefix.as_os_str().to_owned();
        path.push(suffix);
        match gix_tempfile::writable_at(
            PathBuf::from(path),
            gix_tempfile::ContainingDirectory::Exists,
            gix_tempfile::AutoRemove::Tempfile,
        ) {
            Ok(handle) => return Ok(handle),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(Error::Io),
        }
    }
    Err(Error::Io)
}

/// A temporary table file being written: C's `struct fd_writer` over a
/// `struct reftable_tmpfile`.
pub struct TableFile {
    handle: gix_tempfile::Handle<gix_tempfile::handle::Writable>,
    fsync: bool,
}

impl Sink for TableFile {
    /// `fd_writer_write()` (`stack.c:56-60`).
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.handle
            .with_mut(|f| f.write_all(data))
            .and_then(|r| r)
            .map_err(|_| Error::Io)
    }

    /// `fd_writer_flush()` (`stack.c:62-66`).
    fn flush(&mut self) -> Result<()> {
        if !self.fsync {
            return Ok(());
        }
        self.handle
            .with_mut(|f| f.as_file().sync_all())
            .and_then(|r| r)
            .map_err(|_| Error::Io)
    }
}

/// Create the temporary file for a table covering `min..=max`, named after it.
fn new_table_file(dir: &Path, min: u64, max: u64, opts: &WriteOptions) -> Result<TableFile> {
    let prefix = dir.join(format!("{}.temp.", format_name(min, max)));
    let mut handle = tmpfile_from_pattern(&prefix)?;
    if opts.default_permissions.is_some() {
        let path = handle
            .with_mut(|f| f.path().to_owned())
            .map_err(|_| Error::Io)?;
        set_permissions(&path, opts.default_permissions)?;
    }
    Ok(TableFile {
        handle,
        fsync: opts.fsync,
    })
}

/// Write `buf` into a held lock, `fsync()` it if asked, and rename it into place.
fn commit_lock(mut lock: gix_lock::File, buf: &[u8], fsync: bool) -> Result<()> {
    lock.with_mut(|f| f.write_all(buf)).map_err(|_| Error::Io)?;
    if fsync {
        lock.with_mut(|f| f.sync_all()).map_err(|_| Error::Io)?;
    }
    lock.commit().map_err(|_| Error::Io)?;
    Ok(())
}

impl Stack {
    /// `reftable_new_stack()` (`stack.c:504-552`): open the stack in `dir`,
    /// which must exist; a missing `tables.list` is an empty stack.
    pub fn new(dir: &Path, opts: &StackOptions) -> Result<Self> {
        let mut st = Stack {
            list_file: dir.join("tables.list"),
            list_handle: None,
            list_lock: Arc::new(Mutex::new(None)),
            reftable_dir: dir.to_owned(),
            opts: opts.clone(),
            merged: MergedTable::new(Vec::new(), opts.hash_id)?,
            stats: CompactionStats::default(),
        };
        st.reload_maybe_reuse(true)?;
        Ok(st)
    }

    /// The directory holding the tables.
    pub fn dir(&self) -> &Path {
        &self.reftable_dir
    }

    /// The options this stack was opened with.
    pub fn options(&self) -> &StackOptions {
        &self.opts
    }

    /// `reftable_stack_merged_table()`: valid until the next reload or write.
    pub fn merged_table(&self) -> &MergedTable {
        &self.merged
    }

    /// The loaded tables, oldest first.
    pub fn tables(&self) -> &[Arc<Table>] {
        &self.merged.tables
    }

    /// `reftable_stack_hash_id()`.
    pub fn hash_id(&self) -> HashId {
        self.merged.hash_id()
    }

    /// `reftable_stack_compaction_stats()`.
    pub fn compaction_stats(&self) -> &CompactionStats {
        &self.stats
    }

    /// `reftable_stack_init_ref_iterator()`.
    pub fn ref_iterator(&self) -> Result<Iterator> {
        self.merged.ref_iterator()
    }

    /// `reftable_stack_init_log_iterator()`.
    pub fn log_iterator(&self) -> Result<Iterator> {
        self.merged.log_iterator()
    }

    fn table_path(&self, name: &str) -> PathBuf {
        self.reftable_dir.join(name)
    }

    /// `reftable_stack_reload_once()` (`stack.c:227-367`): open the tables in
    /// `names`, reusing the already open ones of the same name if `reuse_open`.
    fn reload_once(&mut self, names: &[String], reuse_open: bool) -> Result<()> {
        let mut cur: Vec<Option<Arc<Table>>> = self.merged.tables.iter().cloned().map(Some).collect();
        let mut new_tables = Vec::with_capacity(names.len());

        for name in names {
            // Linear, but compaction keeps the number of tables small.
            let reused = reuse_open
                .then(|| cur.iter_mut().find(|t| t.as_ref().is_some_and(|t| t.name() == name)))
                .flatten()
                .and_then(Option::take);
            let table = match reused {
                Some(t) => t,
                None => Table::new(BlockSource::from_file(&self.table_path(name))?, name)?,
            };
            new_tables.push(table);
        }

        let mut new_merged = MergedTable::new(new_tables, self.opts.hash_id)?;

        // Close the old, non-reused tables and proactively try to unlink them,
        // for systems where a compacting process could not while they were open.
        for t in cur.into_iter().flatten() {
            let path = self.table_path(t.name());
            drop(t);
            let _ = std::fs::remove_file(path);
        }

        new_merged.suppress_deletions = self.opts.suppress_deletions;
        self.merged = new_merged;
        Ok(())
    }

    /// `reftable_stack_reload_maybe_reuse()` (`stack.c:369-502`): reload from
    /// `tables.list`, retrying for up to three seconds while a concurrent
    /// writer replaces tables under us.
    fn reload_maybe_reuse(&mut self, reuse_open: bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(3000);
        let mut delay: u64 = 0;
        let mut tries = 0;
        let mut opened: Option<std::fs::File>;

        let res = loop {
            // Only look at the deadline after the first few tries.
            opened = None;
            tries += 1;
            if tries > 3 && Instant::now() >= deadline {
                // C leaves `err` at the `0` of the last `read_lines()` here.
                break Ok(());
            }

            let names = match std::fs::File::open(&self.list_file) {
                Ok(mut f) => {
                    let names = match fd_read_lines(&mut f) {
                        Ok(n) => n,
                        Err(e) => break Err(e),
                    };
                    opened = Some(f);
                    names
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => break Err(Error::Io),
            };

            match self.reload_once(&names, reuse_open) {
                Ok(()) => break Ok(()),
                Err(Error::NotExist) => {}
                Err(e) => break Err(e),
            }

            // A missing table can be caused by a concurrent writer; there was
            // one if the list changed.
            let names_after = match read_lines(&self.list_file) {
                Ok(n) => n,
                Err(e) => break Err(e),
            };
            if names_after == names {
                break Err(Error::NotExist);
            }

            delay = delay + (delay * u64::from(rand_u32())) / u64::from(u32::MAX) + 1;
            std::thread::sleep(Duration::from_millis(delay));
        };

        // Invalidate the stat cache, then keep the list open if it identifies
        // a file, so its inode cannot be recycled while we cache it.
        self.list_handle = None;
        if res.is_ok() {
            if let Some(file) = opened {
                if let Ok(md) = file.metadata() {
                    let (dev, ino) = file_identity(&md);
                    if dev != 0 && ino != 0 {
                        self.list_handle = Some(ListHandle { _file: file, dev, ino });
                    }
                }
            }
        }
        if let Some(on_reload) = &self.opts.on_reload {
            on_reload();
        }
        res
    }

    /// `stack_uptodate()` (`stack.c:554-629`): `Ok(false)` if the stack in
    /// memory matches `tables.list`. With `skip_if_locked`, a stack an
    /// [`Addition`] holds locked counts as up to date, so that a reload does not
    /// change it under the addition; right after taking the lock the check must
    /// not be skipped, to notice concurrent updates.
    fn is_outdated(&self, skip_if_locked: bool) -> Result<bool> {
        if skip_if_locked && lock_list(&self.list_lock).is_some() {
            return Ok(false);
        }

        // Cached stat information tells whether the file was rewritten; it is
        // only ever replaced by rename, never written in place.
        if let Some(h) = &self.list_handle {
            match std::fs::metadata(&self.list_file) {
                Ok(md) => {
                    if file_identity(&md) == (h.dev, h.ino) {
                        return Ok(false);
                    }
                }
                // A missing "tables.list" is fine; the stack is outdated only if
                // it has tables loaded.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(!self.merged.tables.is_empty());
                }
                Err(_) => return Err(Error::Io),
            }
        }

        let names = read_lines(&self.list_file)?;
        if names.len() != self.merged.tables.len() {
            return Ok(true);
        }
        Ok(self.merged.tables.iter().zip(&names).any(|(t, n)| t.name() != n))
    }

    /// `reftable_stack_reload()` (`stack.c:631-637`): reload if `tables.list`
    /// changed, unless an [`Addition`] holds the stack locked.
    pub fn reload(&mut self) -> Result<()> {
        if self.is_outdated(true)? {
            self.reload_maybe_reuse(true)?;
        }
        Ok(())
    }

    /// `reftable_stack_next_update_index()` (`stack.c:973-980`).
    pub fn next_update_index(&self) -> u64 {
        self.merged.tables.last().map_or(1, |t| t.max_update_index() + 1)
    }

    /// `reftable_stack_addition_new()` / `reftable_stack_init_addition()`
    /// (`stack.c:850-866`, `677-718`): lock the stack for adding tables, written
    /// with `opts` (`None`: the defaults). A stack that is out of date once
    /// locked is reloaded first.
    pub fn addition_new(&mut self, opts: Option<&WriteOptions>) -> Result<Addition> {
        let opts = opts.cloned().unwrap_or_default();
        let lock = flock_acquire(&self.list_file, opts.lock_timeout_ms)?;
        let lock_path = lock.lock_path().to_owned();
        *lock_list(&self.list_lock) = Some(lock);
        // From here on, dropping `add` releases the lock (`reftable_addition_close()`).
        let mut add = Addition {
            list_lock: Arc::clone(&self.list_lock),
            locked: true,
            opts,
            reftable_dir: self.reftable_dir.clone(),
            new_tables: Vec::new(),
            next_update_index: 0,
        };
        set_permissions(&lock_path, add.opts.default_permissions)?;

        if self.is_outdated(false)? {
            self.reload_maybe_reuse(true)?;
        }
        add.next_update_index = self.next_update_index();
        Ok(add)
    }

    /// `reftable_stack_add()` / `stack_try_add()` (`stack.c:720-754`): add one
    /// table written by `write_table`, which must set the writer's limits.
    pub fn add(
        &mut self,
        write_table: impl FnOnce(&mut Writer<TableFile>, &Stack) -> Result<()>,
        opts: Option<&WriteOptions>,
    ) -> Result<()> {
        let mut add = self.addition_new(opts)?;
        add.add(self, write_table)?;
        add.commit(self)
    }

    /// `stack_write_compact()` (`stack.c:982-1079`): merge tables
    /// `first..=last` into `wr`, dropping tombstones when compacting from the
    /// bottom of the stack, and expired log entries.
    fn write_compact<S: Sink>(
        &mut self,
        wr: &mut Writer<S>,
        first: usize,
        last: usize,
        config: Option<&LogExpiryConfig>,
    ) -> Result<()> {
        let tables = &self.merged.tables;
        for t in &tables[first..=last] {
            self.stats.bytes += t.size;
        }
        wr.set_limits(tables[first].min_update_index(), tables[last].max_update_index())?;

        let mt = MergedTable::new(tables[first..=last].to_vec(), self.opts.hash_id)?;
        let mut entries = 0;
        let res = (|| {
            let mut it = mt.ref_iterator()?;
            it.seek_ref(b"")?;
            let mut r = RefRecord::default();
            while it.next_ref(&mut r)? {
                if first == 0 && r.is_deletion() {
                    continue;
                }
                wr.add_ref(&r)?;
                entries += 1;
            }

            let mut it = mt.log_iterator()?;
            it.seek_log(b"")?;
            let mut log = LogRecord::default();
            while it.next_log(&mut log)? {
                if first == 0 && log.is_deletion() {
                    continue;
                }
                if let Some(config) = config {
                    if config.min_update_index > 0 && log.update_index < config.min_update_index {
                        continue;
                    }
                    if config.time > 0 && log.update().is_some_and(|u| u.time < config.time) {
                        continue;
                    }
                }
                wr.add_log(&log)?;
                entries += 1;
            }
            Ok(())
        })();
        self.stats.entries_written += entries;
        res
    }

    /// `stack_compact_locked()` (`stack.c:1081-1145`): write the compacted
    /// table into a closed temporary file.
    fn compact_locked(
        &mut self,
        first: usize,
        last: usize,
        config: Option<&LogExpiryConfig>,
        opts: &WriteOptions,
    ) -> Result<gix_tempfile::Handle<gix_tempfile::handle::Closed>> {
        let min = self.merged.tables[first].min_update_index();
        let max = self.merged.tables[last].max_update_index();
        let file = new_table_file(&self.reftable_dir, min, max, opts)?;
        let mut wr = Writer::new(file, self.opts.hash_id, opts)?;
        self.write_compact(&mut wr, first, last, config)?;
        wr.close()?;
        wr.into_sink().handle.close().map_err(|_| Error::Io)
    }

    /// `stack_compact_range()` (`stack.c:1166-1530`): compact tables
    /// `first..=last` into one. [`Error::Lock`] means part of the stack is
    /// locked by another process, which callers may ignore.
    fn compact_range(
        &mut self,
        mut first: usize,
        last: usize,
        expiry: Option<&LogExpiryConfig>,
        opts: &WriteOptions,
        best_effort: bool,
    ) -> Result<()> {
        if first > last || (expiry.is_none() && first == last) {
            return Ok(());
        }
        self.stats.attempts += 1;
        let res = self.compact_range_inner(&mut first, last, expiry, opts, best_effort);
        if res == Err(Error::Lock) {
            self.stats.failures += 1;
        }
        res
    }

    fn compact_range_inner(
        &mut self,
        first: &mut usize,
        last: usize,
        expiry: Option<&LogExpiryConfig>,
        opts: &WriteOptions,
        best_effort: bool,
    ) -> Result<()> {
        // Hold the list lock to read "tables.list" and lock the tables of the range.
        let tables_list_lock = flock_acquire(&self.list_file, opts.lock_timeout_ms)?;

        // The range the caller asked for may have changed if the stack is
        // outdated; rather than guessing, abort.
        if self.is_outdated(false)? {
            return Err(Error::Outdated);
        }

        // Lock the tables from last to first, so that a newer process can
        // compact the tables it added while an older one is still busy with
        // the ones before them.
        let mut table_locks: Vec<gix_lock::Marker> = Vec::new();
        let mut i = last + 1;
        while i > *first {
            let name = self.table_path(self.merged.tables[i - 1].name());
            match flock_acquire(&name, 0) {
                Ok(lock) => {
                    // Closed to avoid running out of file descriptors on
                    // large stacks.
                    table_locks.push(lock.close().map_err(|_| Error::Io)?);
                }
                Err(Error::Lock) if last - (i - 1) >= 2 && best_effort => {
                    // Compact the tables locked so far, those after this one.
                    *first = i;
                    break;
                }
                Err(e) => return Err(e),
            }
            i -= 1;
        }
        let first = *first;

        // With all tables of the range locked, concurrent updates of the
        // stack may proceed while we compact.
        drop(tables_list_lock);

        // Tombstones may cancel out every ref in the range, leaving no table.
        let new_table = match self.compact_locked(first, last, expiry, opts) {
            Ok(t) => Some(t),
            Err(Error::EmptyTable) => None,
            Err(e) => return Err(e),
        };

        // Re-lock "tables.list" to replace the compacted range with the new table.
        let tables_list_lock = flock_acquire(&self.list_file, opts.lock_timeout_ms)?;
        set_permissions(tables_list_lock.lock_path(), opts.default_permissions)?;

        // A concurrent process may have updated the stack while it was unlocked.
        // Continue only if the compacted tables are still in it, in order.
        let (names, first_to_replace, last_to_replace) = if self.is_outdated(false)? {
            let names = read_lines(&self.list_file)?;
            let first_name = self.merged.tables[first].name();
            let Some(new_offset) = names.iter().position(|n| n == first_name) else {
                return Err(Error::Outdated);
            };
            for j in 1..=(last - first) {
                let old = self.merged.tables.get(first + j).map(|t| t.name());
                let new = names.get(new_offset + j).map(String::as_str);
                if old.is_none() || old != new {
                    return Err(Error::Outdated);
                }
            }
            (names, new_offset, last + new_offset - first)
        } else {
            let names = self.merged.tables.iter().map(|t| t.name().to_owned()).collect();
            (names, first, last)
        };

        // Move the compacted table into place, unless it is empty.
        let mut new_table_name = None;
        let mut new_table_path = None;
        if let Some(new_table) = new_table {
            let name = format!(
                "{}.ref",
                format_name(
                    self.merged.tables[first].min_update_index(),
                    self.merged.tables[last].max_update_index()
                )
            );
            let path = self.table_path(&name);
            new_table.persist(&path).map_err(|_| Error::Io)?;
            new_table_name = Some(name);
            new_table_path = Some(path);
        }

        let mut list = String::new();
        for n in &names[..first_to_replace] {
            list.push_str(n);
            list.push('\n');
        }
        if let Some(n) = &new_table_name {
            list.push_str(n);
            list.push('\n');
        }
        for n in names.iter().skip(last_to_replace + 1) {
            list.push_str(n);
            list.push('\n');
        }

        if let Err(e) = commit_lock(tables_list_lock, list.as_bytes(), opts.fsync) {
            if let Some(p) = new_table_path {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }

        // Reload before deleting the compacted tables: on some systems they can
        // only be deleted once closed.
        self.reload_maybe_reuse(first < last)?;

        // Delete the old tables; concurrent readers may still use them, so
        // failures are expected.
        for lock in &table_locks {
            let _ = std::fs::remove_file(lock.resource_path());
        }
        Ok(())
    }

    /// `reftable_stack_compact_all()` (`stack.c:1532-1543`): compact the whole
    /// stack into one table written with `opts` (`None`: the defaults),
    /// expiring reflog entries per `config`.
    pub fn compact_all(&mut self, opts: Option<&WriteOptions>, config: Option<&LogExpiryConfig>) -> Result<()> {
        let opts = opts.cloned().unwrap_or_default();
        let last = self.merged.tables.len().saturating_sub(1);
        self.compact_range(0, last, config, &opts, false)
    }

    /// `stack_segments_for_compaction()` (`stack.c:1626-1646`).
    fn segments_for_compaction(&self, opts: &WriteOptions) -> Segment {
        let version = if self.opts.hash_id == HashId::Sha1 { 1 } else { 2 };
        let overhead = header_size(version) as u64 - 1;
        let sizes: Vec<u64> = self.merged.tables.iter().map(|t| t.size - overhead).collect();
        suggest_compaction_segment(&sizes, opts.auto_compaction_factor)
    }

    /// `update_segment_if_compaction_required()` (`stack.c:1648-1672`).
    fn compaction_segment(&self, opts: &WriteOptions, use_geometric: bool) -> (bool, Segment) {
        if self.merged.tables.len() < 2 {
            return (false, Segment::default());
        }
        if !use_geometric {
            return (true, Segment::default());
        }
        let seg = self.segments_for_compaction(opts);
        (seg.end > seg.start, seg)
    }

    /// `reftable_stack_compaction_required()` (`stack.c:1674-1687`): whether
    /// all tables could be compacted, or with `use_heuristics`, whether the
    /// geometric sequence `opts` asks for needs restoring.
    pub fn compaction_required(&self, opts: Option<&WriteOptions>, use_heuristics: bool) -> bool {
        let opts = opts.cloned().unwrap_or_default();
        self.compaction_segment(&opts, use_heuristics).0
    }

    /// `reftable_stack_auto_compact()` (`stack.c:1689-1711`).
    pub fn auto_compact(&mut self, opts: Option<&WriteOptions>) -> Result<()> {
        let opts = opts.cloned().unwrap_or_default();
        let (required, seg) = self.compaction_segment(&opts, true);
        if required {
            return self.compact_range(seg.start, seg.end - 1, None, &opts, true);
        }
        Ok(())
    }

    /// `reftable_stack_read_ref()` (`stack.c:1719-1747`): `Ok(None)` if absent.
    pub fn read_ref(&self, refname: &[u8]) -> Result<Option<RefRecord>> {
        let mut it = self.merged.ref_iterator()?;
        if !it.seek_ref(refname)? {
            return Ok(None);
        }
        let mut r = RefRecord::default();
        if !it.next_ref(&mut r)? || r.refname != refname || r.is_deletion() {
            return Ok(None);
        }
        Ok(Some(r))
    }

    /// `reftable_stack_read_log()` (`stack.c:1749-1779`): the newest log entry
    /// of `refname`, `Ok(None)` if there is none.
    pub fn read_log(&self, refname: &[u8]) -> Result<Option<LogRecord>> {
        let mut it = self.merged.log_iterator()?;
        if !it.seek_log(refname)? {
            return Ok(None);
        }
        let mut log = LogRecord::default();
        if !it.next_log(&mut log)? || log.refname != refname || log.is_deletion() {
            return Ok(None);
        }
        Ok(Some(log))
    }

    /// `reftable_stack_clean()` (`stack.c:1845-1858`): delete table files no
    /// longer in the stack whose updates it has already absorbed.
    pub fn clean(&mut self) -> Result<()> {
        // Taking the lock reloads an outdated stack.
        let _add = self.addition_new(None)?;
        self.clean_locked()
    }

    /// `reftable_stack_clean_locked()` (`stack.c:1818-1843`) with
    /// `remove_maybe_stale_table()` (`stack.c:1787-1816`).
    fn clean_locked(&self) -> Result<()> {
        let max = self.merged.max_update_index();
        let dir = std::fs::read_dir(&self.reftable_dir).map_err(|_| Error::Io)?;
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // `is_table_name()`: the part after the last dot is "ref".
            if name.rsplit_once('.').is_none_or(|(_, ext)| ext != "ref") {
                continue;
            }
            if self.merged.tables.iter().any(|t| t.name() == name) {
                continue;
            }
            let path = self.table_path(name);
            let Ok(source) = BlockSource::from_file(&path) else { continue };
            let Ok(table) = Table::new(source, name) else { continue };
            let update_idx = table.max_update_index();
            drop(table);
            if update_idx <= max {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }
}

/// `struct reftable_addition`: a transaction adding tables to a stack. It
/// holds the stack's `tables.list` lock until committed or dropped, and while
/// it does, [`Stack::reload()`] leaves the stack as it is.
pub struct Addition {
    /// The lock of the stack this addition was created for.
    list_lock: ListLock,
    /// Whether this addition is the one holding `list_lock`, so that it never
    /// releases the lock of another addition (`stack.c:643-648`).
    locked: bool,
    opts: WriteOptions,
    reftable_dir: PathBuf,
    new_tables: Vec<String>,
    next_update_index: u64,
}

impl Addition {
    /// The update index the tables of this addition start at.
    pub fn next_update_index(&self) -> u64 {
        self.next_update_index
    }

    /// The options the tables of this addition are written with.
    pub fn options(&self) -> &WriteOptions {
        &self.opts
    }

    /// `st` must be the stack this addition locked; C keeps a pointer to it.
    fn check_stack(&self, st: &Stack) -> Result<()> {
        if Arc::ptr_eq(&self.list_lock, &st.list_lock) {
            Ok(())
        } else {
            Err(Error::Api)
        }
    }

    /// `reftable_addition_add()` (`stack.c:869-971`): write one table with
    /// `write_table`, which must set the writer's limits to at least
    /// [`next_update_index()`](Self::next_update_index). A table without
    /// records is not added.
    pub fn add(
        &mut self,
        st: &Stack,
        write_table: impl FnOnce(&mut Writer<TableFile>, &Stack) -> Result<()>,
    ) -> Result<()> {
        self.check_stack(st)?;
        let file = new_table_file(&self.reftable_dir, self.next_update_index, self.next_update_index, &self.opts)?;
        let mut wr = Writer::new(file, st.opts.hash_id, &self.opts)?;
        write_table(&mut wr, st)?;
        match wr.close() {
            Err(Error::EmptyTable) => return Ok(()),
            res => res?,
        }
        let (min, max) = (wr.min_update_index(), wr.max_update_index());
        let tmp = wr.into_sink().handle.close().map_err(|_| Error::Io)?;
        if min < self.next_update_index {
            return Err(Error::Api);
        }

        // On Windows this relies on the random part to pick a unique name.
        let name = format!("{}.ref", format_name(min, max));
        tmp.persist(self.reftable_dir.join(&name)).map_err(|_| Error::Io)?;
        self.new_tables.push(name);
        Ok(())
    }

    /// `reftable_addition_commit()` (`stack.c:775-848`): rewrite `tables.list`
    /// with the new tables appended, reload, and auto-compact.
    pub fn commit(mut self, st: &mut Stack) -> Result<()> {
        self.check_stack(st)?;
        if self.new_tables.is_empty() {
            return Ok(());
        }
        let mut list = String::new();
        for t in &st.merged.tables {
            list.push_str(t.name());
            list.push('\n');
        }
        for n in &self.new_tables {
            list.push_str(n);
            list.push('\n');
        }
        // Committing or failing to, the lock is gone afterwards.
        let lock = lock_list(&self.list_lock).take().ok_or(Error::Api)?;
        self.locked = false;
        commit_lock(lock, list.as_bytes(), self.opts.fsync)?;

        // Success: the new tables belong to the stack now.
        self.new_tables.clear();

        st.reload_maybe_reuse(true)?;

        if !self.opts.disable_auto_compact {
            // A concurrent writer may be compacting part of the stack
            // (`Lock`), or have rewritten it (`Outdated`); both are benign.
            match st.auto_compact(Some(&self.opts)) {
                Ok(()) | Err(Error::Lock | Error::Outdated) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drop for Addition {
    /// `reftable_addition_close()` (`stack.c:655-675`): delete uncommitted
    /// tables and release the stack's lock if this addition holds it.
    fn drop(&mut self) {
        for name in &self.new_tables {
            let _ = std::fs::remove_file(self.reftable_dir.join(name));
        }
        if self.locked {
            // Dropping the lock file rolls it back.
            lock_list(&self.list_lock).take();
            self.locked = false;
        }
    }
}

/// `struct segment` (`stack.h`): a range `start..end` of tables to compact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Segment {
    /// First table of the segment.
    pub start: usize,
    /// One past the last table.
    pub end: usize,
    /// Total size of the segment.
    pub bytes: u64,
}

/// `suggest_compaction_segment()` (`stack.c:1550-1624`): the segment to
/// compact so that each table is at least `factor` times the size of the next.
pub fn suggest_compaction_segment(sizes: &[u64], factor: u8) -> Segment {
    let factor = u64::from(if factor == 0 { crate::DEFAULT_GEOMETRIC_FACTOR } else { factor });
    let mut seg = Segment::default();
    let n = sizes.len();

    // No or only one table is a geometric sequence already.
    if n <= 1 {
        return seg;
    }

    // Find the end of the segment: iterating from the newest table, the first
    // one whose predecessor is smaller than `factor` times its size. Tables
    // after it are valid members of the sequence already.
    let mut i = n - 1;
    let mut bytes = 0;
    while i > 0 {
        if sizes[i - 1] < sizes[i] * factor {
            seg.end = i + 1;
            bytes = sizes[i];
            break;
        }
        i -= 1;
    }

    // Find the start: keep accumulating sizes from the end, as the tables are
    // merged backwards recursively, and keep going past the first start found
    // since earlier tables may violate the sequence as well.
    while i > 0 {
        let curr = bytes;
        bytes += sizes[i - 1];
        if sizes[i - 1] < curr * factor {
            seg.start = i - 1;
            seg.bytes = bytes;
        }
        i -= 1;
    }
    seg
}

/// A stack is shared between threads behind a lock by its users (the ref
/// store keeps one per worktree), and an addition may outlive the borrow of
/// the stack it locked.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Stack>();
    assert_send_sync::<Addition>();
    assert_send_sync::<StackOptions>();
};
