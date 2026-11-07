//! `the_repository->index` between two steps of one command.
//!
//! git reads the index into one process-wide `struct index_state` and every later step of the
//! command works on that same structure: what a refresh learnt about an entry — that its file
//! still matches, `ce_mark_uptodate()` — is still known when the command writes the index at
//! the end. The commands here mostly read the index afresh in each step, which forgets those
//! marks; the next write then treats every racily clean entry as unverified, and in a split
//! index moves it into the split half as a stand-in where git writes none
//! (`prepare_to_write_split_index()`, split-index.c:283-294).
//!
//! A step that refreshed the index holds it here, and the step that goes on to work on the
//! index takes it instead of reading it back.

use std::cell::RefCell;

thread_local! {
    static HELD: RefCell<Option<(gix::Repository, gix::index::File)>> = const { RefCell::new(None) };
}

/// Hold `index`, the in-memory index of `repo` as the command now has it.
pub fn hold(repo: &gix::Repository, index: gix::index::File) {
    HELD.with(|held| *held.borrow_mut() = Some((repo.clone(), index)));
}

/// A copy of the held index, or else the index on disk — an empty one when there is no file,
/// as `do_read_index()` reads it with `must_exist == 0` (read-cache.c:2232).
pub fn peek_or_read(repo: &gix::Repository) -> anyhow::Result<gix::index::File> {
    if let Some(index) = HELD.with(|held| held.borrow().as_ref().map(|(_, i)| i.clone())) {
        return Ok(index);
    }
    Ok(crate::index_open::or_empty(repo)?)
}

/// Take the held index, if any; whoever takes it writes it.
pub fn take() -> Option<gix::index::File> {
    HELD.with(|held| held.borrow_mut().take()).map(|(_, i)| i)
}

/// Write the held index, if nothing took it.
pub fn flush() -> anyhow::Result<()> {
    if let Some((repo, mut index)) = HELD.with(|held| held.borrow_mut().take()) {
        crate::index_racy::write(&repo, &mut index)?;
    }
    Ok(())
}

thread_local! {
    static WRITTEN: RefCell<Option<gix::index::File>> = const { RefCell::new(None) };
}

/// Keep `index`, which the command has just written, as the in-memory index it goes on
/// working with: `write_locked_index()` leaves `the_repository->index` in place, with the
/// `CE_UPTODATE` marks and `CE_SKIP_WORKTREE` bits it was written with.
pub fn keep_written(index: gix::index::File) {
    WRITTEN.with(|written| *written.borrow_mut() = Some(index));
}

/// Forget the index [`keep_written`] kept: the command wrote the index again since, and
/// reads that one back (`discard_index()` + `repo_read_index()`).
pub fn forget_written() {
    WRITTEN.with(|written| written.borrow_mut().take());
}

/// Write the held index, if nothing took it, and hand over the in-memory index the command
/// now has: the one [`keep_written`] kept, else the held one just written; `None` when the
/// command holds neither and the index on disk is all there is.
pub fn flush_and_take_in_memory() -> anyhow::Result<Option<gix::index::File>> {
    let held = match HELD.with(|held| held.borrow_mut().take()) {
        Some((repo, mut index)) => {
            crate::index_racy::write(&repo, &mut index)?;
            Some(index)
        }
        None => None,
    };
    Ok(WRITTEN.with(|written| written.borrow_mut().take()).or(held))
}

/// Forget the held index without writing it — git rolled its lock back, or discarded the
/// in-memory index before handing the work to a child process.
pub fn discard() {
    HELD.with(|held| held.borrow_mut().take());
}
