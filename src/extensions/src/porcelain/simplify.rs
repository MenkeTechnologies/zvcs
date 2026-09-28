//! git's history simplification — `revision.c`, one copy.
//!
//! Three passes run over a path-limited walk, in this order, and every command
//! that takes `-- <path>` needs all three to agree:
//!
//! 1. [`classify`] — `try_to_simplify_commit()`. Per commit: compare its tree
//!    against each parent's over the pathspec, decide the `TREESAME` flag, and
//!    (under default simplification) prune the parent list to the first parent
//!    it matched. `revs->dense` and `revs->simplify_history` both change what it
//!    does; [`Mode`] carries them.
//! 2. [`merge_simplify`] — `simplify_merges()` / `simplify_one()`, run only for
//!    `--simplify-merges`. Rewrites every parent to *its* simplification, drops
//!    the parents that add nothing, and collapses a commit onto its sole relevant
//!    parent when it is TREESAME to it. What survives is the set of commits that
//!    simplify to themselves.
//! 3. [`Ancestry::rewrite`] — `rewrite_parents()` / `rewrite_one()`, the display
//!    pass. A parent that is not shown is replaced by the first shown commit
//!    behind it, and a TREESAME root parent drops out of the list entirely.
//!
//! The tree comparison itself is not here: a caller supplies it through
//! [`TreeDiff`] so the pathspec engine, the rename settings and the diff cache a
//! command already owns are the ones used, rather than a second diff of this
//! module's own. What is shared is the *algorithm*, which is where the eight
//! private copies of the old ambiguity rule went wrong: each drifted separately.
//!
//! "Relevant" is git's `relevant_commit()` — `!(flags & UNINTERESTING)` for the
//! commits these callers see, since a `^<rev>`-excluded commit never enters the
//! walk. Every entry point therefore takes the walked set and reads relevance off
//! membership in it.
//!
//! A reflog walk (`-g`) is the exception: it has no walked set, so the three
//! passes collapse into the per-entry state machine [`ReflogWalk`], which runs
//! `try_to_simplify_commit()` and `rewrite_parents()` as each entry streams out.

use anyhow::Result;
use gix::hash::ObjectId;
use std::collections::{HashMap, HashSet};

/// `rev_compare_tree()`: whether the change from `parent` to `commit` touches the
/// caller's pathspec. `None` is git's "compare against the (non-existent) first
/// parent of a root", i.e. the empty tree.
pub(super) trait TreeDiff {
    fn differs(&mut self, commit: ObjectId, parent: Option<ObjectId>) -> Result<bool>;
}

/// The three `struct rev_info` fields `classify` reads.
#[derive(Clone, Copy)]
pub(super) struct Mode {
    /// `revs->dense`, cleared by `--sparse` and set by `--dense` (the default).
    /// When it is off, an ordinary one-parent commit is "always a change" and is
    /// never marked TREESAME — only merges and roots are still examined.
    pub dense: bool,
    /// `revs->simplify_history`, cleared by `--full-history` and by
    /// `--simplify-merges`. When it is on, the scan stops at the first parent the
    /// commit is TREESAME to and prunes the parent list to it; when it is off,
    /// every parent is compared and the per-parent verdicts are kept.
    pub simplify_history: bool,
    /// `revs->first_parent_only`. The scan compares parent 1 and stops, leaving
    /// the rest of the parent list in place but uninspected.
    pub first_parent: bool,
}

/// `try_to_simplify_commit()`'s verdict for one commit.
pub(super) struct Classified {
    /// `commit->parents` as the scan left it: the full list, except that default
    /// simplification prunes it to the one parent the commit is TREESAME to.
    pub parents: Vec<ObjectId>,
    /// `revs->treesame`, the per-parent decoration. Parallel to [`Self::parents`]
    /// when it exists at all; git only allocates it for a merge under
    /// `--full-history`, so it is empty otherwise and every reader defaults a
    /// missing entry to `false`.
    pub treesame_with: Vec<bool>,
    /// The `TREESAME` object flag.
    pub treesame: bool,
}

