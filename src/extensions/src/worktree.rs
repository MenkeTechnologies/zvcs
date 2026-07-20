//! Worktree helpers shared by every command that materializes a tree.
//!
//! Lives outside `porcelain` on purpose: that module is generated from its
//! directory listing, where every file is taken to be a subcommand.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

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
