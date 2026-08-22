use std::{
    cmp::{Ordering, Reverse},
    collections::VecDeque,
};

use gix_date::SecondsSinceUnixEpoch;
use gix_hash::ObjectId;
use smallvec::SmallVec;

#[derive(Default, Debug, Copy, Clone)]
/// The order with which to prioritize the search.
pub enum CommitTimeOrder {
    #[default]
    /// Sort commits by newest first.
    NewestFirst,
    /// Sort commits by oldest first.
    #[doc(alias = "Sort::REVERSE", alias = "git2")]
    OldestFirst,
}

/// Specify how to sort commits during a [simple](super::Simple) traversal.
///
/// ### Sample History
///
/// The following history will be referred to for explaining how the sort order works, with the number denoting the commit timestamp
/// (*their X-alignment doesn't matter*).
///
/// ```text
/// ---1----2----4----7 <- second parent of 8
///     \              \
///      3----5----6----8---
/// ```
#[derive(Default, Debug, Copy, Clone)]
pub enum Sorting {
    /// Commits are sorted as they are mentioned in the commit graph.
    ///
    /// In the *sample history* the order would be `8, 6, 7, 5, 4, 3, 2, 1`.
    ///
    /// ### Note
    ///
    /// This is not to be confused with `git log/rev-list --topo-order`, which is notably different from
    /// as it avoids overlapping branches.
    #[default]
    BreadthFirst,
    /// Commits are sorted by their commit time in the order specified, either newest or oldest first.
    ///
    /// The sorting applies to all currently queued commit ids and thus is full.
    ///
    /// In the *sample history* the order would be `8, 7, 6, 5, 4, 3, 2, 1` for [`NewestFirst`](CommitTimeOrder::NewestFirst),
    /// or `1, 2, 3, 4, 5, 6, 7, 8` for [`OldestFirst`](CommitTimeOrder::OldestFirst).
    ///
    /// # Performance
    ///
    /// This mode benefits greatly from having an object_cache in `find()`
    /// to avoid having to lookup each commit twice.
    ByCommitTime(CommitTimeOrder),
    /// This sorting is similar to [`ByCommitTime`](Sorting::ByCommitTime), but adds a cutoff to not return commits older than
    /// a given time, stopping the iteration once no younger commits is queued to be traversed.
    ///
    /// As the query is usually repeated with different cutoff dates, this search mode benefits greatly from an object cache.
    ///
    /// In the *sample history* and a cut-off date of 4, the returned list of commits would be `8, 7, 6, 4`.
    ByCommitTimeCutoff {
        /// The order in which to prioritize lookups.
        order: CommitTimeOrder,
        /// The number of seconds since unix epoch, the same value obtained by any `gix_date::Time` structure and the way git counts time.
        seconds: gix_date::SecondsSinceUnixEpoch,
    },
}

/// The error is part of the item returned by the [Ancestors](super::Simple) iterator.
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Find(#[from] gix_object::find::existing_iter::Error),
    #[error(transparent)]
    ObjectDecode(#[from] gix_object::decode::Error),
    #[error(transparent)]
    HiddenGraph(#[from] gix_revwalk::graph::get_or_insert_default::Error),
}

use Result as Either;

type QueueKey<T> = Either<T, Reverse<T>>;
type CommitDateQueue = gix_revwalk::PriorityQueue<QueueKey<SecondsSinceUnixEpoch>, ObjectId>;

bitflags::bitflags! {
    #[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
    struct PaintFlags: u8 {
        const VISIBLE = 1 << 0;
        const HIDDEN = 1 << 1;
    }
}

/// Priority for hidden-frontier painting that prefers newer commits, using generation numbers
/// when available and falling back to commit time as a tie-breaker.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct GenThenTime {
    generation: gix_revwalk::graph::Generation,
    time: SecondsSinceUnixEpoch,
}

impl From<&gix_revwalk::graph::Commit<PaintFlags>> for GenThenTime {
    fn from(commit: &gix_revwalk::graph::Commit<PaintFlags>) -> Self {
        GenThenTime {
            generation: commit.generation.unwrap_or(gix_commitgraph::GENERATION_NUMBER_INFINITY),
            time: commit.commit_time,
        }
    }
}

impl Ord for GenThenTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.generation.cmp(&other.generation).then(self.time.cmp(&other.time))
    }
}

