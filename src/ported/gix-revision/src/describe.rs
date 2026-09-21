use std::{
    borrow::Cow,
    fmt::{Display, Formatter},
};

use bstr::BStr;
use gix_hashtable::HashMap;

/// The positive result produced by [describe()][function::describe()].
#[derive(Debug, Clone)]
pub struct Outcome<'name> {
    /// The name of the tag or branch that is closest to the commit `id`.
    ///
    /// If `None`, no name was found but it was requested to provide the `id` itself as fallback.
    pub name: Option<Cow<'name, BStr>>,
    /// The input commit object id that we describe.
    pub id: gix_hash::ObjectId,
    /// The number of commits that are between the tag or branch with `name` and `id`.
    /// These commits are all in the future of the named tag or branch.
    pub depth: u32,
    /// The mapping between object ids and their names initially provided by the describe call.
    pub name_by_oid: HashMap<gix_hash::ObjectId, Cow<'name, BStr>>,
    /// The amount of commits we traversed.
    pub commits_seen: u32,
    /// Every candidate the walk kept, ordered the way `git describe` orders them: by depth, then
    /// by the order they were found in. The first is the one `name` and `depth` come from.
    ///
    /// `git describe --debug` narrates this table on stderr, and there is no way to recompute it
    /// from `name` and `depth` alone.
    pub candidates: Vec<CandidateName<'name>>,
    /// The commit the walk stopped at because a candidate beyond `max_candidates` was found there,
    /// which is `git describe --debug`'s "gave up search at".
    pub gave_up_on: Option<gix_hash::ObjectId>,
    /// The commit the walk stopped at because the best candidates already cover it, which is
    /// `git describe --debug`'s "finished search at".
    pub finished_search_at: Option<gix_hash::ObjectId>,
}

/// One entry of [`Outcome::candidates`]: a name the walk found and how far the described commit is
/// in front of it.
#[derive(Debug, Clone)]
pub struct CandidateName<'name> {
    /// The commit the name was found on, which is the key it had in `name_by_oid`.
    pub id: gix_hash::ObjectId,
    /// The name, as it was given in `name_by_oid`.
    pub name: Cow<'name, BStr>,
    /// The number of commits between this name and the commit being described.
    pub depth: u32,
}

impl<'a> Outcome<'a> {
    /// Turn this outcome into a structure that can display itself in the typical `git describe` format.
    pub fn into_format(self, hex_len: usize) -> Format<'a> {
        Format {
            name: self.name,
            id: self.id,
            hex_len,
            depth: self.depth,
            long: false,
            dirty_suffix: None,
        }
    }
}

/// A structure implementing `Display`, producing a `git describe` like string.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
pub struct Format<'a> {
    /// The name of the branch or tag to display, as is.
    ///
    /// If `None`, the `id` will be displayed as a fallback.
    pub name: Option<Cow<'a, BStr>>,
    /// The `id` of the commit to describe.
    pub id: gix_hash::ObjectId,
    /// The amount of hex characters to use to display `id`.
    pub hex_len: usize,
    /// The amount of commits between `name` and `id`, where `id` is in the future of `name`.
    pub depth: u32,
    /// If true, the long form of the describe string will be produced even if `id` lies directly on `name`,
    /// hence has a depth of 0.
    pub long: bool,
    /// If `Some(suffix)`, it will be appended to the describe string.
    /// This should be set if the working tree was determined to be dirty.
    pub dirty_suffix: Option<String>,
}

impl Format<'_> {
    /// Return true if the `name` is directly associated with `id`, i.e. there are no commits between them.
    pub fn is_exact_match(&self) -> bool {
        self.depth == 0
    }

    /// Set this instance to print in long mode, that is if `depth` is 0, it will still print the whole
    /// long form even though it's not quite necessary.
    ///
    /// Otherwise, it is allowed to shorten itself.
    pub fn long(&mut self, long: bool) -> &mut Self {
        self.long = long;
        self
    }
}

impl Display for Format<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(name) = self.name.as_deref() {
            if !self.long && self.is_exact_match() {
                name.fmt(f)?;
            } else {
                write!(f, "{}-{}-g{}", name, self.depth, self.id.to_hex_with_len(self.hex_len))?;
            }
        } else {
            self.id.to_hex_with_len(self.hex_len).fmt(f)?;
        }

        if let Some(suffix) = &self.dirty_suffix {
            write!(f, "-{suffix}")?;
        }
        Ok(())
    }
}

