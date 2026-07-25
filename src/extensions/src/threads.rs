//! How many workers a parallel path should use.
//!
//! git's diff machinery is single-threaded: one core renders the patch for every
//! file of every commit while the rest of the machine idles. Nothing about the
//! work requires that — a blob pair is diffed in isolation, and the object
//! database is immutable while a read-only verb runs — so the parallel paths here
//! (see `porcelain::diff`) fan the per-blob and per-commit work across the box.
//!
//! Two rules keep that from backfiring:
//!
//! * A thread is only worth spawning if it gets real work, so a batch must carry
//!   at least `min_per_thread` items per worker. Diffing three files across ten
//!   threads costs more in spawns and handle clones than it saves.
//! * The count is capped by the machine's parallelism and overridable with
//!   `ZVCS_THREADS`, which pins it exactly (`1` forces the sequential path). CI
//!   containers report the host's core count through `available_parallelism`
//!   while being cgroup-limited to far less, and benchmarks need a way to hold
//!   the variable still.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Cached `ZVCS_THREADS` / `available_parallelism` resolution.
///
/// `0` means "not resolved yet"; the stored value is the real count plus one so
/// that a legitimate zero-length answer is impossible. Resolution reads the
/// environment and queries the OS, and the parallel paths ask once per batch.
static RESOLVED: AtomicUsize = AtomicUsize::new(0);

/// The machine's usable worker count, honoring `ZVCS_THREADS`.
fn available() -> usize {
    let cached = RESOLVED.load(Ordering::Relaxed);
    if cached != 0 {
        return cached - 1;
    }
    let n = match std::env::var("ZVCS_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) => n.max(1),
        None => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
    };
    RESOLVED.store(n + 1, Ordering::Relaxed);
    n
}

/// Workers to run `items` units of work across, given each is only worth a thread
/// at `min_per_thread` units. Returns `1` when the batch is too small to split,
/// which callers take as "stay on the sequential path".
pub fn count(items: usize, min_per_thread: usize) -> usize {
    let min = min_per_thread.max(1);
    if items <= min {
        return 1;
    }
    available().min(items / min).max(1)
}

#[cfg(test)]
mod tests {
    use super::count;

    /// A batch that cannot give every worker `min_per_thread` items stays
    /// sequential — the spawn and the per-thread object-handle clone cost more
    /// than the split saves.
    #[test]
    fn small_batches_stay_sequential() {
        assert_eq!(count(0, 4), 1);
        assert_eq!(count(1, 4), 1);
        assert_eq!(count(4, 4), 1);
    }

    /// Above the threshold the count is bounded by both the machine and the
    /// work: 100 items at 4 apiece can never justify more than 25 workers.
    #[test]
    fn worker_count_is_bounded_by_the_work() {
        let n = count(100, 4);
        assert!((1..=25).contains(&n), "expected at most 25 workers for 100 items at 4 each, got {n}");
    }

    /// `min_per_thread` of 0 must not divide by zero.
    #[test]
    fn zero_minimum_is_clamped() {
        assert!(count(10, 0) >= 1);
    }
}