impl PartialOrd<Self> for GenThenTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The state used and potentially shared by multiple graph traversals.
#[derive(Clone)]
pub(super) struct State {
    /// Pending visible commits when traversal is driven in insertion/topological order.
    ///
    /// This queue is consumed by `next_by_topology()`, and also becomes the active frontier for
    /// first-parent traversal after any time-ordered queue is flattened back into FIFO order.
    next: VecDeque<ObjectId>,
    /// Pending visible commits when traversal is driven by commit date.
    ///
    /// This queue is consumed by `next_by_commit_date()`. It holds the same logical frontier as
    /// `next`, but keeps it ordered by commit time instead of insertion order.
    queue: CommitDateQueue,
    /// Backing storage for the currently yielded commit.
    buf: Vec<u8>,
    /// The object hash kind of the currently yielded commit data in `buf`.
    /// It's used to know the kind of hash to expect when a new iterator is returned from `buf`
    /// via `Simple::commit_iter()`.
    object_hash: gix_hash::Kind,
    /// Set of commits that were already enqueued for the visible traversal, for cycle-checking.
    seen: gix_hashtable::HashSet<ObjectId>,
    /// Hidden frontier commits that must not be yielded or crossed during traversal.
    hidden: gix_revwalk::graph::IdMap<()>,
    /// Hidden input tips from which the hidden frontier is derived.
    ///
    /// These are consumed on the first call ot `next` to compute the hidden frontier once.
    hidden_tips: Vec<ObjectId>,
    /// Scratch buffer for parent commit lookups when commit times are loaded from the object database.
    parents_buf: Vec<u8>,
    /// Reusable parent id/time storage populated from the commit-graph cache.
    parent_ids: SmallVec<[(ObjectId, SecondsSinceUnixEpoch); 2]>,
}

fn to_queue_key(i: i64, order: CommitTimeOrder) -> QueueKey<i64> {
    match order {
        CommitTimeOrder::NewestFirst => Ok(i),
        CommitTimeOrder::OldestFirst => Err(Reverse(i)),
    }
}