/// Port of `try_to_simplify_commit()` (revision.c) for one commit.
///
/// `walked` is the limited list — membership in it is `relevant_commit()`.
pub(super) fn classify(
    id: ObjectId,
    parents: &[ObjectId],
    walked: &HashSet<ObjectId>,
    mode: Mode,
    diff: &mut dyn TreeDiff,
) -> Result<Classified> {
    classify_with(id, parents, &|p| walked.contains(p), mode, diff)
}

/// [`classify`] with `relevant_commit()` as a predicate rather than membership in
/// a walked list — for a walk that has no such list, like the reflog walk, where
/// every commit short of an UNINTERESTING one is relevant.
pub(super) fn classify_with(
    id: ObjectId,
    parents: &[ObjectId],
    relevant: &dyn Fn(&ObjectId) -> bool,
    mode: Mode,
    diff: &mut dyn TreeDiff,
) -> Result<Classified> {
    if parents.is_empty() {
        // "Pretend as if we are comparing ourselves to the (non-existent) first
        // parent of this commit object": a root is TREESAME when its tree carries
        // nothing the pathspec names.
        let treesame = !diff.differs(id, None)?;
        return Ok(Classified { parents: Vec::new(), treesame_with: Vec::new(), treesame });
    }
    if !mode.dense && parents.len() == 1 {
        // "Normal non-merge commit? If we don't want to make the history dense,
        // we consider it always to be a change."
        return Ok(Classified {
            parents: parents.to_vec(),
            treesame_with: Vec::new(),
            treesame: false,
        });
    }

    let mut relevant_change = false;
    let mut irrelevant_change = false;
    let mut relevant_parents = 0usize;
    let mut treesame_with: Vec<bool> = Vec::new();

    for (nth, parent) in parents.iter().enumerate() {
        if relevant(parent) {
            relevant_parents += 1;
        }
        if nth == 1 {
            // Now we know this is a merge.
            if mode.first_parent {
                // "Do not compare with later parents when we care only about the
                // first parent chain" — the list itself is left alone.
                break;
            }
            if !mode.simplify_history {
                // `initialise_treesame()`, seeded from the first iteration.
                treesame_with = vec![false; parents.len()];
                treesame_with[0] = !(irrelevant_change || relevant_change);
            }
        }
        if !diff.differs(id, Some(*parent))? {
            // REV_TREE_SAME.
            if !mode.simplify_history || !relevant(parent) {
                // "Even if a merge with an uninteresting side branch brought the
                // entire change we are interested in, we do not want to lose the
                // other branches of this merge, so we just keep going."
                if let Some(slot) = treesame_with.get_mut(nth) {
                    *slot = true;
                }
                continue;
            }
            // Default simplification diverts the history onto this parent and
            // throws the rest of the list away.
            return Ok(Classified {
                parents: vec![*parent],
                treesame_with: Vec::new(),
                treesame: true,
            });
        }
        if relevant(parent) {
            relevant_change = true;
        } else {
            irrelevant_change = true;
        }
    }

    // "If we have any relevant parents, then we only consider TREESAMEness with
    // respect to them … Only if we have only irrelevant parents do we base
    // TREESAME on them."
    let treesame =
        if relevant_parents > 0 { !relevant_change } else { !irrelevant_change };
    Ok(Classified { parents: parents.to_vec(), treesame_with, treesame })
}

/// What `merge_simplify` leaves behind for the whole walk.
pub(super) struct MergeSimplified {
    /// `st->simplified`: the commit each walked commit collapses onto. A commit
    /// survives the pass — is in `revs->commits` afterwards — exactly when it maps
    /// to itself.
    pub simplified: HashMap<ObjectId, ObjectId>,
    /// `commit->parents` as `simplify_one()` rewrote it, for every walked commit.
    /// A commit that was simplified away still lends its list to the display pass.
    pub parents: HashMap<ObjectId, Vec<ObjectId>>,
    /// The `TREESAME` flag after `remove_marked_parents()`'s `update_treesame()`.
    pub treesame: HashMap<ObjectId, bool>,
}

impl MergeSimplified {
    /// Whether `id` survived the pass (`st->simplified == commit`).
    pub fn kept(&self, id: &ObjectId) -> bool {
        self.simplified.get(id) == Some(id)
    }
}

