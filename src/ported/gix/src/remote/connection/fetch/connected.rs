//! Port of git's `connected.c`: the post-fetch check that everything the remote said it was
//! sending actually arrived and hangs together with what we already had.
//!
//! git phrases the question as a `rev-list` invocation:
//!
//! ```text
//! git rev-list --objects --stdin --not --exclude-hidden=fetch --all --quiet --alternate-refs
//! ```
//!
//! fed with the tip of every ref the fetch is about to store. If that walk completes, every commit,
//! tag and tree reachable from those tips is present locally; if it fails, the remote left something
//! out and the refs must not be installed. As `connected.c` puts it, this "does _not_ validate the
//! individual objects" - blobs are named by the trees that contain them but never read, which is why
//! a missing blob passes and a missing tree does not.
//!
//! [`check_connected()`] is that function, and it is called from
//! [`receive()`](super::receive_pack) right where `store_updated_refs()` calls it in
//! `builtin/fetch.c` - before the ref transaction, so a short pack leaves the refs untouched.

use std::collections::{HashSet, VecDeque};

use gix_hash::{ObjectId, oid};
use gix_object::Find;

/// The failure modes of [`check_connected()`] that are *not* a verdict of "disconnected".
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error("Could not read the references to exclude from the connectivity check")]
    References(#[from] crate::reference::iter::Error),
    #[error("Could not iterate the references to exclude from the connectivity check")]
    ReferencesIter(#[from] crate::reference::iter::init::Error),
    #[error("Could not load the alternate object databases")]
    LoadAlternates(#[from] gix_odb::store::load_index::Error),
}

/// git's `struct check_connected_options`, reduced to the fields the fetch call site sets.
#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    /// `opt.is_deepening_fetch`: a fetch that only deepens an existing shallow boundary already has
    /// everything reachable, so git drops the `--not --all` half of the walk.
    pub is_deepening_fetch: bool,
    /// Whether this fetch asked for a filter, and so is turning the repository into a partial clone.
    ///
    /// git's `partial_clone_register()` runs before the check does (`builtin/clone.c` registers the
    /// promisor remote ahead of `update_remote_refs()`), so `repo_has_promisor_remote()` is already
    /// true by the time `check_connected()` looks. Here the configuration is written after the fetch
    /// returns, so the caller has to say so.
    pub from_promisor: bool,
}

/// Return `true` if every object reachable from `tips` is present in `repo`, in the sense
/// `check_connected()` means it.
///
/// `tips` is git's `oid_iterate_fn`; an empty one is connected by definition, matching the early
/// `if (!oid) return err;`.
pub fn check_connected(
    repo: &crate::Repository,
    tips: &[ObjectId],
    opts: Options,
) -> Result<bool, Error> {
    if tips.is_empty() {
        return Ok(true);
    }

    // The partial-clone shortcut: enumerating and excluding all promisor objects is slow, and in a
    // partial clone the walk degenerates anyway, so git settles for "each wanted tip arrived in a
    // promisor pack". Only if one of them did not does it fall through to the walk.
    if (opts.from_promisor || !crate::promisor::remotes(repo).is_empty())
        && tips.iter().all(|id| in_promisor_pack(repo, id))
    {
        return Ok(true);
    }

    // `--not --all`, plus `--alternate-refs`: everything we can already reach is uninteresting, so
    // the walk only has to account for what this fetch was supposed to bring.
    // A ref may name a tag object or, in a broken repository, nothing at all; only the commits it
    // ends at can be handed to the walk. Anything unreadable here is simply not excluded, which is
    // how `rev-list` treats an uninteresting side it cannot follow.
    let mut hidden: Vec<ObjectId> = Vec::new();
    if !opts.is_deepening_fetch {
        for repo in std::iter::once(repo).chain(alternate_repos(repo)?.iter()) {
            for reference in repo.references()?.all()?.filter_map(Result::ok) {
                if let Some(Peeled::Commit(id)) = reference.try_id().and_then(|id| peel(repo, id.detach())) {
                    hidden.push(id);
                }
            }
        }
    }

    // Peel the tips the way `rev-list` handles pending objects: a tag has to be parsed to be
    // followed, a commit enters the commit walk, and a tree or blob is checked on its own.
    let mut commit_tips = Vec::new();
    let mut extra_trees = Vec::new();
    for tip in tips {
        match peel(repo, *tip) {
            Some(Peeled::Commit(id)) => commit_tips.push(id),
            Some(Peeled::Tree(id)) => extra_trees.push(id),
            Some(Peeled::Blob) => {}
            None => return Ok(false),
        }
    }

    // The commit half of the walk. A commit that cannot be read - or whose ancestry is cut short by
    // one that cannot be read - is exactly the "did not send all necessary objects" case.
    let mut interesting = Vec::new();
    if !commit_tips.is_empty() {
        let walk = match repo.rev_walk(commit_tips.iter().copied()).with_hidden(hidden).all() {
            Ok(walk) => walk,
            Err(_) => return Ok(false),
        };
        for info in walk {
            match info {
                Ok(info) => interesting.push(info.detach()),
                Err(_) => return Ok(false),
            }
        }
    }

    // The object half. git marks the trees of the boundary commits uninteresting first
    // (`mark_edges_uninteresting()`), which is what keeps the walk proportional to what actually
    // changed rather than to the whole history.
    let ids: HashSet<ObjectId> = interesting.iter().map(|info| info.id).collect();
    let mut want_trees = Vec::with_capacity(interesting.len());
    let mut edge_trees = Vec::new();
    for info in &interesting {
        match tree_of(repo, &info.id) {
            Some(tree) => want_trees.push(tree),
            None => return Ok(false),
        }
        for parent in info.parent_ids.iter() {
            if !ids.contains(parent) {
                if let Some(tree) = tree_of(repo, parent) {
                    edge_trees.push(tree);
                }
            }
        }
    }

    let mut seen = HashSet::new();
    for tree in edge_trees {
        walk_tree(repo, tree, &mut seen, Missing::Tolerate);
    }
    for tree in want_trees.into_iter().chain(extra_trees) {
        if !walk_tree(repo, tree, &mut seen, Missing::Fail) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// How a tree walk reacts to an object it cannot read.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Missing {
    /// The uninteresting side: git's walk marks what it can and does not complain about the rest.
    Tolerate,
    /// The side the fetch was supposed to complete.
    Fail,
}

/// Walk `root` and everything below it, recording each object in `seen` and stopping early at any
/// subtree that is already there.
///
/// Blobs are recorded but never read - `rev-list --objects` names them from their containing tree
/// and does not open them, which is the difference between "connected" and "valid" that
/// `connected.c` calls out.
fn walk_tree(repo: &crate::Repository, root: ObjectId, seen: &mut HashSet<ObjectId>, missing: Missing) -> bool {
    let mut queue = VecDeque::from_iter(Some(root));
    let mut buf = Vec::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(Some(data)) = repo.objects.try_find(&id, &mut buf) else {
            if missing == Missing::Fail {
                return false;
            }
            continue;
        };
        if data.kind != gix_object::Kind::Tree {
            continue;
        }
        for entry in gix_object::TreeRefIter::from_bytes(data.data, id.kind()).filter_map(Result::ok) {
            if entry.mode.is_tree() {
                queue.push_back(entry.oid.to_owned());
            } else if !entry.mode.is_commit() {
                // Gitlink entries name a commit in another repository, which is never ours to have.
                seen.insert(entry.oid.to_owned());
            }
        }
    }
    true
}

/// What a ref tip turned out to be once tags were followed.
enum Peeled {
    Commit(ObjectId),
    Tree(ObjectId),
    Blob,
}

/// Follow `id` through any number of tag objects, as `parse_object()` does for a pending object.
///
/// `None` means the chain ran into something that is not here, which is a disconnection.
fn peel(repo: &crate::Repository, id: ObjectId) -> Option<Peeled> {
    let mut id = id;
    let mut buf = Vec::new();
    loop {
        let data = repo.objects.try_find(&id, &mut buf).ok().flatten()?;
        match data.kind {
            gix_object::Kind::Commit => return Some(Peeled::Commit(id)),
            gix_object::Kind::Tree => return Some(Peeled::Tree(id)),
            gix_object::Kind::Blob => return Some(Peeled::Blob),
            gix_object::Kind::Tag => {
                id = gix_object::TagRefIter::from_bytes(data.data, id.kind()).target_id().ok()?;
            }
        }
    }
}

/// The tree of the commit `id`, or `None` if the commit is missing or unparseable.
fn tree_of(repo: &crate::Repository, id: &oid) -> Option<ObjectId> {
    let mut buf = Vec::new();
    let data = repo.objects.try_find(id, &mut buf).ok().flatten()?;
    (data.kind == gix_object::Kind::Commit)
        .then(|| gix_object::CommitRefIter::from_bytes(data.data, id.kind()).tree_id().ok())
        .flatten()
}

/// git's `find_pack_entry_one()` restricted to packs marked `.promisor`.
fn in_promisor_pack(repo: &crate::Repository, id: &oid) -> bool {
    let pack_dir = repo.objects.store_ref().path().join("pack");
    let Ok(entries) = std::fs::read_dir(pack_dir) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("promisor") {
            continue;
        }
        let index = path.with_extension("idx");
        if gix_pack::index::File::at(&index, id.kind())
            .ok()
            .and_then(|file| file.lookup(id))
            .is_some()
        {
            return true;
        }
    }
    false
}

/// The repositories behind `objects/info/alternates`, which `--alternate-refs` adds to the
/// uninteresting side so that borrowed history does not have to be re-sent.
fn alternate_repos(repo: &crate::Repository) -> Result<Vec<crate::Repository>, Error> {
    Ok(repo
        .objects
        .store_ref()
        .alternate_db_paths()?
        .into_iter()
        .filter_map(|objects_dir| crate::open_opts(objects_dir.parent()?, repo.options.clone()).ok())
        .collect())
}