/// Compute the boundary at which the visible walk must stop because commits become reachable from
/// both the visible tips and the hidden tips.
///
/// The algorithm performs a merge-base-style paint in a temporary `gix_revwalk::Graph`:
/// visible tips are marked with `VISIBLE`, hidden tips with `HIDDEN`, and these flags are
/// propagated to parents in generation/time order. A commit carrying both flags is part of the
/// overlap between the two histories, which is what the visible traversal must not cross.
///
/// Both the painting and its termination are git's `limit_list()`:
///
/// * a commit that gains `HIDDEN` hands it to every ancestor the graph has already seen, which is
///   `mark_parents_uninteresting()` — the recursive descent is what catches a commit that was
///   painted `VISIBLE` earlier on a path the hidden side reaches only now,
/// * the loop leaves only when a *hidden* commit was popped and no visible commit is queued any
///   more, which is `still_interesting()`. Leaving on a visible pop is what git never does, and
///   what an earlier version of this function did: `git show ^HEAD HEAD~2 HEAD^0` popped `HEAD~2`
///   last, stopped with the hidden paint still one commit short of it, and printed it plus its
///   ancestors where stock git prints nothing at all.
///
/// The returned set is not all commits reachable from hidden tips: hidden-only history the visible
/// tips can never reach stays out of it. The actual `Simple` walk then skips the commits in the set
/// and refuses to enqueue parents across them.
fn compute_hidden_frontier(
    visible_tips: &[ObjectId],
    hidden_tips: &[ObjectId],
    objects: &impl gix_object::Find,
    cache: Option<&gix_commitgraph::Graph>,
    grafts: Option<std::sync::Arc<gix_revwalk::graft::Table>>,
    predicate: &mut impl FnMut(&gix_hash::oid) -> bool,
) -> Result<gix_revwalk::graph::IdMap<()>, Error> {
    // The painting has to see the same parents the visible walk will, or it paints
    // a frontier the walk can never reach.
    let mut graph = gix_revwalk::Graph::<gix_revwalk::graph::Commit<PaintFlags>>::new(objects, cache).with_grafts(grafts);
    // The queue value carries whether the entry was still visible-only when it was queued, so the
    // loop below can test "is any visible commit still pending" in constant time instead of
    // scanning the whole queue on every iteration. A commit that gains `HIDDEN` after it was
    // queued is queued again with the flag, so the count is an over-estimate at worst — which
    // only ever means popping a few entries more than strictly needed, never stopping too early.
    let mut queue = gix_revwalk::PriorityQueue::<GenThenTime, (ObjectId, bool)>::new();
    let mut visible_pending = 0usize;

    for &visible in visible_tips {
        graph.get_or_insert_full_commit(visible, |commit| {
            commit.data |= PaintFlags::VISIBLE;
            let visible_only = !commit.data.contains(PaintFlags::HIDDEN);
            visible_pending += usize::from(visible_only);
            queue.insert(GenThenTime::from(&*commit), (visible, visible_only));
        })?;
    }
    for &hidden in hidden_tips {
        graph.get_or_insert_full_commit(hidden, |commit| {
            commit.data |= PaintFlags::HIDDEN;
            queue.insert(GenThenTime::from(&*commit), (hidden, false));
        })?;
    }

    while let Some((_info, (commit_id, was_visible_only))) = queue.pop() {
        visible_pending -= usize::from(was_visible_only);
        let commit = graph.get_mut(&commit_id).expect("queued commits are in the graph");
        let flags = commit.data;

        for parent_id in commit.parents.clone() {
            // The same predicate the visible walk consults, which is where the
            // caller expresses a graft: a shallow boundary commit's parents are
            // deliberately absent from the object database, and reading one is
            // the error this painting pass used to fail with. git never asks for
            // them either — its rev-list sees the boundary through
            // `--shallow-file` and treats the commit as parentless.
            if !predicate(&parent_id) {
                continue;
            }
            let mut newly_hidden = false;
            graph.get_or_insert_full_commit(parent_id, |parent| {
                if (parent.data & flags) != flags {
                    newly_hidden = flags.contains(PaintFlags::HIDDEN) && !parent.data.contains(PaintFlags::HIDDEN);
                    parent.data |= flags;
                    let visible_only = !parent.data.contains(PaintFlags::HIDDEN);
                    visible_pending += usize::from(visible_only);
                    queue.insert(GenThenTime::from(&*parent), (parent_id, visible_only));
                }
            })?;
            if newly_hidden {
                mark_ancestors_hidden(&mut graph, parent_id);
            }
        }

        // git's `still_interesting()`, consulted only after an uninteresting commit: everything
        // left is hidden, so nothing the visible walk can reach is left to discover.
        if flags.contains(PaintFlags::HIDDEN) && visible_pending == 0 {
            break;
        }
    }

    Ok(graph
        .detach()
        .into_iter()
        .filter_map(|(id, commit)| {
            commit
                .data
                .contains(PaintFlags::VISIBLE | PaintFlags::HIDDEN)
                .then_some((id, ()))
        })
        .collect())
}

/// git's `mark_parents_uninteresting()`: a commit known to be reachable from a hidden tip makes
/// every ancestor of it hidden as well, and the descent stops at an already-hidden commit.
///
/// Only ancestors the graph has already read are visited, which is git's "the parent is not parsed
/// yet, so it has no parent list to descend into" — such a commit is painted later, when its child
/// comes off the queue, exactly as git paints it from a later `process_parents()`.
fn mark_ancestors_hidden(
    graph: &mut gix_revwalk::Graph<'_, '_, gix_revwalk::graph::Commit<PaintFlags>>,
    start: ObjectId,
) {
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        let Some(commit) = graph.get(&id) else { continue };
        for parent_id in commit.parents.clone() {
            match graph.get_mut(&parent_id) {
                Some(parent) if !parent.data.contains(PaintFlags::HIDDEN) => {
                    parent.data |= PaintFlags::HIDDEN;
                    stack.push(parent_id);
                }
                _ => {}
            }
        }
    }
}