/// A bit-field which keeps track of which commit is reachable by one of 32 candidates names.
pub type Flags = u32;
const MAX_CANDIDATES: usize = std::mem::size_of::<Flags>() * 8;

/// The options required to call [`describe()`][function::describe()].
#[derive(Clone, Debug)]
pub struct Options<'name> {
    /// The candidate names from which to determine the `name` to use for the describe string,
    /// as a mapping from a commit id and the name associated with it.
    pub name_by_oid: HashMap<gix_hash::ObjectId, Cow<'name, BStr>>,
    /// The amount of names we will keep track of. Defaults to the maximum of 32.
    ///
    /// If the number is exceeded, it will be capped at 32 and defaults to 10.
    pub max_candidates: usize,
    /// If no candidate for naming, always show the abbreviated hash. Default: false.
    pub fallback_to_oid: bool,
    /// Only follow the first parent during graph traversal. Default: false.
    ///
    /// This may speed up the traversal at the cost of accuracy.
    pub first_parent: bool,
    /// The size of git's `names` hashmap, which is *not* always `name_by_oid.len()`.
    ///
    /// `describe_commit()` stops the walk as soon as it has collected a candidate for every known
    /// name (`builtin/describe.c:418-422`):
    ///
    /// ```c
    /// if (match_cnt == max_candidates ||
    ///     match_cnt == hashmap_get_size(&names)) {
    ///         gave_up_on = c;
    ///         break;
    /// }
    /// ```
    ///
    /// `names` is filled by `get_name()` for *every* ref the `--match`/`--exclude` filters admit,
    /// including the lightweight tags that a default (annotated-only) describe refuses to turn
    /// into candidates — it "still remember[s] lightweight ones, only to give hints in an error
    /// message" (`builtin/describe.c:219-222`). So without `--tags`/`--all` that hashmap is larger
    /// than `name_by_oid`, and using `name_by_oid.len()` here would stop the walk too early.
    ///
    /// `None` means "the same as `name_by_oid.len()`", which is right whenever the caller admits
    /// every filtered ref into the map.
    pub names_total: Option<usize>,
    /// The subset of `name_by_oid` whose names are *annotated* tags — git's `n->prio == 2`.
    ///
    /// The walk's early stop is guarded by the number of annotated candidates, not by the number
    /// of candidates (`builtin/describe.c:446`):
    ///
    /// ```c
    /// /* Stop if last remaining path already covered by best candidate(s) */
    /// if (annotated_cnt && lazy_queue_empty(&queue)) {
    /// ```
    ///
    /// and `annotated_cnt` only counts the candidates git took from an annotated tag
    /// (`builtin/describe.c:436-437`). Under `--tags` a lightweight tag is a perfectly good
    /// candidate that does *not* arm that stop, so the walk keeps going until an annotated one
    /// turns up — which is how `describe --tags` finds the annotated tag behind a lightweight one
    /// and reports both in `--debug`.
    ///
    /// `None` means "every name counts as annotated", which is what a caller that cannot tell
    /// them apart gets.
    pub annotated_names: Option<gix_hashtable::HashSet>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            max_candidates: 10, // the same number as git uses, otherwise we perform worse by default on big repos
            name_by_oid: Default::default(),
            fallback_to_oid: false,
            first_parent: false,
            names_total: None,
            annotated_names: None,
        }
    }
}

/// The error returned by the [`describe()`](function::describe()) function.
pub type Error = gix_error::Message;

pub(crate) mod function {
    use std::{borrow::Cow, cmp::Ordering};

    use bstr::BStr;
    use gix_error::{Exn, ResultExt, message};
    use gix_hash::oid;

    use super::{CandidateName, Error, Outcome};
    use crate::{
        Graph, PriorityQueue,
        describe::{CommitTime, Flags, MAX_CANDIDATES, Options},
    };

