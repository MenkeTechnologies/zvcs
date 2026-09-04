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
/// no entries and skipping each entry the work tree already holds.
///
/// The per-entry skip is git's, not an optimization either: `checkout_entry_ca()`
/// returns before it writes anything for a path whose file already matches its entry
/// (see [`is_up_to_date`]), and the difference shows up as soon as `refs/replace/`
/// maps an entry's blob to another object.
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
    let dir = dir.into();

    // ```c
    // if (!check_path(path.buf, path.len, &st, state->base_dir_len)) {
    //         [...]
    //         unsigned changed = ie_match_stat(state->istate, ce, &st,
    //                                          CE_MATCH_IGNORE_VALID | CE_MATCH_IGNORE_SKIP_WORKTREE);
    //         [...]
    //         if (!changed)
    //                 return 0;
    // ```
    //
    // (`checkout_entry_ca()`, entry.c.) The early return is ahead of the `state->force`
    // test, so *no* checkout writes a file that already matches its entry — not
    // `checkout -f`, not `reset --hard`, not `checkout-index -f`. This crate's checkout
    // has no such gate and writes every entry it is handed, which is invisible in the
    // ordinary case and not invisible at all when `refs/replace/` maps the entry's blob
    // to another one: re-materialising then puts the *replacement* bytes in the work tree
    // and leaves the file modified against the very index that named it. git materialises
    // through the replacement too when it materialises at all — the difference is that it
    // does not materialise a file it has no reason to touch.
    let stale: Vec<usize> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| !is_up_to_date(entry, backing, &dir, options.stat_options))
            .map(|(i, _)| i)
            .collect()
    };
    if stale.len() == index.entries().len() {
        return gix::worktree::state::checkout(index, dir, objects, files, bytes, should_interrupt, options)
            .map(|_| ());
    }

    // Narrow the state to the entries that still need writing, check those out, then put
    // the full list back with the stat data the checkout recorded for each one it wrote.
    // The path storage is left alone throughout — `checkout()` takes it and returns it —
    // so every entry's path range stays valid on both sides of the call.
    let mut all = index.swap_entries(Vec::new());
    if stale.is_empty() {
        index.swap_entries(all);
        return Ok(());
    }
    index.swap_entries(stale.iter().map(|&i| all[i].clone()).collect());
    let res = gix::worktree::state::checkout(index, dir, objects, files, bytes, should_interrupt, options);
    let written = index.swap_entries(Vec::new());
    for (slot, entry) in stale.into_iter().zip(written) {
        all[slot] = entry;
    }
    index.swap_entries(all);
    res.map(|_| ())
}

/// `ie_match_stat()` (read-cache.c) as `checkout_entry_ca()` asks it: does the work tree
/// already hold exactly what this entry names?
///
/// Three things have to agree, and the entry is written whenever any of them cannot be
/// established:
///
///  * the file exists and its **type** is the entry's — a gitlink is never skipped,
///    because a directory is not something a stat comparison can answer for;
///  * the recorded `stat` matches the file's under the repository's `core.checkStat` /
///    `core.trustCtime` settings, which is `ce_match_stat_basic()`;
///  * the file's **content** hashes to the entry's object id, which is git's
///    `ce_modified_check_fs()` — the branch it takes for a racily-clean entry, whose stat
///    cannot be trusted on its own.
///
/// The content check is unconditional here rather than only for racy entries, which is the
/// stricter of the two readings: it can only cause a file to be written that git would have
/// left alone, never the reverse. It hashes the file raw, so a path whose blob went through
/// a clean filter compares unequal and is rewritten — the same conservative direction.
fn is_up_to_date(
    entry: &gix::index::Entry,
    backing: &gix::index::PathStorageRef,
    dir: &std::path::Path,
    stat_options: gix::index::entry::stat::Options,
) -> bool {
    use gix::index::entry::Mode;
    if !matches!(entry.mode, Mode::FILE | Mode::FILE_EXECUTABLE | Mode::SYMLINK) {
        return false;
    }
    let path = entry.path_in(backing);
    let Ok(rela) = gix::path::try_from_bstr(path) else {
        return false;
    };
    let full = dir.join(rela);
    let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
        return false;
    };
    // `ce_match_stat_basic()`'s type switch: a symlink entry needs a symlink on disk and a
    // blob entry needs a regular file, whatever the stat fields say.
    let type_matches = match entry.mode {
        Mode::SYMLINK => md.is_symlink(),
        _ => md.is_file(),
    };
    if !type_matches {
        return false;
    }
    // `ce_match_stat_basic()`'s `S_IFREG` arm: the executable bit is part of the mode git
    // compares, and no stat field carries it.
    if entry.mode == Mode::FILE_EXECUTABLE && !md.is_executable() {
        return false;
    }
    if entry.mode == Mode::FILE && md.is_executable() {
        return false;
    }
    let Ok(disk) = gix::index::entry::Stat::from_fs(&md) else {
        return false;
    };
    if !entry.stat.matches(&disk, stat_options) {
        return false;
    }
    let content = if md.is_symlink() {
        let Ok(target) = std::fs::read_link(&full) else {
            return false;
        };
        gix::path::into_bstr(target).into_owned().into()
    } else {
        let Ok(bytes) = std::fs::read(&full) else {
            return false;
        };
        bytes
    };
    gix::objs::compute_hash(entry.id.kind(), gix::objs::Kind::Blob, &content).is_ok_and(|id| id == entry.id)
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
