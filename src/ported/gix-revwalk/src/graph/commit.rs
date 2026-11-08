use gix_date::SecondsSinceUnixEpoch;
use smallvec::SmallVec;

use super::LazyCommit;
use crate::graph::{Commit, Either, Generation};

impl<'graph, 'cache> LazyCommit<'graph, 'cache> {
    /// Return an iterator over the parents of this commit.
    pub fn iter_parents(&self) -> Parents<'graph, 'cache> {
        let backing = match &self.backing {
            // `parse_commit_buffer()` reads the parents and the date and leaves
            // the idents alone (commit.c:556-591), so a malformed author never
            // ends the parent list.
            Either::Left(buf) => Either::Left(
                gix_object::commit::walk::WalkInfo::from_bytes(buf, self.object_hash)
                    .map(|info| info.parents.into_iter()),
            ),
            Either::Right((cache, pos)) => Either::Right((*cache, cache.commit_at(*pos).iter_parents())),
        };
        Parents {
            backing,
            _graph: std::marker::PhantomData,
        }
    }

    /// Returns the timestamp at which this commit was created.
    ///
    /// This is the single-most important date for determining recency of commits.
    /// Note that this can only fail if the commit is backed by the object database *and* parsing fails.
    pub fn committer_timestamp(&self) -> Result<SecondsSinceUnixEpoch, gix_object::decode::Error> {
        Ok(match &self.backing {
            // `parse_commit_date()` (commit.c:128-194): 0 where the idents
            // cannot be read, never an error.
            Either::Left(buf) => gix_object::commit::walk::WalkInfo::from_bytes(buf, self.object_hash)?.date,
            Either::Right((cache, pos)) => cache.commit_at(*pos).committer_timestamp() as SecondsSinceUnixEpoch, // a cast as we cannot represent the error and trying seems overkill
        })
    }

    /// Returns the generation of the commit if it is backed by a commit graph.
    pub fn generation(&self) -> Option<Generation> {
        match &self.backing {
            Either::Left(_) => None,
            Either::Right((cache, pos)) => cache.commit_at(*pos).generation().into(),
        }
    }

    /// Returns the generation of the commit and its commit-time, either from cache if available, or parsed from the object buffer.
    pub fn generation_and_timestamp(
        &self,
    ) -> Result<(Option<Generation>, SecondsSinceUnixEpoch), gix_object::decode::Error> {
        Ok(match &self.backing {
            Either::Left(buf) => (
                None,
                gix_object::commit::walk::WalkInfo::from_bytes(buf, self.object_hash)?.date,
            ),
            Either::Right((cache, pos)) => {
                let commit = cache.commit_at(*pos);
                (
                    commit.generation().into(),
                    // a cast as we cannot represent the error and trying seems overkill
                    cache.commit_at(*pos).committer_timestamp() as SecondsSinceUnixEpoch,
                )
            }
        })
    }

    /// Convert ourselves into an owned version, which effectively detaches us from the underlying graph.
    /// Use `new_data()` to provide the `data` field for the owned `Commit`.
    pub fn to_owned<T>(&self, new_data: impl FnOnce() -> T) -> Result<Commit<T>, to_owned::Error> {
        let data = new_data();
        Ok(match &self.backing {
            Either::Left(buf) => {
                let info = gix_object::commit::walk::WalkInfo::from_bytes(buf, self.object_hash)?;
                Commit {
                    parents: info.parents.into_iter().collect(),
                    commit_time: info.date,
                    generation: None,
                    data,
                }
            }
            Either::Right((cache, pos)) => {
                let mut parents = SmallVec::default();
                let commit = cache.commit_at(*pos);
                for pos in commit.iter_parents() {
                    let pos = pos?;
                    parents.push(cache.commit_at(pos).id().to_owned());
                }
                Commit {
                    parents,
                    commit_time: commit.committer_timestamp().try_into().map_err(|_| {
                        to_owned::Error::CommitGraphTime {
                            actual: commit.committer_timestamp(),
                        }
                    })?,
                    generation: Some(commit.generation()),
                    data,
                }
            }
        })
    }
}

/// An iterator over the parents of a commit.
pub struct Parents<'graph, 'cache> {
    _graph: std::marker::PhantomData<&'graph [u8]>,
    backing: Either<
        Result<smallvec::IntoIter<[gix_hash::ObjectId; 2]>, gix_object::decode::Error>,
        (
            &'cache gix_commitgraph::Graph,
            gix_commitgraph::file::commit::Parents<'cache>,
        ),
    >,
}

impl Iterator for Parents<'_, '_> {
    type Item = Result<gix_hash::ObjectId, iter_parents::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.backing {
            Either::Left(Ok(it)) => it.next().map(Ok),
            Either::Left(Err(err)) => {
                let err = *err;
                // The decode error is reported once, then the iteration ends.
                self.backing = Either::Left(Ok(SmallVec::new().into_iter()));
                Some(Err(err.into()))
            }
            Either::Right((cache, it)) => it
                .next()
                .map(|r| r.map(|pos| cache.id_at(pos).to_owned()).map_err(Into::into)),
        }
    }
}

///
pub mod iter_parents {
    /// The error returned by the [`Parents`][super::Parents] iterator.
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("An error occurred when parsing commit parents")]
        DecodeCommit(#[from] gix_object::decode::Error),
        #[error("An error occurred when parsing parents from the commit graph")]
        DecodeCommitGraph(#[from] gix_error::Message),
    }
}

///
pub mod to_owned {
    /// The error returned by [`to_owned()`][crate::graph::LazyCommit::to_owned()].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("A commit could not be decoded during traversal")]
        Decode(#[from] gix_object::decode::Error),
        #[error("Could not find commit position in graph when traversing parents")]
        CommitGraphParent(#[from] gix_error::Message),
        #[error("Commit-graph time could not be presented as signed integer: {actual}")]
        CommitGraphTime { actual: u64 },
    }
}