/// Port of `simplify_merges()` / `simplify_one()` (revision.c).
///
/// Each commit is replaced by its simplification: parents are rewritten to *their*
/// simplifications, then duplicates and parents that are ancestors of other parents
/// are dropped (as are root parents TREESAME to the empty tree), never dropping
/// every parent the commit is TREESAME to. A commit that still has a parent, is
/// TREESAME, and has exactly one relevant parent becomes that parent's
/// simplification. What survives is the set of commits that simplify to themselves.
///
/// Dropping parents can only make a commit *more* TREESAME, so the flag is
/// recomputed over the survivors — `relevant_parents ? !relevant_change :
/// !irrelevant_change`, which with everything relevant is "every surviving parent
/// is treesame". That recompute is what collapses a merge whose only remaining
/// parent it matches: in `I -> {side, A} -> M1`, `M1`'s `side` rewrites to `I`,
/// `I` is redundant beside `A`, and `M1` is TREESAME to the `A` that is left, so
/// `M1` becomes `A` — which is exactly what `git log --parents` shows, rewriting
/// the child merge's parent list from `M1` to `A`.
///
/// `--first-parent` only stops the *rewriting* at parent 1 (`if
/// (revs->first_parent_only) break;` in both of `simplify_one`'s loops, then
/// `cnt = 1`). The later parents stay in the list, unrewritten and uninspected,
/// and `%p`/`--parents` still print them.
pub(super) fn merge_simplify(
    repo: &gix::Repository,
    order: &[ObjectId],
    info: &HashMap<ObjectId, Classified>,
    first_parent: bool,
) -> Result<MergeSimplified> {
    // Relevance is `!(flags & UNINTERESTING)`; excluded commits never enter this
    // walk, so the walked set is exactly the relevant one.
    let walked: HashSet<ObjectId> = order.iter().copied().collect();
    let mut simplified: HashMap<ObjectId, ObjectId> = HashMap::with_capacity(order.len());
    let mut rewritten: HashMap<ObjectId, Vec<ObjectId>> = HashMap::with_capacity(order.len());
    let mut treesame_of: HashMap<ObjectId, bool> = HashMap::with_capacity(order.len());
    for id in order {
        if let Some(me) = info.get(id) {
            treesame_of.insert(*id, me.treesame);
        }
    }

    // git feeds the list reversed and re-queues whatever is not ready yet.
    let mut queue: Vec<ObjectId> = order.iter().rev().copied().collect();
    let mut guard = 0usize;
    while !queue.is_empty() {
        guard += 1;
        anyhow::ensure!(guard <= order.len() + 2, "simplify-merges did not converge");
        let mut next: Vec<ObjectId> = Vec::new();
        for id in std::mem::take(&mut queue) {
            if simplified.contains_key(&id) {
                continue;
            }
            let Some(me) = info.get(&id) else {
                simplified.insert(id, id);
                continue;
            };
            // A root simplifies to itself and its parents are not rewritten.
            if me.parents.is_empty() {
                simplified.insert(id, id);
                continue;
            }
            // Only the parents that matter have to be ready: under
            // `--first-parent` that is parent 1 alone.
            let considered: &[ObjectId] =
                if first_parent { &me.parents[..1] } else { &me.parents };
            let pending: Vec<ObjectId> = considered
                .iter()
                .copied()
                .filter(|p| walked.contains(p) && !simplified.contains_key(p))
                .collect();
            if !pending.is_empty() {
                next.extend(pending);
                next.push(id);
                continue;
            }

            // Rewrite each parent to its simplification, carrying its treesame bit.
            // Past parent 1, `--first-parent` leaves the entry exactly as it was.
            let mut parents: Vec<(ObjectId, bool)> = me
                .parents
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let target = if first_parent && i > 0 {
                        *p
                    } else {
                        simplified.get(p).copied().unwrap_or(*p)
                    };
                    (target, me.treesame_with.get(i).copied().unwrap_or(false))
                })
                .collect();

            let mut treesame = me.treesame;
            let cnt = if first_parent {
                1
            } else {
                // `remove_duplicate_parents`.
                let mut seen: HashSet<ObjectId> = HashSet::new();
                parents.retain(|(p, _)| seen.insert(*p));
                parents.len()
            };

            if cnt > 1 {
                let ids: Vec<ObjectId> = parents.iter().map(|(p, _)| *p).collect();
                // `mark_redundant_parents`: `reduce_heads()` over the parent list —
                // a parent reachable from another adds nothing.
                let mut marked: Vec<bool> = vec![false; parents.len()];
                for (i, p) in ids.iter().enumerate() {
                    let others: Vec<ObjectId> =
                        ids.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, q)| *q).collect();
                    if super::log::ancestor_closure(repo, &others)?.contains(p) {
                        marked[i] = true;
                    }
                }
                // `mark_treesame_root_parents`: a root parent that is TREESAME to
                // the empty tree contributed nothing either.
                for (i, (p, _)) in parents.iter().enumerate() {
                    if info.get(p).is_some_and(|s| s.parents.is_empty() && s.treesame) {
                        marked[i] = true;
                    }
                }
                // `leave_one_treesame_to_parent`: never drop every parent we are
                // TREESAME to — that is the path the default scan would follow.
                if marked.iter().any(|m| *m) {
                    let has_unmarked_treesame =
                        parents.iter().zip(&marked).any(|((_, ts), m)| *ts && !*m);
                    if !has_unmarked_treesame {
                        if let Some(i) =
                            parents.iter().zip(&marked).position(|((_, ts), m)| *ts && *m)
                        {
                            marked[i] = false;
                        }
                    }
                }
                if marked.iter().any(|m| *m) {
                    let mut it = marked.iter();
                    parents.retain(|_| !it.next().copied().unwrap_or(false));
                    // `remove_marked_parents`: "Removing parents can only increase
                    // TREESAMEness", so the flag is recomputed over the survivors.
                    treesame = parents.iter().all(|(_, ts)| *ts);
                }
            }

            // `one_relevant_parent`: for a single parent, or under
            // `--first-parent`, it is the first parent; else the sole relevant one.
            let relevant = one_relevant_parent(
                &parents.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
                &walked,
                first_parent,
            );

            let becomes = match relevant {
                Some(parent) if treesame && !parents.is_empty() => {
                    simplified.get(&parent).copied().unwrap_or(parent)
                }
                _ => id,
            };
            treesame_of.insert(id, treesame);
            rewritten.insert(id, parents.into_iter().map(|(p, _)| p).collect());
            simplified.insert(id, becomes);
        }
        queue = next;
    }

    Ok(MergeSimplified { simplified, parents: rewritten, treesame: treesame_of })
}

