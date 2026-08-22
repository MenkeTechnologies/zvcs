use crate::Error;
use gix_error::ErrorExt as _;

impl crate::Repository {
    /// Create a graph data-structure capable of accelerating graph traversals and storing state of type `T` with each commit
    /// it encountered.
    ///
    /// Note that the `cache` will be used if present, and it's best obtained with
    /// [`commit_graph_if_enabled()`](crate::Repository::commit_graph_if_enabled()).
    ///
    /// Note that a commitgraph is only allowed to be used if `core.commitGraph` is true (the default), and that configuration errors are
    /// ignored as well.
    ///
    /// ### Performance
    ///
    /// Note that the [Graph][gix_revwalk::Graph] can be sensitive to various object database settings that may affect the performance
    /// of the commit walk.
    pub fn revision_graph<'cache, T>(
        &self,
        cache: Option<&'cache gix_commitgraph::Graph>,
    ) -> gix_revwalk::Graph<'_, 'cache, T> {
        // Every walk built on this graph reads parents through the graft table, which
        // is git parsing every commit through `parse_commit_buffer()` (commit.c:554).
        gix_revwalk::Graph::new(&self.objects, cache).with_grafts(Some(self.commit_grafts().clone()))
    }

    /// Return a cache for commits and their graph structure, as managed by `git commit-graph`, for accelerating commit walks on
    /// a low level.
    ///
    /// Note that [`revision_graph()`][crate::Repository::revision_graph()] should be preferred for general purpose walks that don't
    /// rely on the actual commit cache to be present, while leveraging the commit-graph if possible.
    pub fn commit_graph(&self) -> Result<gix_commitgraph::Graph, Error> {
        if !self.commit_graph_compatible() {
            // `message!` yields a `Message`, and `Error` converts from `Exn<_>` rather
            // than from `Message` directly, so it is raised first — the bridge
            // gix-error documents at its own lib.rs:94.
            return Err(gix_error::message!("commit-graph is incompatible with the graft table")
                .raise()
                .into());
        }
        gix_commitgraph::at(self.objects.store_ref().path().join("info")).map_err(Into::into)
    }

    /// `commit_graph_compatible()` (commit-graph.c:223-242): a commit-graph records
    /// the parents the commit objects carry, so it contradicts a graft table and
    /// must not be used while one is in effect.
    ///
    /// ```c
    /// prepare_commit_graft(r);
    /// if (r->parsed_objects &&
    ///     (r->parsed_objects->grafts_nr || r->parsed_objects->substituted_parent))
    ///         return 0;
    /// if (is_repository_shallow(r))
    ///         return 0;
    /// ```
    ///
    /// The shallow file feeds the same table here, so the one check covers both of
    /// git's. Replace refs, git's third reason to refuse the graph, are handled by
    /// the object database rather than here.
    pub(crate) fn commit_graph_compatible(&self) -> bool {
        self.commit_grafts_are_empty()
    }

    /// Return a newly opened commit-graph if it is available *and* enabled in the Git configuration.
    pub fn commit_graph_if_enabled(
        &self,
    ) -> Result<Option<gix_commitgraph::Graph>, super::commit_graph_if_enabled::Error> {
        Ok(self
            .config
            .may_use_commit_graph()?
            .then(|| self.commit_graph_compatible())
            .unwrap_or_default()
            .then(|| gix_commitgraph::at(self.objects.store_ref().path().join("info")))
            .transpose()
            .or_else(|err| match err.downcast_any_ref::<std::io::Error>() {
                Some(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                _ => Err(err.into_error()),
            })?)
    }
}
