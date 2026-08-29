//! Worktree helpers shared by every command that materializes a tree.
//!
//! Lives outside `porcelain` on purpose: that module is generated from its
//! directory listing, where every file is taken to be a subcommand.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// `DEFAULT_NUM_WORKERS` (`parallel-checkout.c:41`).
const DEFAULT_NUM_WORKERS: i64 = 1;
/// `DEFAULT_THRESHOLD_FOR_PARALLELISM` (`parallel-checkout.c:40`).
const DEFAULT_THRESHOLD_FOR_PARALLELISM: i64 = 100;

/// Port of `get_parallel_checkout_configs()` (`parallel-checkout.c:44-67`):
///
/// ```c
/// if (repo_config_get_int(the_repository, "checkout.workers", num_workers))
///         *num_workers = DEFAULT_NUM_WORKERS;
/// else if (*num_workers < 1)
///         *num_workers = online_cpus();
///
/// if (repo_config_get_int(the_repository, "checkout.thresholdForParallelism", threshold))
///         *threshold = DEFAULT_THRESHOLD_FOR_PARALLELISM;
/// ```
///
/// Returns `(workers, threshold)`, or the `fatal:` line git dies with when either
/// value is unreadable. Both are `git_config_int`, so a bad value is
/// `bad numeric config value …` — and note that the *worker count* is read first,
/// so with both keys broken it is `checkout.workers` that reports.
///
/// # What is honored
///
/// The threshold decides how many entries a checkout must have before git hands
/// them to `checkout--worker` processes instead of writing them inline
/// (`unpack-trees.c:482` calls this from `check_updates()`, then
/// `run_parallel_checkout(&state, pc_workers, pc_threshold, …)` at `:504`). This
/// port writes every entry inline — `checkout_subset` below is a single-threaded
/// `gix_worktree_state::checkout`, and `porcelain::checkout__worker` implements
/// the helper's command line and pkt-line framing but not the item protocol — so
/// no value of either key changes which files appear or what is in them. What is
/// reproduced is the read itself and its rejection, which is observable: git
/// refuses the command outright, before any file is touched.
///
/// `GIT_TEST_CHECKOUT_WORKERS`, the environment escape hatch that short-circuits
/// both reads (`parallel-checkout.c:46-58`), is deliberately not honored: it exists
/// to force git's own test suite onto the parallel path this port does not have.
pub fn parallel_checkout_configs(repo: &gix::Repository) -> Result<(i64, i64), String> {
    let workers = match crate::config::config_int_named(repo, "checkout.workers", "checkout.workers")? {
        // `online_cpus()` for a count below one, which is how `checkout.workers=0`
        // asks for "one per core".
        Some(n) if n < 1 => {
            i64::try_from(std::thread::available_parallelism().map_or(1, |n| n.get())).unwrap_or(1)
        }
        Some(n) => n,
        None => DEFAULT_NUM_WORKERS,
    };
    let threshold = crate::config::config_int_named(
        repo,
        "checkout.thresholdForParallelism",
        "checkout.thresholdForParallelism",
    )?
    .unwrap_or(DEFAULT_THRESHOLD_FOR_PARALLELISM);
    Ok((workers, threshold))
}

/// Check out `index` into `dir`, skipping the call entirely when the index has
/// no entries.
///
/// The guard is not an optimization. Commands that check out a *reduced* index
/// build it by cloning the target tree and calling `remove_entries` to drop
/// everything already present in the worktree. When the target tree matches the
/// current one, that removes every entry and leaves a state whose entry list is
/// empty while its path backing is not.
///
/// `gix_worktree_state::checkout` opens with an unconditional
/// `index.take_path_backing()`, and `State::take_path_backing` asserts that the
/// entry list and the path backing are empty together — so that state aborts
/// the process:
///
/// ```text
/// assertion `left == right` failed: BUG: cannot take out backing multiple times
/// ```
///
/// A no-op checkout is exactly what an empty subset means, so skipping is both
/// correct and what avoids the panic. Every call site routes through here so
/// the invariant holds in one place instead of thirteen.
#[allow(clippy::result_large_err)] // checkout::Error is large; boxing would churn all call sites
pub fn checkout_subset<Find>(
    index: &mut gix::index::State,
    dir: impl Into<PathBuf>,
    objects: Find,
    files: &dyn gix::features::progress::Count,
    bytes: &dyn gix::features::progress::Count,
    should_interrupt: &AtomicBool,
    options: gix::worktree::state::checkout::Options,
) -> Result<(), gix::worktree::state::checkout::Error>
where
    Find: gix::objs::Find + Send + Clone,
{
    if index.entries().is_empty() {
        return Ok(());
    }
    gix::worktree::state::checkout(
        index,
        dir,
        objects,
        files,
        bytes,
        should_interrupt,
        options,
    )
    .map(|_| ())
}

/// Remove the now-empty ancestor directories of `full`, stopping at `workdir` or at the
/// first directory that still holds something.
///
/// ```c
/// if (remove_or_warn(ce->ce_mode, ce->name))
///         return;
/// schedule_dir_for_removal(ce->name, ce_namelen(ce));
/// ```
///
/// (`unlink_entry()`, unpack-trees.c.) Deleting the last file in a directory takes the
/// directory with it — `remove_scheduled_dirs()` walks back up as far as the emptiness
/// goes — so a checkout that drops `nested/deep/path.txt` leaves no `nested/` behind.
pub fn prune_empty_dirs(workdir: &std::path::Path, full: &std::path::Path) {
    let original: Option<&std::path::Path> = original_cwd();
    let mut cur = full.parent();
    while let Some(dir) = cur {
        if dir == workdir || !dir.starts_with(workdir) {
            break;
        }
        // ```c
        // if ((startup_info->original_cwd &&
        //      !strcmp(removal.buf, startup_info->original_cwd)) ||
        //     rmdir(removal.buf))
        //         break;
        // ```
        //
        // (`do_remove_scheduled_dirs()`, symlinks.c:288-291, and the same guard in
        // `remove_path()`, dir.c:3533-3535.) git never removes the directory the process
        // was started in — pulling the ground out from under the caller's shell is worse
        // than leaving one empty directory behind.
        // The caller's path may be relative (`../src` from inside it), so both sides are
        // resolved before they are compared.
        if original.is_some() && original == gix::path::realpath(dir).ok().as_deref() {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

/// `startup_info->original_cwd`: the directory this process was started in,
/// captured before anything can `chdir` away from it.
///
/// git normalises it against the work tree and drops it when it *is* the work
/// tree root (which is protected anyway); the comparison in
/// [`prune_empty_dirs`] is against an absolute path, so only the realpath is
/// needed here.
pub fn original_cwd() -> Option<&'static std::path::Path> {
    static ORIGINAL_CWD: std::sync::OnceLock<Option<std::path::PathBuf>> =
        std::sync::OnceLock::new();
    ORIGINAL_CWD
        .get_or_init(|| std::env::current_dir().ok().map(|dir| gix::path::realpath(&dir).unwrap_or(dir)))
        .as_deref()
}