///
mod init {
    use super::{
        CommitDateQueue, CommitTimeOrder, Error, Sorting, State, collect_parents, compute_hidden_frontier, to_queue_key,
    };
    use crate::commit::{Either, Info, ParentIds, Parents, Simple};
    use gix_date::SecondsSinceUnixEpoch;
    use gix_hash::{ObjectId, oid};
    use gix_object::{CommitRefIter, FindExt};
    use std::{cmp::Reverse, collections::VecDeque};

    impl Default for State {
        fn default() -> Self {
            State {
                next: Default::default(),
                queue: gix_revwalk::PriorityQueue::new(),
                buf: vec![],
                object_hash: gix_hash::Kind::shortest(),
                seen: Default::default(),
                hidden: Default::default(),
                hidden_tips: Vec::new(),
                parents_buf: vec![],
                parent_ids: Default::default(),
            }
        }
    }

    impl State {
        fn clear(&mut self) {
            let Self {
                next,
                queue,
                buf,
                object_hash,
                seen,
                hidden,
                hidden_tips,
                parents_buf: _,
                parent_ids: _,
            } = self;
            next.clear();
            queue.clear();
            buf.clear();
            *object_hash = gix_hash::Kind::shortest();
            seen.clear();
            hidden.clear();
            hidden_tips.clear();
        }
    }

    impl Sorting {
        /// If not topo sort, provide the cutoff date if present.
        fn cutoff_time(&self) -> Option<SecondsSinceUnixEpoch> {
            match self {
                Sorting::ByCommitTimeCutoff { seconds, .. } => Some(*seconds),
                _ => None,
            }
        }
    }

    /// Builder methods
    impl<Find, Predicate> Simple<Find, Predicate>
    where
        Find: gix_object::Find,
        Predicate: FnMut(&gix_hash::oid) -> bool,
    {
        /// Set the `sorting` method.
        pub fn sorting(mut self, sorting: Sorting) -> Result<Self, Error> {
            self.sorting = sorting;
            match self.sorting {
                Sorting::BreadthFirst => self.queue_to_vecdeque(),
                Sorting::ByCommitTime(order) | Sorting::ByCommitTimeCutoff { order, .. } => {
                    let state = &mut self.state;
                    for commit_id in state.next.drain(..) {
                        add_to_queue(
                            commit_id,
                            order,
                            sorting.cutoff_time(),
                            &mut state.queue,
                            &self.objects,
                            &mut state.buf,
                        )?;
                    }
                }
            }
            Ok(self)
        }

        /// Change our commit parent handling mode to the given one.
        ///
        /// Note that this is orthogonal to the [sorting](Self::sorting()): git's `first_parent_only`
        /// only makes `add_parents_to_list()` stop after the first parent (`revision.c`), it never
        /// changes which queue the walk pops from. The parent it does queue still goes through
        /// `commit_list_insert_by_date()`, so `--first-parent` under a commit-date sort stays in
        /// commit-date order. An earlier version of this method flattened the date queue into the
        /// FIFO one here, which silently downgraded every date-sorted first-parent walk to
        /// [`BreadthFirst`](Sorting::BreadthFirst).
        pub fn parents(mut self, mode: Parents) -> Self {
            self.parents = mode;
            self
        }

        /// Hide the given `tips`, along with all commits reachable by them so that they will not be returned
        /// by the traversal.
        pub fn hide(mut self, tips: impl IntoIterator<Item = ObjectId>) -> Result<Self, Error> {
            self.state.hidden_tips = tips.into_iter().collect();
            Ok(self)
        }

        /// Set the commitgraph as `cache` to greatly accelerate any traversal.
        ///
        /// The cache will be used if possible, but we will fall back without error to using the object
        /// database for commit lookup. If the cache is corrupt, we will fall back to the object database as well.
        pub fn commit_graph(mut self, cache: Option<gix_commitgraph::Graph>) -> Self {
            self.cache = cache;
            self
        }

        /// Substitute the parents of every commit named by `grafts`, which is git's
        /// `parse_commit_buffer()` consulting `lookup_commit_graft()` (commit.c:554-590).
        ///
        /// Both files that feed the table are covered: an `info/grafts` line replaces
        /// the parent list outright, and a `<GIT_DIR>/shallow` entry makes the commit
        /// parentless so the walk stops at the clone's boundary instead of reading
        /// parent objects that are not there.
        ///
        /// Note that git refuses to open a commit-graph while a graft table is in
        /// effect (`commit_graph_compatible()`, commit-graph.c:223-242), because a
        /// graph is written from the recorded parents; callers that set both should
        /// resolve that the same way.
        pub fn grafts(mut self, grafts: Option<std::sync::Arc<gix_revwalk::graft::Table>>) -> Self {
            self.grafts = grafts.filter(|table| !table.is_empty());
            self
        }

        fn queue_to_vecdeque(&mut self) {
            let state = &mut self.state;
            state.next.extend(
                std::mem::replace(&mut state.queue, gix_revwalk::PriorityQueue::new())
                    .into_iter_unordered()
                    .map(|(_time, id)| id),
            );
        }

        fn visible_inputs_sorted(&self) -> Vec<ObjectId> {
            let mut out: Vec<_> = self
                .state
                .next
                .iter()
                .copied()
                .chain(self.state.queue.iter_unordered().copied())
                .collect();
            out.sort();
            out.dedup();
            out
        }

        fn compute_hidden_frontier(&mut self, hidden_tips: Vec<ObjectId>) -> Result<(), Error> {
            self.state.hidden.clear();
            if hidden_tips.is_empty() {
                return Ok(());
            }
            let visible_tips = self.visible_inputs_sorted();
            if visible_tips.is_empty() {
                return Ok(());
            }
            self.state.hidden = compute_hidden_frontier(
                &visible_tips,
                &hidden_tips,
                &self.objects,
                self.cache.as_ref(),
                self.grafts.clone(),
                &mut self.predicate,
            )?;
            self.state.next.retain(|id| !self.state.hidden.contains_key(id));
            self.state.queue = std::mem::replace(&mut self.state.queue, gix_revwalk::PriorityQueue::new())
                .into_iter_unordered()
                .filter(|(_, id)| !self.state.hidden.contains_key(id))
                .collect();
            Ok(())
        }
    }