/// `one_relevant_parent()` (revision.c): the single parent a TREESAME commit may
/// safely collapse onto.
///
/// For a one-parent commit, or under `--first-parent`, it is the first parent
/// "even if not relevant by the above definition". For a merge it is the sole
/// relevant parent, and `None` when there are several or none.
pub(super) fn one_relevant_parent(
    parents: &[ObjectId],
    walked: &HashSet<ObjectId>,
    first_parent: bool,
) -> Option<ObjectId> {
    one_relevant_parent_with(parents, &|p| walked.contains(p), first_parent)
}

/// [`one_relevant_parent`] with `relevant_commit()` as a predicate.
fn one_relevant_parent_with(
    parents: &[ObjectId],
    is_relevant: &dyn Fn(&ObjectId) -> bool,
    first_parent: bool,
) -> Option<ObjectId> {
    if parents.is_empty() {
        return None;
    }
    if first_parent || parents.len() == 1 {
        return parents.first().copied();
    }
    let mut relevant = None;
    for p in parents {
        if is_relevant(p) {
            if relevant.is_some() {
                return None;
            }
            relevant = Some(*p);
        }
    }
    relevant
}

/// The state `rewrite_one()` reads: the walked set, and each walked commit's
/// TREESAME flag and parent list *as the earlier passes left them*.
pub(super) struct Ancestry<'a> {
    pub walked: &'a HashSet<ObjectId>,
    pub treesame: &'a HashMap<ObjectId, bool>,
    pub parents: &'a HashMap<ObjectId, Vec<ObjectId>>,
    pub first_parent: bool,
}

