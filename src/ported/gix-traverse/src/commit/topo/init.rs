use gix_hash::{ObjectId, oid};
use gix_revwalk::{PriorityQueue, graph::IdMap};

use crate::commit::{
    Info, Parents, Topo, find,
    topo::{Error, Sorting, WalkFlags, iter::gen_and_commit_time},
};

/// Drop repeated ids, keeping the first mention of each — the order git's pending list has after
/// its `SEEN` flag is applied. Order is load-bearing: it decides the seed order of the topological
/// queue and therefore how commits sharing a commit date come out.
fn dedup_first_wins(ids: &mut Vec<ObjectId>) {
    let mut seen = gix_hashtable::HashSet::<ObjectId>::default();
    ids.retain(|id| seen.insert(*id));
}

/// Builder for [`Topo`].
pub struct Builder<Find, Predicate> {
    commit_graph: Option<gix_commitgraph::Graph>,
    find: Find,
    predicate: Predicate,
    sorting: Sorting,
    parents: Parents,
    tips: Vec<ObjectId>,
    ends: Vec<ObjectId>,
}

impl<Find> Builder<Find, fn(&oid) -> bool>
where
    Find: gix_object::Find,
{
    /// Create a new `Builder` for a [`Topo`] that reads commits from a repository with `find`.
    /// starting at the `tips` and ending at the `ends`. Like `git rev-list
    /// --topo-order ^ends tips`.
    pub fn from_iters(
        find: Find,
        tips: impl IntoIterator<Item = impl Into<ObjectId>>,
        ends: Option<impl IntoIterator<Item = impl Into<ObjectId>>>,
    ) -> Self {
        Self::new(find).with_tips(tips).with_ends(ends.into_iter().flatten())
    }

    /// Create a new `Builder` for a [`Topo`] that reads commits from a
    /// repository with `find`.
    pub fn new(find: Find) -> Self {
        Self {
            commit_graph: Default::default(),
            find,
            sorting: Default::default(),
            parents: Default::default(),
            tips: Default::default(),
            ends: Default::default(),
            predicate: |_| true,
        }
    }

    /// Set a `predicate` to filter out revisions from the walk. Can be used to
    /// implement e.g. filtering on paths or time. This does *not* exclude the
    /// parent(s) of a revision that is excluded. Specify a revision as an 'end'
    /// if you want that behavior.
    pub fn with_predicate<Predicate>(self, predicate: Predicate) -> Builder<Find, Predicate>
    where
        Predicate: FnMut(&oid) -> bool,
    {
        Builder {
            commit_graph: self.commit_graph,
            find: self.find,
            sorting: self.sorting,
            parents: self.parents,
            tips: self.tips,
            ends: self.ends,
            predicate,
        }
    }
}