    fn add_to_queue(
        commit_id: ObjectId,
        order: CommitTimeOrder,
        cutoff_time: Option<SecondsSinceUnixEpoch>,
        queue: &mut CommitDateQueue,
        objects: &impl gix_object::Find,
        buf: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let commit_iter = objects.find_commit_iter(&commit_id, buf)?;
        let time = commit_iter.committer()?.seconds();
        let key = to_queue_key(time, order);
        match (cutoff_time, order) {
            (Some(cutoff_time), _) if time >= cutoff_time => queue.insert(key, commit_id),
            (Some(_), _) => {}
            (None, _) => queue.insert(key, commit_id),
        }
        Ok(())
    }

    /// Lifecycle methods
    impl<Find> Simple<Find, fn(&oid) -> bool>
    where
        Find: gix_object::Find,
    {
        /// Create a new instance.
        ///
        /// * `find` - a way to lookup new object data during traversal by their `ObjectId`, writing their data into buffer and returning
        ///   an iterator over commit tokens if the object is present and is a commit. Caching should be implemented within this function
        ///   as needed.
        /// * `tips`
        ///   * the starting points of the iteration, usually commits
        ///   * each commit they lead to will only be returned once, including the tip that started it
        pub fn new(tips: impl IntoIterator<Item = impl Into<ObjectId>>, find: Find) -> Self {
            Self::filtered(tips, find, |_| true)
        }
    }