impl Ancestry<'_> {
    /// `rewrite_parents()` (revision.c): every parent is replaced by what
    /// [`Self::rewrite_one`] finds behind it, a parent that walks off the end of
    /// history is dropped, and the result is deduplicated.
    ///
    /// This is the *display* pass, so it runs on whatever the earlier passes left
    /// in the parent list — including a `--simplify-merges` rewrite, which git has
    /// already written into `commit->parents` by the time this is called.
    pub(super) fn rewrite(&self, parents: &[ObjectId]) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = Vec::with_capacity(parents.len());
        for p in parents {
            if let Some(id) = self.rewrite_one(*p) {
                out.push(id);
            }
        }
        // `remove_duplicate_parents()`: identity dedup, nothing cleverer.
        let mut seen: HashSet<ObjectId> = HashSet::new();
        out.retain(|p| seen.insert(*p));
        out
    }

    /// `rewrite_one_1()` (revision.c:4035-4054): walk back through TREESAME
    /// commits to the first one the output will show, or to a TREESAME merge with
    /// no single relevant parent to collapse onto. `None` is
    /// `rewrite_one_noparents` — the chain ran into a TREESAME root, which git
    /// drops from the parent list entirely.
    fn rewrite_one(&self, parent: ObjectId) -> Option<ObjectId> {
        let mut p = parent;
        // Each step moves strictly further back in a finite DAG, so the walk ends;
        // the counter only bounds a state map that a caller built inconsistently.
        for _ in 0..=self.parents.len() {
            if !self.walked.contains(&p) {
                // `flags & UNINTERESTING`.
                return Some(p);
            }
            if !self.treesame.get(&p).copied().unwrap_or(false) {
                return Some(p);
            }
            let grandparents = self.parents.get(&p).map(Vec::as_slice).unwrap_or(&[]);
            if grandparents.is_empty() {
                return None;
            }
            // `if (!(p = one_relevant_parent(revs, p->parents))) return
            // rewrite_one_ok;` (revision.c:4050-4051): a TREESAME merge with
            // several relevant parents is where the walk stops, and it stays in
            // the list as it is — only a parentless one is dropped.
            match one_relevant_parent(grandparents, self.walked, self.first_parent) {
                Some(next) => p = next,
                None => return Some(p),
            }
        }
        Some(p)
    }
}

/// `get_commit_action()`'s history-simplification arm: whether a commit survives
/// the display filter once `classify`/`merge_simplify` have had their say.
///
/// The filter only exists when a pathspec is in play (`revs->prune`) and the
/// history is dense; `--sparse` turns it off and every walked commit is shown.
/// A TREESAME commit is dropped, *except* that a caller which wants ancestry —
/// `--parents`, `--graph`, `--simplify-merges`, `--children` — keeps a merge
/// between two or more relevant commits, because that merge is what ties the
/// topology together.
pub(super) fn shows(
    treesame: bool,
    parents: &[ObjectId],
    walked: &HashSet<ObjectId>,
    prune: bool,
    dense: bool,
    want_ancestry: bool,
) -> bool {
    if !prune || !dense || !treesame {
        return true;
    }
    if !want_ancestry {
        return false;
    }
    parents.iter().filter(|p| walked.contains(*p)).count() >= 2
}