impl<Find, Predicate> Builder<Find, Predicate>
where
    Find: gix_object::Find,
    Predicate: FnMut(&oid) -> bool,
{
    /// Add commits to start reading from.
    ///
    /// The behavior is similar to specifying additional `ends` in `git rev-list --topo-order ^ends tips`.
    pub fn with_tips(mut self, tips: impl IntoIterator<Item = impl Into<ObjectId>>) -> Self {
        self.tips.extend(tips.into_iter().map(Into::into));
        self
    }

    /// Add commits ending the traversal.
    ///
    /// These commits themselves will not be read, i.e. the behavior is similar to specifying additional
    /// `ends` in `git rev-list --topo-order ^ends tips`.
    pub fn with_ends(mut self, ends: impl IntoIterator<Item = impl Into<ObjectId>>) -> Self {
        self.ends.extend(ends.into_iter().map(Into::into));
        self
    }

    /// Set the `sorting` to use for the topological walk.
    pub fn sorting(mut self, sorting: Sorting) -> Self {
        self.sorting = sorting;
        self
    }

    /// Specify how to handle commit `parents` during traversal.
    pub fn parents(mut self, parents: Parents) -> Self {
        self.parents = parents;
        self
    }

    /// Set or unset the `commit_graph` to use for the iteration.
    pub fn with_commit_graph(mut self, commit_graph: Option<gix_commitgraph::Graph>) -> Self {
        self.commit_graph = commit_graph;
        self
    }

    /// Build a new [`Topo`] instance.
    ///
    /// Note that merely building an instance is currently expensive.
    pub fn build(mut self) -> Result<Topo<Find, Predicate>, Error> {
        // git's `SEEN` flag: `prepare_revision_walk()` walks `revs->pending` and only inserts a
        // commit into `revs->commits` the first time it meets it, so naming the same commit twice
        // — two refs on one tip, `--all` next to an explicit branch, or a literal
        // `rev-list --topo-order main main` — contributes exactly one entry. Deduplicating here
        // rather than at each call site is what keeps `sort_in_topological_order`'s invariants
        // intact: a repeated seed used to be pushed onto `topo_queue` twice (yielding the commit
        // twice) *and* to be walked twice by `indegree_walk_step`, which bumped every parent's
        // indegree once too often and could strand it below the `== 1` gate in
        // `expand_topo_walk`.
        dedup_first_wins(&mut self.tips);
        dedup_first_wins(&mut self.ends);

        let mut w = Topo {
            commit_graph: self.commit_graph,
            find: self.find,
            predicate: self.predicate,
            indegrees: IdMap::default(),
            states: IdMap::default(),
            explore_queue: PriorityQueue::new(),
            indegree_queue: PriorityQueue::new(),
            topo_queue: super::iter::Queue::new(self.sorting),
            parents: self.parents,
            min_gen: gix_commitgraph::GENERATION_NUMBER_INFINITY,
            buf: vec![],
        };

        // Initial flags for the states of the tips and ends. All of them are
        // seen and added to the explore and indegree queues. The ends are by
        // definition (?) uninteresting and bottom.
        let tip_flags = WalkFlags::Seen | WalkFlags::Explored | WalkFlags::InDegree;
        let end_flags = tip_flags | WalkFlags::Uninteresting | WalkFlags::Bottom;

        for (id, flags) in self
            .tips
            .iter()
            .map(|id| (id, tip_flags))
            .chain(self.ends.iter().map(|id| (id, end_flags)))
        {
            // The same commit named as a tip *and* as an end — `rev-list --topo-order main ^main`,
            // or an `--all` that re-adds an excluded ref. git ORs `UNINTERESTING` onto the object
            // it already saw and leaves the queues alone; queueing it a second time here would
            // double-count its parents' indegrees just like a repeated tip would.
            if let Some(state) = w.states.get_mut(id) {
                *state |= flags;
                continue;
            }

            *w.indegrees.entry(*id).or_default() = 1;
            let commit = find(w.commit_graph.as_ref(), &w.find, id, &mut w.buf)?;
            let (generation, time) = gen_and_commit_time(commit)?;

            if generation < w.min_gen {
                w.min_gen = generation;
            }

            w.states.insert(*id, flags);
            w.explore_queue.insert((generation, time), *id);
            w.indegree_queue.insert((generation, time), *id);
        }

        // NOTE: Parents of the ends must also be marked uninteresting for some
        // reason. See handle_commit()
        for id in &self.ends {
            let parents = w.collect_all_parents(id)?;
            for (id, _) in parents {
                w.states
                    .entry(id)
                    .and_modify(|s| *s |= WalkFlags::Uninteresting)
                    .or_insert(WalkFlags::Uninteresting | WalkFlags::Seen);
            }
        }

        w.compute_indegrees_to_depth(w.min_gen)?;

        // NOTE: in Git the ends are also added to the topo_queue in addition to
        // the tips, but then in simplify_commit() Git is told to ignore it. For
        // now the tests pass.
        for id in self.tips.iter() {
            // git's `UNINTERESTING` is a flag on the *object*, not on the pending entry
            // that mentioned it, and a negative mention beats a positive one no matter
            // which order the two arrive in: `limit_list()` drops the commit before
            // `sort_in_topological_order()` ever sees it. Naming the same commit on both
            // sides — `git rev-list --topo-order main ^main`, or a `fast-export`/
            // `shortlog` whose `--all` re-adds a ref that was also excluded — therefore
            // yields nothing at all. The state map above already carries `end_flags` for
            // such a commit, because the `ends` are chained after the `tips` and
            // overwrite them; without this check the seeding loop would queue it anyway
            // and `pop_commit` hands out whatever the queue holds.
            let state = w.states.get(id).copied().unwrap_or_else(WalkFlags::empty);
            if state.contains(WalkFlags::Uninteresting) {
                continue;
            }

            let i = w.indegrees.get(id).ok_or(Error::MissingIndegreeUnexpected)?;

            if *i != 1 {
                continue;
            }

            let commit = find(w.commit_graph.as_ref(), &w.find, id, &mut w.buf)?;
            let (_, time) = gen_and_commit_time(commit)?;
            let parent_ids = w.collect_all_parents(id)?.into_iter().map(|e| e.0).collect();

            w.topo_queue.push(
                time,
                Info {
                    id: *id,
                    parent_ids,
                    commit_time: Some(time),
                },
            );
        }

        w.topo_queue.initial_sort();
        Ok(w)
    }
}
