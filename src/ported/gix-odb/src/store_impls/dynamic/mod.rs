//! The standard object store which should fit all needs.
use std::{cell::RefCell, ops::Deref};

use crate::Store;

/// This effectively acts like a handle but exists to be usable from the actual `crate::Handle` implementation which adds caches on top.
/// Each store is quickly cloned and contains thread-local state for shared packs.
pub struct Handle<S>
where
    S: Deref<Target = Store> + Clone,
{
    pub(crate) store: S,
    /// Defines what happens when there is no more indices to load.
    pub refresh: RefreshMode,
    /// The maximum recursion depth for resolving ref-delta base objects, that is objects referring to other objects within
    /// a pack.
    /// Recursive loops are possible only in purposefully crafted packs.
    /// This value doesn't have to be huge as in typical scenarios, these kind of objects are rare and chains supposedly are
    /// even more rare.
    pub max_recursion_depth: usize,

    /// If true, replacements will not be performed even if these are available.
    pub ignore_replacements: bool,

    /// The compression level to use when this handle causes a loose object database to be opened.
    ///
    /// Changing this value does not affect loose object databases that are already open or change the value in other handles.
    pub loose_compression: gix_zlib::Compression,

    pub(crate) token: Option<handle::Mode>,
    snapshot: RefCell<load_index::Snapshot>,
    inflate: RefCell<gix_zlib::Inflate>,
    packed_object_count: RefCell<Option<u64>>,
}

/// Context for [`Store::load_one_index()`].
///
/// It is typically created by [`Handle::index_ctx()`] from handle-local settings and the marker of its current
/// snapshot. [`Store::load_all_indices()`] creates it directly as that operation has no handle.
#[derive(Clone, Copy)]
pub(crate) struct IndexCtx {
    refresh_mode: RefreshMode,
    marker: types::SlotIndexMarker,
    loose_compression: gix_zlib::Compression,
}

/// Decide what happens when all indices are loaded.
#[derive(Default, Clone, Copy)]
pub enum RefreshMode {
    /// Check for new or changed pack indices (and pack data files) when the last known index is loaded.
    /// During runtime we will keep pack indices stable by never reusing them, however, there is the option for
    /// clearing internal caches which is likely to change pack ids and it will trigger unloading of packs as they are missing on disk.
    #[default]
    AfterAllIndicesLoaded,
    /// Use this if you expect a lot of missing objects that shouldn't trigger refreshes even after all packs are loaded.
    /// This comes at the risk of not learning that the packs have changed in the mean time.
    Never,
}

impl RefreshMode {
    /// Set this refresh mode to never refresh.
    pub fn never(&mut self) {
        *self = RefreshMode::Never;
    }
}

/// A hook that obtains the objects named by `ids` from a *promisor remote* and places them in this store,
/// returning `true` if it made progress and the lookup that triggered it should be retried.
///
/// This is how a *partial clone* stays usable: the clone deliberately left objects behind, and any read of
/// one of them has to go back to the remote first. `git` implements the same hook in `promisor-remote.c`,
/// where a missing object makes `oid_object_info_extended()` call `promisor_remote_get_direct()`.
///
/// The hook must not read objects through the store that installed it, as it is called while that store is
/// resolving a lookup.
pub type PromisorFetchFn = Box<dyn Fn(&[gix_hash::ObjectId]) -> bool + Send + Sync>;

thread_local! {
    /// Set while a [`PromisorFetchFn`] runs, so a store consulted *by the fetch itself* reports objects as
    /// missing instead of recursing into another fetch. `git` gets this for free by running its promisor
    /// fetch in a subprocess that has `fetch_if_missing` turned off.
    static PROMISOR_FETCH_IN_PROGRESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Store {
    /// Install `fetch` as the hook consulted when an object is not present locally, unless one is installed
    /// already, in which case this does nothing.
    ///
    /// Every handle of this store, including ones cloned later, observes the hook.
    pub fn set_promisor(&self, fetch: PromisorFetchFn) {
        let _keep_the_first_hook = self.promisor.set(fetch);
    }

    /// Return `true` if a promisor hook is installed, meaning missing objects may still be obtainable.
    pub fn has_promisor(&self) -> bool {
        self.promisor.get().is_some()
    }

    /// Ask the promisor hook for `ids`, returning `true` if it claims to have placed them in this store.
    ///
    /// Returns `false` without calling out if no hook is installed or if we are already inside one.
    ///
    /// Handles pick the objects up on their next disk refresh; call this ahead of a bulk read to spare
    /// it one round trip per object, which is what `git`'s `check_updates()` does before a checkout.
    pub fn fetch_from_promisor(&self, ids: &[gix_hash::ObjectId]) -> bool {
        let Some(fetch) = self.promisor.get() else {
            return false;
        };
        if ids.is_empty() || PROMISOR_FETCH_IN_PROGRESS.get() {
            return false;
        }
        PROMISOR_FETCH_IN_PROGRESS.set(true);
        let _reset = ResetPromisorFlagOnDrop;
        if !fetch(ids) {
            return false;
        }
        // The hook wrote a pack behind the store's back. Fold it into the slot map here, once, so that
        // every handle - including ones cloned afterwards, as the worktree checkout does per thread -
        // starts from an index that already knows about it. Leaving this to each handle's own refresh
        // makes them race, and a handle that loses re-fetches an object that is already on disk.
        self.consolidate_with_disk_state(
            false, /* needs init */
            true,  /* load one new index */
            self.loose_compression,
        )
        .ok();
        true
    }
}

/// Clears [`PROMISOR_FETCH_IN_PROGRESS`] even if the hook unwinds, so one failed fetch doesn't leave every
/// later lookup in this thread convinced it is nested inside a fetch.
struct ResetPromisorFlagOnDrop;

impl Drop for ResetPromisorFlagOnDrop {
    fn drop(&mut self) {
        PROMISOR_FETCH_IN_PROGRESS.set(false);
    }
}

///
pub mod find;

///
pub mod prefix;

mod header;

///
pub mod iter;

///
pub mod write;

///
pub mod init;

pub(crate) mod types;
pub use types::Metrics;

pub(crate) mod handle;

///
pub mod load_index;

///
pub mod verify;

mod load_one;

mod metrics;

mod access;

///
pub mod structure;
