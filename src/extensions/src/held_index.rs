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

/// A copy of the held index, or else the index on disk (`HEAD`'s tree when there is none).
pub fn peek_or_read(repo: &gix::Repository) -> anyhow::Result<gix::index::File> {
    if let Some(index) = HELD.with(|held| held.borrow().as_ref().map(|(_, i)| i.clone())) {
        return Ok(index);
    }
    Ok(repo.index_or_load_from_head_or_empty()?.into_owned())
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

/// Forget the held index without writing it — git rolled its lock back, or discarded the
/// in-memory index before handing the work to a child process.
pub fn discard() {
    HELD.with(|held| held.borrow_mut().take());
}
