//! The `reftable` ref storage backend (`refs/reftable-backend.c`, git v2.56.0).
//!
//! References and reflogs live in stacks of reftables (see `gix-reftable`): the
//! *main* stack in `<common dir>/reftable` holds the shared references and those
//! of the main worktree, each linked worktree keeps its private references in a
//! stack of its own under `<its git dir>/reftable`. A [`Backend`] is what git
//! calls `struct reftable_ref_store`; a [`file::Store`](crate::file::Store)
//! opened with [`RefStorage::Reftable`](crate::store::RefStorage::Reftable)
//! carries one and routes each of its operations here.
//!
//! The backend is filled in by several independent steps, each owning a module:
//!
//! | module          | contents                                                         |
//! |-----------------|------------------------------------------------------------------|
//! | this one        | [`Backend`], its construction, [`Error`], the lazy [`WriteConfig`] |
//! | `worktree`      | [`parse_worktree_ref()`] and [`Backend::backend_for()`]          |
//! | `find`          | reading one reference                                            |
//! | `iter`          | iterating references, merging worktree and main stack            |
//! | `log`           | reading reflogs                                                  |
//! | `transaction`   | preparing and committing transactions                            |
//! | `maintenance`   | reflog creation/deletion/expiry, optimize, rename/copy, fsck     |
//!
//! Operations not ported yet fail with [`Error::Unsupported`].
//!
//! Like git, every operation reloads the stacks it reads from (`reload` of
//! [`Backend::backend_for()`]); a stack is behind a mutex so that a reload, which
//! mutates it, is possible through a shared backend.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use gix_object::bstr::BString;
use gix_reftable::{Stack, StackOptions, WriteOptions};

mod find;
mod iter;
mod log;
mod maintenance;
mod transaction;
mod worktree;

pub use iter::RefIter;
pub use log::ReflogEntry;
pub use maintenance::{ExpireFlags, ExpirePolicy, FsckReport};
pub use transaction::TransactionData;
pub use worktree::{WorktreeType, parse_worktree_ref};

/// One stack of the backend, git's `struct reftable_backend`, shared between the
/// backend and the operations using it. Lock it with [`lock()`].
///
/// git also caches an iterator per stack, dropped whenever the stack reloads
/// (`reftable_backend_on_reload()`); that is an optimization this port omits.
pub type StackRef = Arc<Mutex<Stack>>;

/// Lock `stack`; a poisoned lock is taken over, as the stack itself stays
/// consistent (it is only replaced wholesale on reload).
pub fn lock(stack: &StackRef) -> MutexGuard<'_, Stack> {
    stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The error of backend operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The operation is not ported to the reftable backend yet.
    #[error("reftable: {operation} is not supported yet")]
    Unsupported {
        /// The name of the backend operation, as git's `refs_be_reftable` calls it.
        operation: &'static str,
    },
    /// The reftable library failed; its message is `reftable_error_str()`.
    #[error(transparent)]
    Reftable(#[from] gix_reftable::Error),
    /// A file system operation of the backend itself failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The error of an operation that is not ported yet.
    pub(crate) fn unsupported(operation: &'static str) -> Self {
        Error::Unsupported { operation }
    }
}

/// The options git parses lazily before the first write or compaction,
/// `struct reftable_be_write_options` (refs/reftable-backend.c:141-155).
#[derive(Debug, Clone)]
pub struct WriteConfig {
    /// The options for writing tables and compacting stacks.
    pub opts: WriteOptions,
    /// `core.logAllRefUpdates`, `None` while unset (`LOG_REFS_UNSET`); whoever
    /// decides whether to write a reflog maps unset to `Disable` in a bare
    /// repository and to `Normal` otherwise (`should_write_log()`,
    /// refs/reftable-backend.c:1443-1461).
    pub log_all_ref_updates: Option<crate::store::WriteReflog>,
}

impl Default for WriteConfig {
    /// `reftable_be_write_options()` (refs/reftable-backend.c:361-392) without
    /// any configuration: a 100ms lock timeout, and the library's block size
    /// spelled out, as reflog messages are trimmed to half of it. New files
    /// get the default mode, which is `0666` less the umask.
    fn default() -> Self {
        WriteConfig {
            opts: WriteOptions {
                lock_timeout_ms: 100,
                block_size: gix_reftable::DEFAULT_BLOCK_SIZE,
                ..WriteOptions::default()
            },
            log_all_ref_updates: None,
        }
    }
}

/// Produces the [`WriteConfig`] of a backend from the repository's
/// configuration. It runs at most once per backend, on the first operation
/// that needs the options, which is where git reads (and dies on invalid)
/// `reftable.blockSize`, `reftable.restartInterval`, `reftable.indexObjects`,
/// `reftable.geometricFactor`, `reftable.lockTimeout` and
/// `core.logAllRefUpdates` (`reftable_be_config()`, refs/reftable-backend.c:323-359);
/// it also accounts for `core.sharedRepository` in `default_permissions` and
/// `GIT_TEST_REFTABLE_AUTOCOMPACTION` in `disable_auto_compact`.
pub type WriteConfigFn = Arc<dyn Fn() -> WriteConfig + Send + Sync>;

