bitflags::bitflags! {
    /// The flags used in the graph for finding [merge bases](crate::merge_base()).
    #[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
    pub struct Flags: u8 {
        /// The commit belongs to the graph reachable by the first commit
        const COMMIT1 = 1 << 0;
        /// The commit belongs to the graph reachable by all other commits.
        const COMMIT2 = 1 << 1;

        /// Marks the commit as done, it's reachable by both COMMIT1 and COMMIT2.
        const STALE = 1 << 2;
        /// The commit was already put ontto the results list.
        const RESULT = 1 << 3;
    }
}

/// The error returned by the [`merge_base()`][function::merge_base()] function.
pub type Error = Simple;

/// What can stop a merge-base computation.
#[derive(Debug)]
pub enum Simple {
    /// A commit could not be put into the graph — a decode failure, or an object
    /// database that could not answer.
    Graph(&'static str),
    /// `error(_("could not parse commit %s"))` (commit-reach.c:184-185).
    ///
    /// `paint_down_to_common()` parses every parent it is about to queue
    /// (commit-reach.c:171-186) and *aborts the whole computation* when one cannot
    /// be read, rather than treating it as a boundary. The commit named is the
    /// parent, and git's `repo_parse_commit()` has already printed its own
    /// `error("Could not read %s")` (commit.c:641-645) for the same object by the
    /// time this is returned.
    ///
    /// A graft line naming an object this repository does not have reaches it
    /// without a damaged object database: `lookup_commit_graft()` (commit.c:332-340)
    /// substitutes whatever the file said.
    UnparsableCommit(gix_hash::ObjectId),
}

impl std::fmt::Display for Simple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Simple::Graph(message) => f.write_str(message),
            Simple::UnparsableCommit(id) => write!(f, "could not parse commit {id}"),
        }
    }
}

impl std::error::Error for Simple {}

pub(crate) mod function;

mod octopus {
    use gix_hash::ObjectId;
    use gix_revwalk::{Graph, graph};

    use crate::merge_base::{Error, Flags};

    /// Given a commit at `first` id, traverse the commit `graph` and return *the best common ancestor* between it and `others`,
    /// sorted from best to worst. Returns `None` if there is no common merge-base as `first` and `others` don't *all* share history.
    /// If `others` is empty, `Some(first)` is returned.
    ///
    /// # Performance
    ///
    /// For repeated calls, be sure to re-use `graph` as its content will be kept and reused for a great speed-up. The contained flags
    /// will automatically be cleared.
    pub fn octopus(
        mut first: ObjectId,
        others: &[ObjectId],
        graph: &mut Graph<'_, '_, graph::Commit<Flags>>,
    ) -> Result<Option<ObjectId>, Error> {
        for other in others {
            if let Some(next) =
                crate::merge_base(first, std::slice::from_ref(other), graph)?.map(|bases| *bases.first())
            {
                first = next;
            } else {
                return Ok(None);
            }
        }
        Ok(Some(first))
    }
}
pub use octopus::octopus;