/// The commit state a reflog walk (`-g`) carries from one entry to the next.
///
/// A reflog walk has no limited list to classify up front: `get_revision_1()`
/// takes each entry from `next_reflog_entry()`, clears its `ADDED`, `SEEN` and
/// `SHOWN` flags and runs `try_to_simplify_commit()` on it (revision.c:4386-4420).
/// When the entry is shown and ancestry is wanted, `rewrite_parents()` walks
/// back through the *real* history behind it — `rewrite_one_1()` calls
/// `process_parents()`, and so `try_to_simplify_commit()`, on each commit it
/// passes, whether or not the reflog names it (revision.c:4035-4054). A commit
/// is relevant unless it is UNINTERESTING without being a BOTTOM
/// (revision.c:524-527). What those calls do is
/// written onto the commit objects and outlives the entry: the `TREESAME` flag,
/// the parent list `try_to_simplify_commit()` prunes and `rewrite_parents()`
/// rewrites in place, and — in `rev-list` — the list `finish_commit()` frees
/// after printing (builtin/rev-list.c:228-234). A commit the reflog names twice
/// meets the second time whatever the first left behind.
///
/// Whether a commit has been *parsed* matters as much. `try_to_simplify_commit()`
/// returns at once for a commit with no tree loaded (`repo_get_commit_tree()`
/// answers NULL until `parse_commit()` has run, revision.c:976-977), and an
/// unparsed commit's parent list is empty. Only the reflog entries themselves,
/// the parents `try_to_simplify_commit()` compares against, and the parents
/// `process_parents()` walks to are ever parsed — so the second parent of a
/// merge under `--first-parent`, which neither reaches, stays unsimplified and
/// `rewrite_parents()` keeps it as it is.
pub(super) struct ReflogWalk<'a> {
    repo: &'a gix::Repository,
    mode: Mode,
    /// `revs->limited`: `rewrite_one_1()` runs `process_parents()` only when it
    /// is off, so under it a commit outside the reflog is never simplified.
    limited: bool,
    /// Everything `limit_list()` painted UNINTERESTING.
    uninteresting: &'a HashSet<ObjectId>,
    /// The `^<rev>` tips, which carry BOTTOM as well and so stay relevant.
    bottoms: &'a HashSet<ObjectId>,
    /// `commit->parents`, once parsed.
    parents: HashMap<ObjectId, Vec<ObjectId>>,
    /// The `TREESAME` flag. `try_to_simplify_commit()` only ever sets it.
    treesame: HashSet<ObjectId>,
    /// The `ADDED` flag `process_parents()` checks and sets.
    added: HashSet<ObjectId>,
    /// `commit->object.parsed`.
    parsed: HashSet<ObjectId>,
}

impl<'a> ReflogWalk<'a> {
    pub(super) fn new(
        repo: &'a gix::Repository,
        mode: Mode,
        limited: bool,
        uninteresting: &'a HashSet<ObjectId>,
        bottoms: &'a HashSet<ObjectId>,
    ) -> Self {
        ReflogWalk {
            repo,
            mode,
            limited,
            uninteresting,
            bottoms,
            parents: HashMap::new(),
            treesame: HashSet::new(),
            added: HashSet::new(),
            parsed: HashSet::new(),
        }
    }

    /// `commit->parents` as the walk has left it, parsing the commit if needed.
    pub(super) fn parents(&mut self, id: ObjectId) -> &[ObjectId] {
        let repo = self.repo;
        self.parents
            .entry(id)
            .or_insert_with(|| super::rev_list::commit_parents(repo, id))
    }

    /// One entry out of `next_reflog_entry()`: `flags &= ~(ADDED | SEEN |
    /// SHOWN)`, then `try_to_simplify_commit()`.
    /// The entry was parsed when `next_reflog_commit()` looked it up.
    pub(super) fn pop(&mut self, id: ObjectId, diff: &mut dyn TreeDiff) -> Result<()> {
        self.parsed.insert(id);
        self.added.remove(&id);
        self.simplify(id, diff)
    }

    /// `try_to_simplify_commit()`: prunes the parent list in place and sets the
    /// flag; it never clears one set by an earlier pass.
    fn simplify(&mut self, id: ObjectId, diff: &mut dyn TreeDiff) -> Result<()> {
        if !self.parsed.contains(&id) {
            return Ok(());
        }
        let parents = self.parents(id).to_vec();
        let (uninteresting, bottoms) = (self.uninteresting, self.bottoms);
        // Each parent is parsed just before it is compared (revision.c:1032-1036).
        let mut compared = ParentsCompared { inner: diff, parents: Vec::new() };
        let classified = classify_with(
            id,
            &parents,
            &|p| relevant_commit(uninteresting, bottoms, p),
            self.mode,
            &mut compared,
        )?;
        self.parsed.extend(compared.parents);
        if classified.treesame {
            self.treesame.insert(id);
        }
        self.parents.insert(id, classified.parents);
        Ok(())
    }