    /// Given a `commit` id, traverse the commit `graph` and collect candidate names from the `name_by_oid` mapping to produce
    /// an `Outcome`, which converted [`into_format()`](Outcome::into_format()) will produce a typical `git describe` string.
    ///
    /// Note that the `name_by_oid` map is returned in the [`Outcome`], which can be forcefully returned even if there was no matching
    /// candidate by setting `fallback_to_oid` to true.
    pub fn describe<'name>(
        commit: &oid,
        graph: &mut Graph<'_, '_, Flags>,
        Options {
            name_by_oid,
            mut max_candidates,
            fallback_to_oid,
            first_parent,
            names_total,
            annotated_names,
        }: Options<'name>,
    ) -> Result<Option<Outcome<'name>>, Exn<Error>> {
        let _span = gix_trace::coarse!(
            "gix_revision::describe()",
            commit = %commit,
            name_count = name_by_oid.len(),
            max_candidates,
            first_parent
        );
        max_candidates = max_candidates.min(MAX_CANDIDATES);
        if let Some(name) = name_by_oid.get(commit) {
            return Ok(Some(Outcome {
                name: name.clone().into(),
                id: commit.to_owned(),
                depth: 0,
                name_by_oid,
                commits_seen: 0,
                candidates: Vec::new(),
                gave_up_on: None,
                finished_search_at: None,
            }));
        }

        if max_candidates == 0 || name_by_oid.is_empty() {
            return if fallback_to_oid {
                Ok(Some(Outcome {
                    id: commit.to_owned(),
                    name: None,
                    name_by_oid,
                    depth: 0,
                    commits_seen: 0,
                    candidates: Vec::new(),
                    gave_up_on: None,
                    finished_search_at: None,
                }))
            } else {
                Ok(None)
            };
        }

        let mut queue = PriorityQueue::from_iter(Some((u32::MAX, commit.to_owned())));
        let mut candidates = Vec::new();
        // git's `annotated_cnt`; see `Options::annotated_names`.
        let mut annotated_cnt = 0usize;
        let mut commits_seen = 0;
        let mut gave_up_on_commit = None;
        // `git describe --debug`'s "finished search at <oid>": the commit the early-stop below
        // broke on (`builtin/describe.c:396-401`).
        let mut finished_search_at = None;
        graph.clear();
        graph.insert(commit.to_owned(), 0u32);

        // `hashmap_get_size(&names)` in git's terms; see `Options::names_total`.
        let names_total = names_total.unwrap_or(name_by_oid.len());

        while let Some((commit_date, commit)) = queue.pop() {
            commits_seen += 1;

            // ```c
            // if (match_cnt == max_candidates ||
            //     match_cnt == hashmap_get_size(&names)) {
            //         gave_up_on = c;
            //         break;
            // }
            // ```
            //
            // (`builtin/describe.c:418-422`.) This sits at the *top* of the loop body, right
            // after `seen_commits++` and before the commit is looked up in the name map, so the
            // commit the walk gives up on — `git describe --debug`'s "gave up search at" — is the
            // first commit popped after the table filled, named or not. Testing it only on a
            // commit that carries a name (which is what this walk used to do) reports a later
            // commit and counts the ones in between.
            //
            // The second arm has no equivalent at all outside this check: once every known name
            // is already a candidate there is nothing left to find, so git stops instead of
            // walking the rest of the history. Without it, `describe --tags --match 'test*'` on a
            // merge answered a depth one larger than stock's, because the walk kept going and
            // kept incrementing every candidate's depth.
            if candidates.len() == max_candidates || candidates.len() == names_total {
                gave_up_on_commit = Some((commit_date, commit));
                break;
            }

            let flags = if let Some(name) = name_by_oid.get(&commit) {
                // `else if (match_cnt < max_candidates)` (`builtin/describe.c:429`); the break
                // above already guarantees it, exactly as it does in git.
                let identity_bit = 1 << candidates.len();
                candidates.push(Candidate {
                    id: commit,
                    name: name.clone(),
                    commits_in_its_future: commits_seen - 1,
                    identity_bit,
                    order: candidates.len(),
                });
                // `if (n->prio == 2) annotated_cnt++;` (`builtin/describe.c:436-437`).
                if annotated_names.as_ref().is_none_or(|set| set.contains(&commit)) {
                    annotated_cnt += 1;
                }
                let flags = graph.get_mut(&commit).expect("inserted");
                *flags |= identity_bit;
                *flags
            } else {
                graph[&commit]
            };

            for candidate in candidates
                .iter_mut()
                .filter(|c| (flags & c.identity_bit) != c.identity_bit)
            {
                candidate.commits_in_its_future += 1;
            }

            // `if (annotated_cnt && lazy_queue_empty(&queue))` (`builtin/describe.c:445-446`):
            // a single-trunk history that waits to be replenished. Abort early if the best
            // candidate is in the current commit's past — but only once an *annotated* candidate
            // exists, which is not the same as "any candidate exists" under `--tags`/`--all`.
            if annotated_cnt > 0 && queue.is_empty() {
                let mut shortest_depth = Flags::MAX;
                let mut best_candidates_at_same_depth = 0_u32;
                for candidate in &candidates {
                    match candidate.commits_in_its_future.cmp(&shortest_depth) {
                        Ordering::Less => {
                            shortest_depth = candidate.commits_in_its_future;
                            best_candidates_at_same_depth = candidate.identity_bit;
                        }
                        Ordering::Equal => {
                            best_candidates_at_same_depth |= candidate.identity_bit;
                        }
                        Ordering::Greater => {}
                    }
                }

                if (flags & best_candidates_at_same_depth) == best_candidates_at_same_depth {
                    finished_search_at = Some(commit);
                    break;
                }
            }

            parents_by_date_onto_queue_and_track_names(graph, &mut queue, commit, flags, first_parent)?;
        }

        if candidates.is_empty() {
            return if fallback_to_oid {
                Ok(Some(Outcome {
                    id: commit.to_owned(),
                    name: None,
                    name_by_oid,
                    depth: 0,
                    commits_seen,
                    candidates: Vec::new(),
                    gave_up_on: gave_up_on_commit.map(|(_, id)| id),
                    finished_search_at,
                }))
            } else {
                Ok(None)
            };
        }

        candidates.sort_by(|a, b| {
            a.commits_in_its_future
                .cmp(&b.commits_in_its_future)
                .then_with(|| a.order.cmp(&b.order))
        });

        // ```c
        // if (gave_up_on) {
        //         lazy_queue_put(&queue, gave_up_on);
        //         seen_commits--;
        // }
        // ```
        //
        // (`builtin/describe.c:499-502`.) `lazy_queue_put` on a pending get is
        // `prio_queue_replace`, which drops the commit back in under its own date and a *fresh*
        // insertion counter (`prio-queue.c:105-115`) — so among commits of equal date it comes
        // out last, not first. Re-queuing it at `u32::MAX` popped it ahead of its equal-dated
        // siblings instead, which counted one commit too many into the best candidate's depth in
        // a repository whose commits share a timestamp.
        if let Some((commit_date, commit_id)) = gave_up_on_commit {
            queue.insert(commit_date, commit_id);
            commits_seen -= 1;
        }

        commits_seen += finish_depth_computation(
            queue,
            graph,
            candidates.first_mut().expect("at least one candidate"),
            first_parent,
        )?;

        let table: Vec<CandidateName<'name>> = candidates
            .iter()
            .map(|c| CandidateName {
                id: c.id,
                name: c.name.clone(),
                depth: c.commits_in_its_future,
            })
            .collect();

        Ok(candidates.into_iter().next().map(|c| Outcome {
            name: c.name.into(),
            id: commit.to_owned(),
            depth: c.commits_in_its_future,
            name_by_oid,
            commits_seen,
            candidates: table,
            gave_up_on: gave_up_on_commit.map(|(_, id)| id),
            finished_search_at,
        }))
    }

    fn parents_by_date_onto_queue_and_track_names(
        graph: &mut Graph<'_, '_, Flags>,
        queue: &mut PriorityQueue<CommitTime, gix_hash::ObjectId>,
        commit: gix_hash::ObjectId,
        commit_flags: Flags,
        first_parent: bool,
    ) -> Result<(), Exn<Error>> {
        graph
            .insert_parents(
                &commit,
                &mut |parent_id, parent_commit_date| {
                    queue.insert(parent_commit_date as u32, parent_id);
                    commit_flags
                },
                &mut |_parent_id, flags| *flags |= commit_flags,
                first_parent,
            )
            .or_raise(|| message!("could not insert parents of commit {} into graph", commit.to_hex()))?;
        Ok(())
    }

    /// `finish_depth_computation()` (`builtin/describe.c:290-332`).
    ///
    /// ```c
    /// struct oidset unflagged = OIDSET_INIT;
    ///
    /// for (size_t i = queue->get_pending ? 1 : 0; i < queue->queue.nr; i++) {
    ///         struct commit *commit = queue->queue.array[i].data;
    ///         if (!(commit->object.flags & best->flag_within))
    ///                 oidset_insert(&unflagged, &commit->object.oid);
    /// }
    ///
    /// while (!lazy_queue_empty(queue)) {
    ///         struct commit *c = lazy_queue_get(queue);
    ///         struct commit_list *parents = c->parents;
    ///         seen_commits++;
    ///         if (c->object.flags & best->flag_within) {
    ///                 if (!oidset_size(&unflagged))
    ///                         break;
    ///         } else {
    ///                 oidset_remove(&unflagged, &c->object.oid);
    ///                 best->depth++;
    ///         }
    ///         while (parents) {
    ///                 unsigned seen, flag_before, flag_after;
    ///                 struct commit *p = parents->item;
    ///                 repo_parse_commit(the_repository, p);
    ///                 seen = p->object.flags & SEEN;
    ///                 if (!seen)
    ///                         lazy_queue_put(queue, p);
    ///                 flag_before = p->object.flags & best->flag_within;
    ///                 p->object.flags |= c->object.flags;
    ///                 flag_after = p->object.flags & best->flag_within;
    ///                 if (!seen && !flag_after)
    ///                         oidset_insert(&unflagged, &p->object.oid);
    ///                 if (seen && !flag_before && flag_after)
    ///                         oidset_remove(&unflagged, &p->object.oid);
    ///                 parents = parents->next;
    ///         }
    /// }
    /// ```
    ///
    /// The set is the point. "Is the best candidate already an ancestor of everything still
    /// pending?" is asked once per pop, and git answers it from a set it maintains as the walk
    /// runs rather than by rescanning the queue — because a commit that has already been popped
    /// *unflagged* has to stop counting, while a commit still queued that later inherits the bit
    /// has to stop counting too. Rescanning the live queue answers a different question and stops
    /// the walk late: on a merge whose second parent is unflagged, the extra iterations kept
    /// incrementing `best->depth`, so `describe --tags --match 'test*'` printed a depth one larger
    /// than stock's.
    fn finish_depth_computation(
        mut queue: PriorityQueue<CommitTime, gix_hash::ObjectId>,
        graph: &mut Graph<'_, '_, Flags>,
        best_candidate: &mut Candidate<'_>,
        first_parent: bool,
    ) -> Result<u32, Exn<Error>> {
        let bit = best_candidate.identity_bit;
        // `RefCell` only because `insert_parents` takes its two callbacks at once and both of
        // them maintain this set, exactly as the one C loop body does.
        let unflagged: std::cell::RefCell<gix_hashtable::HashSet> = std::cell::RefCell::new(
            queue.iter_unordered().filter(|id| (graph[*id] & bit) != bit).copied().collect(),
        );

        let mut commits_seen = 0;
        while let Some(commit) = queue.pop_value() {
            commits_seen += 1;
            let flags = graph[&commit];
            if (flags & bit) == bit {
                if unflagged.borrow().is_empty() {
                    break;
                }
            } else {
                unflagged.borrow_mut().remove(&commit);
                best_candidate.commits_in_its_future += 1;
            }

            graph
                .insert_parents(
                    &commit,
                    &mut |parent_id, parent_commit_date| {
                        queue.insert(parent_commit_date as u32, parent_id);
                        if (flags & bit) != bit {
                            unflagged.borrow_mut().insert(parent_id);
                        }
                        flags
                    },
                    &mut |parent_id, parent_flags| {
                        let before = *parent_flags & bit;
                        *parent_flags |= flags;
                        if before != bit && (*parent_flags & bit) == bit {
                            unflagged.borrow_mut().remove(&parent_id);
                        }
                    },
                    first_parent,
                )
                .or_raise(|| {
                    message!("could not insert parents of commit {} into graph", commit.to_hex())
                })?;
        }
        Ok(commits_seen)
    }

    #[derive(Debug)]
    struct Candidate<'a> {
        /// The commit this candidate name sits on.
        id: gix_hash::ObjectId,
        name: Cow<'a, BStr>,
        commits_in_its_future: Flags,
        /// A single bit identifying this candidate uniquely in a bitset
        identity_bit: Flags,
        /// The order at which we found the candidate, first one has order = 0
        order: usize,
    }
}

/// The timestamp for the creation date of a commit in seconds since unix epoch.
type CommitTime = u32;
