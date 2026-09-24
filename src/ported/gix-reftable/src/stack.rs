//! `stack.c` (with the file primitives of `system.c`): a mutable sequence of
//! tables in one directory, listed oldest first in `tables.list`.
//!
//! Writers lock `tables.list`, write a new table under a temporary name,
//! rename it into place and rewrite the list. After each addition the stack is
//! compacted so that table sizes keep a geometric progression.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
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

/// `REFTABLE_STACK_NEW_ADDITION_RELOAD`: reload the stack when it is out of
/// date after locking it, instead of failing with [`Error::Outdated`].
pub const NEW_ADDITION_RELOAD: u32 = 1 << 0;

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
/// number from being recycled (`stack.c:450-485`).
struct ListHandle {
    _file: std::fs::File,
    dev: u64,
    ino: u64,
}

/// `struct reftable_stack`.
pub struct Stack {
    list_file: PathBuf,
    list_handle: Option<ListHandle>,
    reftable_dir: PathBuf,
    opts: WriteOptions,
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

/// `format_name()` (`stack.c:742-750`): `0x<min>-0x<max>-<random>`.
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
    /// `reftable_new_stack()` (`stack.c:503-549`): open the stack in `dir`,
    /// which must exist; a missing `tables.list` is an empty stack.
    pub fn new(dir: &Path, opts: &WriteOptions) -> Result<Self> {
        let mut st = Stack {
            list_file: dir.join("tables.list"),
            list_handle: None,
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

    /// The write options of this stack.
    pub fn options(&self) -> &WriteOptions {
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

    /// `reftable_stack_reload_once()` (`stack.c:226-366`): open the tables in
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

        new_merged.suppress_deletions = true;
        self.merged = new_merged;
        Ok(())
    }

    /// `reftable_stack_reload_maybe_reuse()` (`stack.c:368-501`): reload from
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
        res
    }

    /// `stack_uptodate()` (`stack.c:556-619`): `Ok(false)` if the stack in
    /// memory matches `tables.list`.
    fn is_outdated(&self) -> Result<bool> {
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

    /// `reftable_stack_reload()` (`stack.c:621-627`): reload if `tables.list` changed.
    pub fn reload(&mut self) -> Result<()> {
        if self.is_outdated()? {
            self.reload_maybe_reuse(true)?;
        }
        Ok(())
    }

    /// `reftable_stack_next_update_index()` (`stack.c:958-965`).
    pub fn next_update_index(&self) -> u64 {
        self.merged.tables.last().map_or(1, |t| t.max_update_index() + 1)
    }

    /// `reftable_stack_new_addition()` / `reftable_stack_init_addition()`
    /// (`stack.c:658-700`): lock the stack for adding tables. `flags` takes
    /// [`NEW_ADDITION_RELOAD`].
    pub fn new_addition(&mut self, flags: u32) -> Result<Addition> {
        let lock = flock_acquire(&self.list_file, self.opts.lock_timeout_ms)?;
        let mut add = Addition {
            lock: Some(lock),
            reftable_dir: self.reftable_dir.clone(),
            new_tables: Vec::new(),
            next_update_index: 0,
        };
        if let Some(lock) = &add.lock {
            set_permissions(lock.lock_path(), self.opts.default_permissions)?;
        }

        if self.is_outdated()? {
            if flags & NEW_ADDITION_RELOAD == 0 {
                return Err(Error::Outdated);
            }
            self.reload_maybe_reuse(true)?;
        }
        add.next_update_index = self.next_update_index();
        Ok(add)
    }

    /// `reftable_stack_add()` (`stack.c:702-740`): add one table written by
    /// `write_table`, which must set the writer's limits.
    pub fn add(
        &mut self,
        write_table: impl FnOnce(&mut Writer<TableFile>, &Stack) -> Result<()>,
        flags: u32,
    ) -> Result<()> {
        let res = (|| {
            let mut add = self.new_addition(flags)?;
            add.add(self, write_table)?;
            add.commit(self)
        })();
        if res == Err(Error::Outdated) {
            // The error to report is the outdated one.
            let _ = self.reload();
        }
        res
    }

    /// `stack_write_compact()` (`stack.c:967-1064`): merge tables
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

    /// `stack_compact_locked()` (`stack.c:1066-1130`): write the compacted
    /// table into a closed temporary file.
    fn compact_locked(
        &mut self,
        first: usize,
        last: usize,
        config: Option<&LogExpiryConfig>,
    ) -> Result<gix_tempfile::Handle<gix_tempfile::handle::Closed>> {
        let min = self.merged.tables[first].min_update_index();
        let max = self.merged.tables[last].max_update_index();
        let file = new_table_file(&self.reftable_dir, min, max, &self.opts)?;
        let mut wr = Writer::new(file, &self.opts)?;
        self.write_compact(&mut wr, first, last, config)?;
        wr.close()?;
        wr.into_sink().handle.close().map_err(|_| Error::Io)
    }

    /// `stack_compact_range()` (`stack.c:1150-1513`): compact tables
    /// `first..=last` into one. [`Error::Lock`] means part of the stack is
    /// locked by another process, which callers may ignore.
    fn compact_range(
        &mut self,
        mut first: usize,
        last: usize,
        expiry: Option<&LogExpiryConfig>,
        best_effort: bool,
    ) -> Result<()> {
        if first > last || (expiry.is_none() && first == last) {
            return Ok(());
        }
        self.stats.attempts += 1;
        let res = self.compact_range_inner(&mut first, last, expiry, best_effort);
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
        best_effort: bool,
    ) -> Result<()> {
        // Hold the list lock to read "tables.list" and lock the tables of the range.
        let tables_list_lock = flock_acquire(&self.list_file, self.opts.lock_timeout_ms)?;

        // The range the caller asked for may have changed if the stack is
        // outdated; rather than guessing, abort.
        if self.is_outdated()? {
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
        let new_table = match self.compact_locked(first, last, expiry) {
            Ok(t) => Some(t),
            Err(Error::EmptyTable) => None,
            Err(e) => return Err(e),
        };

        // Re-lock "tables.list" to replace the compacted range with the new table.
        let tables_list_lock = flock_acquire(&self.list_file, self.opts.lock_timeout_ms)?;
        set_permissions(tables_list_lock.lock_path(), self.opts.default_permissions)?;

        // A concurrent process may have updated the stack while it was unlocked.
        // Continue only if the compacted tables are still in it, in order.
        let (names, first_to_replace, last_to_replace) = if self.is_outdated()? {
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

        if let Err(e) = commit_lock(tables_list_lock, list.as_bytes(), self.opts.fsync) {
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

    /// `reftable_stack_compact_all()` (`stack.c:1515-1520`): compact the whole
    /// stack into one table, expiring reflog entries per `config`.
    pub fn compact_all(&mut self, config: Option<&LogExpiryConfig>) -> Result<()> {
        let last = self.merged.tables.len().saturating_sub(1);
        self.compact_range(0, last, config, false)
    }

    /// `stack_segments_for_compaction()` (`stack.c:1603-1622`).
    fn segments_for_compaction(&self) -> Segment {
        let version = if self.opts.hash_id == HashId::Sha1 { 1 } else { 2 };
        let overhead = header_size(version) as u64 - 1;
        let sizes: Vec<u64> = self.merged.tables.iter().map(|t| t.size - overhead).collect();
        suggest_compaction_segment(&sizes, self.opts.auto_compaction_factor)
    }

    /// `update_segment_if_compaction_required()` (`stack.c:1624-1647`).
    fn compaction_segment(&self, use_geometric: bool) -> (bool, Segment) {
        if self.merged.tables.len() < 2 {
            return (false, Segment::default());
        }
        if !use_geometric {
            return (true, Segment::default());
        }
        let seg = self.segments_for_compaction();
        (seg.end > seg.start, seg)
    }

    /// `reftable_stack_compaction_required()` (`stack.c:1649-1656`): whether
    /// all tables could be compacted, or with `use_heuristics`, whether the
    /// geometric sequence needs restoring.
    pub fn compaction_required(&self, use_heuristics: bool) -> bool {
        self.compaction_segment(use_heuristics).0
    }

    /// `reftable_stack_auto_compact()` (`stack.c:1658-1673`).
    pub fn auto_compact(&mut self) -> Result<()> {
        let (required, seg) = self.compaction_segment(true);
        if required {
            return self.compact_range(seg.start, seg.end - 1, None, true);
        }
        Ok(())
    }

    /// `reftable_stack_read_ref()` (`stack.c:1681-1709`): `Ok(None)` if absent.
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

    /// `reftable_stack_read_log()` (`stack.c:1711-1741`): the newest log entry
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

    /// `reftable_stack_clean()` (`stack.c:1807-1825`): delete table files no
    /// longer in the stack whose updates it has already absorbed.
    pub fn clean(&mut self) -> Result<()> {
        let _add = self.new_addition(0)?;
        self.reload()?;
        self.clean_locked()
    }

    /// `reftable_stack_clean_locked()` (`stack.c:1780-1805`) with
    /// `remove_maybe_stale_table()` (`stack.c:1749-1778`).
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

/// `struct reftable_addition`: a transaction adding tables to a stack, which
/// holds the `tables.list` lock until committed or dropped.
pub struct Addition {
    lock: Option<gix_lock::File>,
    reftable_dir: PathBuf,
    new_tables: Vec<String>,
    next_update_index: u64,
}

impl Addition {
    /// The update index the tables of this addition start at.
    pub fn next_update_index(&self) -> u64 {
        self.next_update_index
    }

    /// `reftable_addition_add()` (`stack.c:854-956`): write one table with
    /// `write_table`, which must set the writer's limits to at least
    /// [`next_update_index()`](Self::next_update_index). A table without
    /// records is not added.
    pub fn add(
        &mut self,
        st: &Stack,
        write_table: impl FnOnce(&mut Writer<TableFile>, &Stack) -> Result<()>,
    ) -> Result<()> {
        let file = new_table_file(&self.reftable_dir, self.next_update_index, self.next_update_index, &st.opts)?;
        let mut wr = Writer::new(file, &st.opts)?;
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

    /// `reftable_addition_commit()` (`stack.c:761-833`): rewrite `tables.list`
    /// with the new tables appended, reload, and auto-compact.
    pub fn commit(mut self, st: &mut Stack) -> Result<()> {
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
        let lock = self.lock.take().expect("held until commit");
        commit_lock(lock, list.as_bytes(), st.opts.fsync)?;

        // Success: the new tables belong to the stack now.
        self.new_tables.clear();

        st.reload_maybe_reuse(true)?;

        if !st.opts.disable_auto_compact {
            // A concurrent writer may be compacting part of the stack
            // (`Lock`), or have rewritten it (`Outdated`); both are benign.
            match st.auto_compact() {
                Ok(()) | Err(Error::Lock | Error::Outdated) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drop for Addition {
    /// `reftable_addition_close()` (`stack.c:638-656`): delete uncommitted
    /// tables; dropping the lock releases it.
    fn drop(&mut self) {
        for name in &self.new_tables {
            let _ = std::fs::remove_file(self.reftable_dir.join(name));
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

/// `suggest_compaction_segment()` (`stack.c:1527-1601`): the segment to
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
