//! Build a tree from an in-memory index and record a commit advancing `HEAD`.
//!
//! This is the shared tree-build + commit primitive used both by `git commit`
//! (`porcelain::commit`, which additionally renders git's summary/short-stat)
//! and by autobump (`superset::zbump`, which commits the coalesced pointer
//! bumps so the parent's `modified: <sub> (new commits)` markers clear).
//!
//! It mirrors git exactly: the tree is built in canonical order via the
//! plumbing tree editor, the commit is written with `author`/`committer` from
//! configuration, and `HEAD` is advanced write-through to its branch (or the
//! detached ref) with a matching reflog entry and the first-parent safety check.

use anyhow::Result;
use gix::bstr::ByteSlice;
use gix::ObjectId;

/// Write a commit from the current entries of `index`, advancing `HEAD`.
///
/// Returns the new commit id. Refuses while conflicts are staged, exactly as
/// git does. The caller is responsible for holding the repo lock across the
/// read-modify-write and for having persisted `index` if on-disk consistency is
/// required; this function only reads `index`'s entries to build the tree.
pub fn commit_index(repo: &gix::Repository, index: &gix::index::File, message: &str) -> Result<ObjectId> {
    let hash = repo.object_hash();
    let backing = index.path_backing();

    // Refuse while conflicts are staged, exactly as git does.
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            anyhow::bail!("committing is not possible because you have unmerged files");
        }
    }

    // Feed every index entry into the plumbing tree editor (canonical git order,
    // written to the odb). The high-level `Repository::edit_tree` wrapper is
    // gated behind the `tree-editor` feature, so the editor is constructed
    // directly over the public object database handle instead.
    let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
    for entry in index.entries() {
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(
            path.split(|&b| b == b'/').map(|c| c.as_bstr()),
            mode.kind(),
            entry.id,
        )?;
    }
    let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

    // Parent = current HEAD tip (unborn HEAD → root commit).
    let mut head = repo.head()?;
    let parent = head.try_peel_to_id()?.map(|id| id.detach());
    let parents: Vec<ObjectId> = parent.into_iter().collect();

    // git's on-disk message is newline-terminated.
    let mut msg = message.to_string();
    if !msg.ends_with('\n') {
        msg.push('\n');
    }

    // `Repository::commit` writes the commit and advances `HEAD` (write-through
    // to its branch, or the detached ref) with the canonical reflog message,
    // requiring the first parent to be the current tip — git's ref-safety check.
    let commit_id = repo.commit("HEAD", &msg, tree_id, parents)?;
    Ok(commit_id.detach())
}
