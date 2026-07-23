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
pub fn commit_index(
    repo: &gix::Repository,
    index: &gix::index::File,
    message: &str,
) -> Result<ObjectId> {
    let (tree_id, parents, msg) = prepare_commit(repo, index, message)?;

    // `Repository::commit` writes the commit and advances `HEAD` (write-through
    // to its branch, or the detached ref) with the canonical reflog message,
    // requiring the first parent to be the current tip — git's ref-safety check.
    // It reads author/committer from configuration and, exactly like git, errors
    // when no `user.name`/`user.email` is set — the porcelain `git commit` path.
    let commit_id = repo.commit("HEAD", &msg, tree_id, parents)?;
    Ok(commit_id.detach())
}

/// Like [`commit_index`], but for the daemon's autonomous autobump.
///
/// git refuses to commit without a configured `user.name`/`user.email`; the
/// autobump inherits that via [`commit_index`]'s `repo.commit`, so on a machine
/// or CI runner with no git identity the daemon would stage the coalesced
/// pointer bump but fail to *commit* it — leaving the parent's `modified: <sub>`
/// marker in place and autonomy silently stalled. The daemon must not depend on
/// ambient identity: prefer the configured committer/author when present (so the
/// commit is attributed to the developer, unchanged), and fall back to a fixed
/// `zvcs` identity only when none is configured.
pub fn commit_index_autonomous(
    repo: &gix::Repository,
    index: &gix::index::File,
    message: &str,
) -> Result<ObjectId> {
    let (tree_id, parents, msg) = prepare_commit(repo, index, message)?;

    // Raw git wire time ("<seconds> <±hhmm>"), built the same way gix builds a
    // signature's time for a config-less identity (see gix identity::Personas).
    let now = gix::date::Time::now_local_or_utc().format_or_unix(gix::date::time::Format::Raw);
    let fallback = gix::actor::SignatureRef {
        name: b"zvcs".as_bstr(),
        email: b"zvcs@localhost".as_bstr(),
        time: &now,
    };
    // `committer()`/`author()` are `None` when unconfigured; `Some(Err(_))` on a
    // malformed configured date.
    let committer = match repo.committer() {
        Some(sig) => sig?,
        None => fallback,
    };
    let author = match repo.author() {
        Some(sig) => sig?,
        None => fallback,
    };

    let commit_id = repo.commit_as(committer, author, "HEAD", &msg, tree_id, parents)?;
    Ok(commit_id.detach())
}

/// Build the canonical tree from `index`'s entries and resolve the parent(s),
/// shared by [`commit_index`] and [`commit_index_autonomous`].
fn prepare_commit(
    repo: &gix::Repository,
    index: &gix::index::File,
    message: &str,
) -> Result<(ObjectId, Vec<ObjectId>, String)> {
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

    Ok((tree_id, parents, msg))
}