    /// `process_parents()` with no queue: once per `ADDED`, and an UNINTERESTING
    /// commit is never simplified (revision.c:1124-1174).
    fn process_parents(&mut self, id: ObjectId, diff: &mut dyn TreeDiff) -> Result<()> {
        if !self.added.insert(id) || !self.parsed.contains(&id) {
            return Ok(());
        }
        if self.uninteresting.contains(&id) {
            // Every parent is parsed on the way to marking it
            // (revision.c:1147-1166).
            let parents = self.parents(id).to_vec();
            self.parsed.extend(parents);
            return Ok(());
        }
        self.simplify(id, diff)?;
        // The loop parses each parent it goes on to, stopping after the first
        // under `--first-parent` (revision.c:1181-1213).
        let take = if self.mode.first_parent { 1 } else { usize::MAX };
        let parents: Vec<ObjectId> = self.parents(id).iter().take(take).copied().collect();
        self.parsed.extend(parents);
        Ok(())
    }

    /// `get_commit_action()`'s `revs->prune && revs->dense` arm
    /// (revision.c:4221-4245): whether the TREESAME test lets the entry through.
    pub(super) fn shows(&mut self, id: ObjectId, want_ancestry: bool) -> bool {
        if !self.treesame.contains(&id) {
            return true;
        }
        if !want_ancestry {
            return false;
        }
        let (uninteresting, bottoms) = (self.uninteresting, self.bottoms);
        self.parents(id).iter().filter(|p| relevant_commit(uninteresting, bottoms, p)).count() >= 2
    }

    /// `rewrite_parents()` (revision.c:4072-4092), in place.
    pub(super) fn rewrite(&mut self, id: ObjectId, diff: &mut dyn TreeDiff) -> Result<()> {
        let mut out: Vec<ObjectId> = Vec::new();
        for parent in self.parents(id).to_vec() {
            if let Some(p) = self.rewrite_one(parent, diff)? {
                out.push(p);
            }
        }
        // `remove_duplicate_parents()`.
        let mut seen: HashSet<ObjectId> = HashSet::new();
        out.retain(|p| seen.insert(*p));
        self.parents.insert(id, out);
        Ok(())
    }

    /// `rewrite_one_1()` (revision.c:4035-4054). `None` is
    /// `rewrite_one_noparents`.
    fn rewrite_one(&mut self, mut p: ObjectId, diff: &mut dyn TreeDiff) -> Result<Option<ObjectId>> {
        loop {
            if !self.limited {
                self.process_parents(p, diff)?;
            }
            if self.uninteresting.contains(&p) || !self.treesame.contains(&p) {
                return Ok(Some(p));
            }
            let first_parent = self.mode.first_parent;
            let (uninteresting, bottoms) = (self.uninteresting, self.bottoms);
            let grandparents = self.parents(p);
            if grandparents.is_empty() {
                return Ok(None);
            }
            let relevant = |c: &ObjectId| relevant_commit(uninteresting, bottoms, c);
            match one_relevant_parent_with(grandparents, &relevant, first_parent) {
                Some(next) => p = next,
                None => return Ok(Some(p)),
            }
        }
    }

    /// `finish_commit()`'s `commit_list_free(commit->parents); commit->parents =
    /// NULL;` — `rev-list` drops a printed commit's parents for good.
    pub(super) fn free_parents(&mut self, id: ObjectId) {
        self.parents.insert(id, Vec::new());
    }
}

/// `relevant_commit()` (revision.c:524-527): `(flags & (UNINTERESTING | BOTTOM))
/// != UNINTERESTING`.
fn relevant_commit(
    uninteresting: &HashSet<ObjectId>,
    bottoms: &HashSet<ObjectId>,
    id: &ObjectId,
) -> bool {
    !uninteresting.contains(id) || bottoms.contains(id)
}

/// A [`TreeDiff`] that notes every parent it is asked about — the parents
/// `try_to_simplify_commit()` parses.
struct ParentsCompared<'d> {
    inner: &'d mut dyn TreeDiff,
    parents: Vec<ObjectId>,
}

impl TreeDiff for ParentsCompared<'_> {
    fn differs(&mut self, commit: ObjectId, parent: Option<ObjectId>) -> Result<bool> {
        if let Some(p) = parent {
            self.parents.push(p);
        }
        self.inner.differs(commit, parent)
    }
}