/// `struct reftable_ref_store` (refs/reftable-backend.c:123-158).
pub struct Backend {
    /// The private git directory: the repository's for the main worktree, the
    /// worktree's own for a linked one (`refs->base.gitdir`).
    git_dir: PathBuf,
    /// The common directory, `repo->commondir`.
    common_dir: PathBuf,
    /// Options every stack of this backend is opened with.
    stack_options: StackOptions,
    /// `main_backend`: `<common dir>/reftable`, the shared references and those
    /// of the main worktree. `None` if opening it failed, see `err`.
    main: Option<StackRef>,
    /// `worktree_backend`: `<git dir>/reftable`, the private references of the
    /// linked worktree this backend was opened for; `None` in the main worktree.
    worktree: Option<StackRef>,
    /// `worktree_backends`: the stacks of other worktrees, by worktree name,
    /// opened when a `worktrees/<name>/…` reference is first used.
    other_worktrees: Mutex<HashMap<BString, StackRef>>,
    /// `refs->err`: the error of the last stack initialization, which
    /// operations report instead of running (see [`Backend::check()`]).
    err: Mutex<Option<gix_reftable::Error>>,
    /// Where the [`WriteConfig`] comes from; the defaults if never set.
    write_config_fn: OnceLock<WriteConfigFn>,
    /// `write_opts_lazy_loaded`, set on first use.
    write_config: OnceLock<WriteConfig>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("git_dir", &self.git_dir)
            .field("common_dir", &self.common_dir)
            .field("stack_options", &self.stack_options)
            .field("worktree", &self.worktree.is_some())
            .field("err", &self.err)
            .finish_non_exhaustive()
    }
}

/// Open `<dir>`'s stack with `opts`, `reftable_backend_init()`
/// (refs/reftable-backend.c:47-55).
fn open_stack(dir: &Path, opts: &StackOptions) -> Result<StackRef, gix_reftable::Error> {
    Ok(Arc::new(Mutex::new(Stack::new(dir, opts)?)))
}

impl Backend {
    /// `reftable_be_init()` (refs/reftable-backend.c:406-475): open the main
    /// stack, and for a linked worktree (`common_dir` is `Some`) the worktree's
    /// own. A failure is kept and reported by every operation, as git does.
    pub fn open(git_dir: &Path, common_dir: Option<&Path>, object_hash: gix_hash::Kind) -> Self {
        let stack_options = StackOptions {
            // By length, as which `gix_hash::Kind` variants exist depends on features.
            hash_id: if object_hash.len_in_bytes() == gix_reftable::HashId::Sha256.size() {
                gix_reftable::HashId::Sha256
            } else {
                gix_reftable::HashId::Sha1
            },
            ..StackOptions::default()
        };
        let is_worktree = common_dir.is_some();
        let common_dir = common_dir.unwrap_or(git_dir).to_owned();
        let mut backend = Backend {
            git_dir: git_dir.to_owned(),
            common_dir: common_dir.clone(),
            stack_options,
            main: None,
            worktree: None,
            other_worktrees: Mutex::new(HashMap::new()),
            err: Mutex::new(None),
            write_config_fn: OnceLock::new(),
            write_config: OnceLock::new(),
        };

        // The main stack is in the common directory, made absolute and free of
        // symlinks unless this is a worktree. `strbuf_realpath(…, 0)` leaves
        // the path empty when it fails, so the stack is then `/reftable`.
        let main_dir = if is_worktree {
            common_dir.join("reftable")
        } else {
            match gix_path::realpath(&common_dir) {
                Ok(dir) => dir.join("reftable"),
                Err(_) => PathBuf::from("/reftable"),
            }
        };
        match open_stack(&main_dir, &backend.stack_options) {
            Ok(stack) => backend.main = Some(stack),
            Err(err) => {
                *backend.err.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(err);
                return backend;
            }
        }

        if is_worktree {
            match open_stack(&git_dir.join("reftable"), &backend.stack_options) {
                Ok(stack) => backend.worktree = Some(stack),
                Err(err) => {
                    *backend.err.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(err);
                }
            }
        }
        backend
    }

    /// The private git directory this backend was opened for.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The common directory, holding the main stack.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// The options every stack is opened with.
    pub fn stack_options(&self) -> &StackOptions {
        &self.stack_options
    }

    /// `if (refs->err) return refs->err;`: fail with the error of the last
    /// stack initialization, if there was one.
    pub fn check(&self) -> Result<(), Error> {
        match *self.err.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            Some(err) => Err(err.into()),
            None => Ok(()),
        }
    }

    /// `refs->err = …`: record the outcome of a stack initialization.
    pub(crate) fn set_err(&self, err: Option<gix_reftable::Error>) {
        *self.err.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = err;
    }

    /// `&refs->main_backend`, without reloading it.
    pub fn main_stack(&self) -> Result<StackRef, Error> {
        self.check()?;
        self.main.clone().ok_or(Error::Reftable(gix_reftable::Error::Api))
    }

    /// `&refs->worktree_backend`: the stack of the linked worktree this backend
    /// was opened for, `None` in the main worktree.
    pub fn worktree_stack(&self) -> Option<StackRef> {
        self.worktree.clone()
    }

    /// Install the source of this backend's [`WriteConfig`]. Only the first
    /// installation takes effect, and only before the options are first used.
    pub fn set_write_config_fn(&self, f: WriteConfigFn) {
        let _ = self.write_config_fn.set(f);
    }

    /// `reftable_be_write_options()` (refs/reftable-backend.c:361-392): the
    /// write options, produced on first use.
    pub fn write_config(&self) -> &WriteConfig {
        self.write_config.get_or_init(|| {
            let mut config = match self.write_config_fn.get() {
                Some(f) => f(),
                None => WriteConfig::default(),
            };
            // git mirrors the library's default block size here, as reflog
            // messages are trimmed to half of it.
            if config.opts.block_size == 0 {
                config.opts.block_size = gix_reftable::DEFAULT_BLOCK_SIZE;
            }
            config
        })
    }
}

/// A [`Backend`] is shared by every clone of a store, across threads.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Backend>();
};
