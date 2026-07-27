//! A port of git's `shallow.c` commit painting, together with the decision made by
//! `update_shallow()` in `fetch-pack.c` once the pack has been indexed.
//!
//! When the remote is itself shallow it sends `shallow <oid>` lines during the fetch. Unless the
//! fetch asked for a depth change of its own (`--depth`, `--deepen`, `--unshallow`,
//! `--shallow-since`, `--shallow-exclude`), those lines describe *new* shallow roots, and git will
//! not silently adopt them: `.git/shallow` is only rewritten when cloning or when
//! `--update-shallow` was given. Otherwise every fetched ref that depends on one of the new roots
//! is rejected and left untouched.
//!
//! The interesting part is deciding which refs depend on which root, which is what
//! `assign_shallow_commits_to_refs()` does by painting the commit graph downwards from each
//! fetched tip.

use gix_object::Exists;
use std::collections::HashMap;

use gix_hash::ObjectId;

/// Object flags used by the painting, mirroring the `SEEN`/`UNINTERESTING`/`BOTTOM` bits git sets
/// on `struct object`.
const SEEN: u8 = 1 << 0;
const UNINTERESTING: u8 = 1 << 1;
const BOTTOM: u8 = 1 << 2;

/// The error returned by [`Info::prepare()`] and the painting it drives.
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Could not iterate references to find the tips of our own history")]
    IterRefs(#[from] crate::reference::iter::Error),
    #[error("Could not iterate references to find the tips of our own history")]
    InitRefsIter(#[from] Box<crate::reference::iter::init::Error>),
    #[error("Could not look up an object while painting the commit graph")]
    FindObject(#[from] crate::object::find::Error),
    #[error("Could not decode a commit while painting the commit graph")]
    DecodeCommit(#[from] gix_object::decode::Error),
    #[error("Could not determine if a shallow boundary commit is reachable from our own refs")]
    MergeBase(#[from] crate::repository::merge_bases_many::Error),
    #[error("Could not read the current shallow boundary")]
    ReadShallow(#[from] gix_shallow::read::Error),
    #[error("Could not write the new shallow boundary")]
    WriteShallow(#[from] gix_shallow::write::Error),
    #[error("Could not lock the shallow file for updating")]
    LockShallow(#[from] gix_lock::acquire::Error),
}

/// The `shallow <oid>` lines the remote sent, split into the ones we already have (`ours`) and the
/// ones only the remote knows about (`theirs`), the shape of git's `struct shallow_info`.
pub(crate) struct Info {
    /// All shallow boundary commits the remote advertised, in the order received.
    shallow: Vec<ObjectId>,
    /// Indices into `shallow` for boundaries whose commit we already had before the fetch.
    ours: Vec<usize>,
    /// Indices into `shallow` for boundaries we did not have before the fetch.
    theirs: Vec<usize>,
}

impl Info {
    /// Port of `prepare_shallow_info()`: step 1 splits the sender's shallow commits into "ours" and
    /// "theirs", step 2 drops the ones that are already a root of *our* shallow boundary, which git
    /// spots as a graft with a negative parent count.
    pub(crate) fn prepare(
        repo: &crate::Repository,
        shallow: Vec<ObjectId>,
        our_boundary: &[ObjectId],
    ) -> Self {
        let mut ours = Vec::new();
        let mut theirs = Vec::new();
        for (idx, id) in shallow.iter().enumerate() {
            if repo.objects.exists(id) {
                if our_boundary.contains(id) {
                    continue;
                }
                ours.push(idx);
            } else {
                theirs.push(idx);
            }
        }
        Info { shallow, ours, theirs }
    }

    /// `true` if nothing is left to reason about, git's `!si->nr_ours && !si->nr_theirs`.
    fn is_empty(&self) -> bool {
        self.ours.is_empty() && self.theirs.is_empty()
    }

    /// Port of `remove_nonexistent_theirs_shallow()`: step 4 drops the boundaries the pack turned
    /// out not to contain after all.
    pub(crate) fn remove_nonexistent_theirs(&mut self, repo: &crate::Repository) {
        let shallow = &self.shallow;
        self.theirs.retain(|&idx| repo.objects.exists(&shallow[idx]));
    }

    /// The boundary commits that survived, `ours` followed by `theirs`, which is the set git hands
    /// to `setup_alternate_shallow()` as `extra`.
    fn surviving(&self) -> Vec<ObjectId> {
        self.ours
            .iter()
            .chain(self.theirs.iter())
            .map(|&idx| self.shallow[idx])
            .collect()
    }

    /// Port of `assign_shallow_commits_to_refs()` with `used == NULL`: step 6(+7) associates the
    /// shallow commits with the newly fetched refs.
    ///
    /// `refs` holds the object each fetched ref points to, so bit `n` of a painted bitmap means
    /// "reachable from `refs[n]`". On return `ours` and `theirs` only hold boundaries some ref
    /// actually needs, and the returned vector counts, per ref, how many boundaries it needs — the
    /// `ref_status` array git uses to flag refs as `REF_STATUS_REJECT_SHALLOW`.
    pub(crate) fn assign_to_refs(
        &mut self,
        repo: &crate::Repository,
        refs: &[ObjectId],
    ) -> Result<Vec<usize>, Error> {
        let mut painter = Painter {
            repo,
            flags: HashMap::new(),
            ref_bitmap: HashMap::new(),
            bitmap_nr: refs.len().div_ceil(32),
        };

        // "--not --all" to cut short the traversal if the new refs connect to the old ones. If they
        // don't (e.g. forced updates) the walk has to go all the way down to the shallow commits.
        let our_tips = tips_of_our_refs(repo)?;
        for tip in &our_tips {
            painter.mark_uninteresting(*tip)?;
        }

        // Mark potential bottoms so the walk won't go out of bounds.
        for &idx in self.ours.iter().chain(self.theirs.iter()) {
            *painter.flags.entry(self.shallow[idx]).or_default() |= BOTTOM;
        }

        for (id, tip) in refs.iter().enumerate() {
            painter.paint_down(*tip, id)?;
        }

        self.post_assign(repo, &painter, refs.len(), &our_tips)
    }

    /// Port of `post_assign_shallow()`: step 7 drops the boundaries no ref can reach, and for the
    /// ones we already had, the boundaries that are an ancestor of a ref we already hold — those
    /// need no new root.
    fn post_assign(
        &mut self,
        repo: &crate::Repository,
        painter: &Painter<'_>,
        nr_refs: usize,
        our_tips: &[ObjectId],
    ) -> Result<Vec<usize>, Error> {
        let mut ref_status = vec![0usize; nr_refs];

        // Remove unreachable shallow commits from "theirs".
        let mut kept = Vec::with_capacity(self.theirs.len());
        for &idx in &self.theirs {
            let Some(bitmap) = painter.ref_bitmap.get(&self.shallow[idx]) else {
                continue;
            };
            if bitmap.iter().any(|word| *word != 0) {
                update_refstatus(&mut ref_status, bitmap);
                kept.push(idx);
            }
        }
        self.theirs = kept;

        // Remove unreachable shallow commits from "ours", plus the ones already reachable from a
        // ref we hold, which is git's commit-level reachability test.
        let mut kept = Vec::with_capacity(self.ours.len());
        for &idx in &self.ours {
            let id = self.shallow[idx];
            let Some(bitmap) = painter.ref_bitmap.get(&id) else {
                continue;
            };
            if !bitmap.iter().any(|word| *word != 0) {
                continue;
            }
            if is_ancestor_of_any(repo, id, our_tips)? {
                continue;
            }
            update_refstatus(&mut ref_status, bitmap);
            kept.push(idx);
        }
        self.ours = kept;

        Ok(ref_status)
    }
}

/// Port of `update_refstatus()`: a ref whose bit is set needs this boundary.
fn update_refstatus(ref_status: &mut [usize], bitmap: &[u32]) {
    for (i, status) in ref_status.iter_mut().enumerate() {
        if bitmap[i / 32] & (1 << (i % 32)) != 0 {
            *status += 1;
        }
    }
}

/// git's `repo_in_merge_bases_many(commit, …, 1)`: `true` if `id` is an ancestor of any of `tips`.
fn is_ancestor_of_any(repo: &crate::Repository, id: ObjectId, tips: &[ObjectId]) -> Result<bool, Error> {
    if tips.is_empty() {
        return Ok(false);
    }
    Ok(repo
        .merge_bases_many(id, tips)?
        .into_iter()
        .any(|base| base.detach() == id))
}

/// The commits `HEAD` and every local reference resolve to, git's `refs_head_ref()` plus
/// `refs_for_each_ref()` filtered to what actually is a commit.
fn tips_of_our_refs(repo: &crate::Repository) -> Result<Vec<ObjectId>, Error> {
    let mut tips = Vec::new();
    let mut push = |id: ObjectId| {
        if !tips.contains(&id) {
            tips.push(id);
        }
    };
    if let Ok(Some(id)) = repo.head().map(|head| head.id().map(crate::Id::detach)) {
        if peels_to_commit(repo, id) {
            push(id);
        }
    }
    for reference in repo.references()?.all().map_err(Box::new)?.filter_map(Result::ok) {
        let Ok(id) = reference.into_fully_peeled_id() else {
            continue;
        };
        let id = id.detach();
        if peels_to_commit(repo, id) {
            push(id);
        }
    }
    Ok(tips)
}

/// git's `lookup_commit_reference_gently(oid, 1)`: does this name a commit, possibly through tags?
fn peels_to_commit(repo: &crate::Repository, id: ObjectId) -> bool {
    repo.find_object(id)
        .ok()
        .and_then(|obj| obj.peel_to_kind(gix_object::Kind::Commit).ok())
        .is_some()
}

/// The state `paint_down()` carries between calls: the object flags and the per-commit bitmap of
/// refs that can reach it.
struct Painter<'repo> {
    repo: &'repo crate::Repository,
    flags: HashMap<ObjectId, u8>,
    ref_bitmap: HashMap<ObjectId, Vec<u32>>,
    bitmap_nr: usize,
}

impl Painter<'_> {
    /// Port of `mark_uninteresting()` plus `mark_parents_uninteresting()`: the tip and its whole
    /// ancestry are off limits, so a new ref that merges into our history stops the walk early.
    fn mark_uninteresting(&mut self, tip: ObjectId) -> Result<(), Error> {
        let mut next = vec![tip];
        while let Some(id) = next.pop() {
            let flags = self.flags.entry(id).or_default();
            if *flags & UNINTERESTING != 0 {
                continue;
            }
            *flags |= UNINTERESTING;
            for parent in self.parents(id)? {
                next.push(parent);
            }
        }
        Ok(())
    }

    /// Port of `paint_down()`: walk down from `oid` to its parents until `SEEN`, `UNINTERESTING` or
    /// `BOTTOM` is hit, setting the `id`-th bit in the bitmap of every commit walked.
    fn paint_down(&mut self, oid: ObjectId, id: usize) -> Result<(), Error> {
        if !peels_to_commit(self.repo, oid) {
            return Ok(());
        }
        let mut bitmap = vec![0u32; self.bitmap_nr];
        bitmap[id / 32] |= 1 << (id % 32);

        let mut head = vec![oid];
        while let Some(commit) = head.pop() {
            let flags = self.flags.entry(commit).or_default();
            if *flags & (SEEN | UNINTERESTING) != 0 {
                continue;
            }
            *flags |= SEEN;
            let is_bottom = *flags & BOTTOM != 0;

            let entry = self.ref_bitmap.entry(commit).or_insert_with(|| vec![0; self.bitmap_nr]);
            for (word, add) in entry.iter_mut().zip(bitmap.iter()) {
                *word |= *add;
            }

            if is_bottom {
                continue;
            }
            for parent in self.parents(commit)? {
                if self.flags.get(&parent).is_none_or(|flags| flags & SEEN == 0) {
                    head.push(parent);
                }
            }
        }

        // git resets `SEEN` on every commit once a paint is done, so the next ref paints the same
        // commits again and accumulates its own bit.
        for flags in self.flags.values_mut() {
            *flags &= !SEEN;
        }
        Ok(())
    }

    /// The parents of `id`, or nothing if it isn't a commit we can read.
    fn parents(&self, id: ObjectId) -> Result<Vec<ObjectId>, Error> {
        let Some(object) = self.repo.try_find_object(id)? else {
            return Ok(Vec::new());
        };
        let Ok(commit) = object.peel_to_kind(gix_object::Kind::Commit) else {
            return Ok(Vec::new());
        };
        Ok(commit.into_commit().parent_ids().map(crate::Id::detach).collect())
    }
}

/// What [`update()`] decided to do with the `shallow <oid>` lines the remote sent.
pub(crate) struct Outcome {
    /// Indices into the ref-map mappings whose refs may not be updated without adopting a new
    /// shallow root, git's `REF_STATUS_REJECT_SHALLOW`.
    pub rejected: Vec<usize>,
}

/// How the caller wants new shallow roots handled, the three branches of `update_shallow()` in
/// `fetch-pack.c` that apply once a pack has been received without a depth change being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// git's `args->cloning`: there is no history to protect, so accept every root the pack
    /// actually contains.
    Cloning,
    /// git's `args->update_shallow` (`--update-shallow`): rewrite `.git/shallow` with the roots the
    /// fetched refs really need.
    Update,
    /// git's default: leave `.git/shallow` alone and reject the refs that would need a new root.
    Reject,
}

/// Port of `update_shallow()` in `fetch-pack.c` for the case where the fetch did not ask for a
/// depth change of its own — the case where a depth *was* asked for is handled while negotiating,
/// as the boundary is then known before the pack arrives.
///
/// `remote_shallow` are the `shallow <oid>` lines the remote sent, `ref_tips` the object each
/// fetched ref points to. Returns which of those refs must be left alone.
pub(crate) fn update(
    repo: &crate::Repository,
    mode: Mode,
    remote_shallow: Vec<ObjectId>,
    ref_tips: &[ObjectId],
) -> Result<Outcome, Error> {
    let no_rejections = Outcome { rejected: Vec::new() };
    if remote_shallow.is_empty() {
        return Ok(no_rejections);
    }
    let shallow_file = repo.shallow_file();
    let our_boundary = gix_shallow::read(&shallow_file)?
        .map(Vec::from)
        .unwrap_or_default();

    if mode == Mode::Cloning {
        // The remote is shallow, but this is a clone: there are no objects in the repo to worry
        // about, so accept any shallow point that ended up in the pack.
        let extra: Vec<_> = remote_shallow
            .into_iter()
            .filter(|id| repo.objects.exists(id))
            .collect();
        if !extra.is_empty() {
            write_boundary(&shallow_file, our_boundary, &extra)?;
        }
        return Ok(no_rejections);
    }

    let mut info = Info::prepare(repo, remote_shallow, &our_boundary);
    if info.is_empty() {
        return Ok(no_rejections);
    }
    info.remove_nonexistent_theirs(repo);
    if info.is_empty() {
        return Ok(no_rejections);
    }

    let ref_status = info.assign_to_refs(repo, ref_tips)?;
    if info.is_empty() {
        return Ok(no_rejections);
    }

    match mode {
        Mode::Cloning => unreachable!("handled above"),
        Mode::Update => {
            // The remote is also shallow and `.git/shallow` may be updated, so all refs can be
            // accepted. Only the roots actually reachable from the new refs are added.
            write_boundary(&shallow_file, our_boundary, &info.surviving())?;
            Ok(no_rejections)
        }
        Mode::Reject => Ok(Outcome {
            rejected: ref_status
                .iter()
                .enumerate()
                .filter_map(|(idx, count)| (*count > 0).then_some(idx))
                .collect(),
        }),
    }
}

/// Port of `setup_alternate_shallow()` followed by `commit_shallow_file()`: the existing boundary
/// plus `extra` become the new `.git/shallow`, written under a lock.
fn write_boundary(shallow_file: &std::path::Path, existing: Vec<ObjectId>, extra: &[ObjectId]) -> Result<(), Error> {
    let lock = gix_lock::File::acquire_to_update_resource(shallow_file, gix_lock::acquire::Fail::Immediately, None)?;
    let updates: Vec<_> = extra
        .iter()
        .filter(|id| !existing.contains(id))
        .map(|id| gix_shallow::Update::Shallow(*id))
        .collect();
    gix_shallow::write(lock, nonempty::NonEmpty::from_vec(existing), &updates)?;
    Ok(())
}