    impl<Find, Predicate> Simple<Find, Predicate>
    where
        Find: gix_object::Find,
        Predicate: FnMut(&oid) -> bool,
    {
        /// Create a new instance with commit filtering enabled.
        ///
        /// * `find` - a way to lookup new object data during traversal by their `ObjectId`, writing their data into buffer and returning
        ///   an iterator over commit tokens if the object is present and is a commit. Caching should be implemented within this function
        ///   as needed.
        /// * `tips`
        ///   * the starting points of the iteration, usually commits
        ///   * each commit they lead to will only be returned once, including the tip that started it
        /// * `predicate` - indicate whether a given commit should be included in the result as well
        ///   as whether its parent commits should be traversed.
        pub fn filtered(
            tips: impl IntoIterator<Item = impl Into<ObjectId>>,
            find: Find,
            mut predicate: Predicate,
        ) -> Self {
            let tips = tips.into_iter();
            let mut state = State::default();
            {
                state.clear();
                state.next.reserve(tips.size_hint().0);
                for tip in tips.map(Into::into) {
                    if state.seen.insert(tip) && predicate(&tip) {
                        state.next.push_back(tip);
                    }
                }
            }
            Self {
                objects: find,
                cache: None,
                predicate,
                state,
                parents: Default::default(),
                sorting: Default::default(),
                grafts: None,
            }
        }
    }

    /// Access
    impl<Find, Predicate> Simple<Find, Predicate> {
        /// Return an iterator for accessing data of the current commit, parsed lazily.
        pub fn commit_iter(&self) -> CommitRefIter<'_> {
            CommitRefIter::from_bytes(self.commit_data(), self.state.object_hash)
        }

        /// Return the current commits' raw data, which can be parsed using [`gix_object::CommitRef::from_bytes()`].
        pub fn commit_data(&self) -> &[u8] {
            &self.state.buf
        }
    }

    impl<Find, Predicate> Iterator for Simple<Find, Predicate>
    where
        Find: gix_object::Find,
        Predicate: FnMut(&oid) -> bool,
    {
        type Item = Result<Info, Error>;

        fn next(&mut self) -> Option<Self::Item> {
            if !self.state.hidden_tips.is_empty() {
                let hidden_tips = std::mem::take(&mut self.state.hidden_tips);
                if let Err(err) = self.compute_hidden_frontier(hidden_tips) {
                    self.state.queue.clear();
                    self.state.next.clear();
                    return Some(Err(err));
                }
            }
            match self.sorting {
                Sorting::BreadthFirst => self.next_by_topology(),
                Sorting::ByCommitTime(order) => self.next_by_commit_date(order, None),
                Sorting::ByCommitTimeCutoff { seconds, order } => self.next_by_commit_date(order, seconds.into()),
            }
        }
    }

    /// Utilities
    impl<Find, Predicate> Simple<Find, Predicate>
    where
        Find: gix_object::Find,
        Predicate: FnMut(&oid) -> bool,
    {
        fn next_by_commit_date(
            &mut self,
            order: CommitTimeOrder,
            cutoff: Option<SecondsSinceUnixEpoch>,
        ) -> Option<Result<Info, Error>> {
            let follow_first_parent_only = matches!(self.parents, Parents::First);
            let state = &mut self.state;
            let next = &mut state.queue;

            loop {
                let (commit_time, oid) = match next.pop()? {
                    (Ok(t) | Err(Reverse(t)), o) => (t, o),
                };
                state.object_hash = oid.kind();
                if state.hidden.contains_key(&oid) {
                    continue;
                }
                let mut parents: ParentIds = Default::default();

                // `parse_commit_buffer()` still reads the commit — only its parent list
                // is replaced (commit.c:554-590) — so the graft is applied after the
                // lookup, and a grafted commit that is missing still errors as before.
                if let Some(grafted) = self.grafts.as_ref().and_then(|g| g.parents_of(&oid)).map(<[_]>::to_vec) {
                    if let Err(err) = super::super::find(self.cache.as_ref(), &self.objects, &oid, &mut state.buf) {
                        return Some(Err(err.into()));
                    }
                    for id in grafted {
                        parents.push(id);
                        if follow_first_parent_only && parents.len() > 1 {
                            continue;
                        }
                        insert_into_seen_and_queue(
                            &mut state.seen,
                            &state.hidden,
                            id,
                            &mut self.predicate,
                            next,
                            order,
                            cutoff,
                            || {
                                self.objects
                                    .find_commit_iter(id.as_ref(), &mut state.parents_buf)
                                    .ok()
                                    .and_then(|parent| parent.committer().ok().map(|committer| committer.seconds()))
                                    .unwrap_or_default()
                            },
                        );
                    }
                    return Some(Ok(Info {
                        id: oid,
                        parent_ids: parents,
                        commit_time: Some(commit_time),
                    }));
                }

                match super::super::find(self.cache.as_ref(), &self.objects, &oid, &mut state.buf) {
                    Ok(Either::CachedCommit(commit)) => {
                        if !collect_parents(&mut state.parent_ids, self.cache.as_ref(), commit.iter_parents()) {
                            // drop corrupt caches and try again with ODB
                            self.cache = None;
                            return self.next_by_commit_date(order, cutoff);
                        }
                        for (id, parent_commit_time) in state.parent_ids.drain(..) {
                            parents.push(id);
                            if follow_first_parent_only && parents.len() > 1 {
                                // `--first-parent` stops the *walk* here, not the parent list:
                                // git's `add_parents_to_list()` breaks out of its loop while
                                // `commit->parents` keeps every parent, which is what `%P`,
                                // `--parents` and the `--min-parents`/`--max-parents` gate read.
                                continue;
                            }
                            insert_into_seen_and_queue(
                                &mut state.seen,
                                &state.hidden,
                                id,
                                &mut self.predicate,
                                next,
                                order,
                                cutoff,
                                || parent_commit_time,
                            );
                        }
                    }
                    Ok(Either::CommitRefIter(commit_iter)) => {
                        for token in commit_iter {
                            match token {
                                Ok(gix_object::commit::ref_iter::Token::Tree { .. }) => continue,
                                Ok(gix_object::commit::ref_iter::Token::Parent { id }) => {
                                    parents.push(id);
                                    if follow_first_parent_only && parents.len() > 1 {
                                        // See the comment in the cached-commit arm above: the
                                        // remaining parents are recorded but not walked.
                                        continue;
                                    }
                                    insert_into_seen_and_queue(
                                        &mut state.seen,
                                        &state.hidden,
                                        id,
                                        &mut self.predicate,
                                        next,
                                        order,
                                        cutoff,
                                        || {
                                            let parent =
                                                self.objects.find_commit_iter(id.as_ref(), &mut state.parents_buf).ok();
                                            parent
                                                .and_then(|parent| {
                                                    parent.committer().ok().map(|committer| committer.seconds())
                                                })
                                                .unwrap_or_default()
                                        },
                                    );
                                }
                                Ok(_unused_token) => break,
                                Err(err) => return Some(Err(err.into())),
                            }
                        }
                    }
                    Err(err) => return Some(Err(err.into())),
                }

                return Some(Ok(Info {
                    id: oid,
                    parent_ids: parents,
                    commit_time: Some(commit_time),
                }));
            }
        }

        fn next_by_topology(&mut self) -> Option<Result<Info, Error>> {
            let follow_first_parent_only = matches!(self.parents, Parents::First);
            let state = &mut self.state;
            let next = &mut state.next;

            loop {
                let oid = next.pop_front()?;
                state.object_hash = oid.kind();
                if state.hidden.contains_key(&oid) {
                    continue;
                }
                let mut parents: ParentIds = Default::default();

                // See the same block in `next_by_commit_date()`: the commit is still
                // read, only its parent list comes from the graft table.
                if let Some(grafted) = self.grafts.as_ref().and_then(|g| g.parents_of(&oid)).map(<[_]>::to_vec) {
                    if let Err(err) = super::super::find(self.cache.as_ref(), &self.objects, &oid, &mut state.buf) {
                        return Some(Err(err.into()));
                    }
                    for pid in grafted {
                        parents.push(pid);
                        if follow_first_parent_only && parents.len() > 1 {
                            continue;
                        }
                        insert_into_seen_and_next(&mut state.seen, &state.hidden, pid, &mut self.predicate, next);
                    }
                    return Some(Ok(Info {
                        id: oid,
                        parent_ids: parents,
                        commit_time: None,
                    }));
                }

                match super::super::find(self.cache.as_ref(), &self.objects, &oid, &mut state.buf) {
                    Ok(Either::CachedCommit(commit)) => {
                        if !collect_parents(&mut state.parent_ids, self.cache.as_ref(), commit.iter_parents()) {
                            // drop corrupt caches and try again with ODB
                            self.cache = None;
                            return self.next_by_topology();
                        }

                        for (pid, _commit_time) in state.parent_ids.drain(..) {
                            parents.push(pid);
                            if follow_first_parent_only && parents.len() > 1 {
                                // `--first-parent` stops the *walk* here, not the parent list:
                                // git's `add_parents_to_list()` breaks out of its loop while
                                // `commit->parents` keeps every parent, which is what `%P`,
                                // `--parents` and the `--min-parents`/`--max-parents` gate read.
                                continue;
                            }
                            insert_into_seen_and_next(&mut state.seen, &state.hidden, pid, &mut self.predicate, next);
                        }
                    }
                    Ok(Either::CommitRefIter(commit_iter)) => {
                        for token in commit_iter {
                            match token {
                                Ok(gix_object::commit::ref_iter::Token::Tree { .. }) => continue,
                                Ok(gix_object::commit::ref_iter::Token::Parent { id: pid }) => {
                                    parents.push(pid);
                                    if follow_first_parent_only && parents.len() > 1 {
                                        // See the comment in the cached-commit arm above: the
                                        // remaining parents are recorded but not walked.
                                        continue;
                                    }
                                    insert_into_seen_and_next(
                                        &mut state.seen,
                                        &state.hidden,
                                        pid,
                                        &mut self.predicate,
                                        next,
                                    );
                                }
                                Ok(_a_token_past_the_parents) => break,
                                Err(err) => return Some(Err(err.into())),
                            }
                        }
                    }
                    Err(err) => return Some(Err(err.into())),
                }

                return Some(Ok(Info {
                    id: oid,
                    parent_ids: parents,
                    commit_time: None,
                }));
            }
        }
    }

    fn insert_into_seen_and_next(
        seen: &mut gix_hashtable::HashSet<ObjectId>,
        hidden: &gix_revwalk::graph::IdMap<()>,
        parent_id: ObjectId,
        predicate: &mut impl FnMut(&oid) -> bool,
        next: &mut VecDeque<ObjectId>,
    ) {
        if hidden.contains_key(&parent_id) {
            return;
        }
        if seen.insert(parent_id) && predicate(&parent_id) {
            next.push_back(parent_id);
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn insert_into_seen_and_queue(
        seen: &mut gix_hashtable::HashSet<ObjectId>,
        hidden: &gix_revwalk::graph::IdMap<()>,
        parent_id: ObjectId,
        predicate: &mut impl FnMut(&oid) -> bool,
        queue: &mut CommitDateQueue,
        order: CommitTimeOrder,
        cutoff: Option<SecondsSinceUnixEpoch>,
        get_parent_commit_time: impl FnOnce() -> gix_date::SecondsSinceUnixEpoch,
    ) {
        if hidden.contains_key(&parent_id) {
            return;
        }
        if seen.insert(parent_id) && predicate(&parent_id) {
            let parent_commit_time = get_parent_commit_time();
            let key = to_queue_key(parent_commit_time, order);
            match cutoff {
                Some(cutoff_older_than) if parent_commit_time < cutoff_older_than => {}
                Some(_) | None => queue.insert(key, parent_id),
            }
        }
    }
}

fn collect_parents(
    dest: &mut SmallVec<[(gix_hash::ObjectId, gix_date::SecondsSinceUnixEpoch); 2]>,
    cache: Option<&gix_commitgraph::Graph>,
    parents: gix_commitgraph::file::commit::Parents<'_>,
) -> bool {
    dest.clear();
    let cache = cache.as_ref().expect("parents iter is available, backed by `cache`");
    for parent_id in parents {
        match parent_id {
            Ok(pos) => dest.push({
                let parent = cache.commit_at(pos);
                (
                    parent.id().to_owned(),
                    parent.committer_timestamp() as gix_date::SecondsSinceUnixEpoch,
                )
            }),
            Err(_err) => return false,
        }
    }
    true
}
