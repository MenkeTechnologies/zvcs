/// The outcome produced by [`commit::merge_base()`](crate::commit::virtual_merge_base()).
pub struct Outcome {
    /// The commit ids of all the virtual merge bases we have produced in the process of recursively merging the merge-bases.
    /// As they have been written to the object database, they are still available until they are garbage collected.
    /// The last one is the most recently produced and the one returned as `commit_id`.
    /// This is never empty.
    pub virtual_merge_bases: nonempty::NonEmpty<gix_hash::ObjectId>,
    /// The id of the commit that was created to hold the merged tree.
    pub commit_id: gix_hash::ObjectId,
    /// The hash of the merged tree.
    pub tree_id: gix_hash::ObjectId,
}

/// One of the base-merges performed while folding several merge-bases into one, kept so a caller
/// can report what happened inside it.
///
/// Git keeps these too. `merge_ort_internal()` recurses with the *same* `opt->priv`, and
/// `clear_or_reinit_internal_opts(opti, 1)` — the call that resets it between bases — deliberately
/// leaves `opti->conflicts` alone (merge-ort.c:748-773, the clearing loop is guarded by
/// `if (!reinitialize)`). So the inner merges' `path_msg()` entries are still in the map the outer
/// merge appends to, and `merge_display_update_messages()` prints them interleaved per path. They
/// are only *invisible* by default because `path_msg()` returns early on
/// `opt->priv->call_depth && opt->verbosity < 5` (merge-ort.c:815).
#[derive(Clone)]
pub struct InnerMerge {
    /// The tree of `opt->branch1` — "Temporary merge branch 1" — for this merge, i.e. the side the
    /// conflicts below are reported against.
    pub our_tree_id: gix_hash::ObjectId,
    /// The conflicts this base-merge produced, in the order it produced them.
    pub conflicts: Vec<crate::tree::Conflict>,
}

/// The error returned by [`commit::merge_base()`](crate::commit::virtual_merge_base()).
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    MergeTree(#[from] crate::tree::Error),
    #[error("Failed to write tree for merged merge-base or virtual commit")]
    WriteObject(gix_object::write::Error),
    #[error("Failed to decode a commit needed to build a virtual merge-base")]
    DecodeCommit(#[from] gix_object::decode::Error),
    #[error(
        "Conflicts occurred when trying to resolve multiple merge-bases by merging them. This is most certainly a bug."
    )]
    VirtualMergeBaseConflict,
    #[error("Could not find commit to use as basis for a virtual commit")]
    FindCommit(#[from] gix_object::find::existing_object::Error),
}

pub(super) mod function {
    use gix_object::FindExt;

    use super::Error;
    use crate::blob::builtin_driver;

    /// Create a single virtual merge-base by merging `first_commit`, `second_commit` and `others` into one.
    /// Note that `first_commit` and `second_commit` are expected to have been popped off `others`, so `first_commit`
    /// was the last provided merge-base of function that provides multiple merge-bases for a pair of commits.
    ///
    /// The parameters `graph`, `diff_resource_cache`, `blob_merge`, `objects`, `abbreviate_hash` and `options` are passed
    /// directly to [`tree()`](crate::tree()) for merging the trees of two merge-bases at a time.
    /// Note that most of `options` are overwritten to match the requirements of a merge-base merge.
    #[expect(clippy::too_many_arguments)]
    pub fn virtual_merge_base<'objects>(
        first_commit: gix_hash::ObjectId,
        second_commit: gix_hash::ObjectId,
        others: Vec<gix_hash::ObjectId>,
        graph: &mut gix_revwalk::Graph<'_, '_, gix_revwalk::graph::Commit<gix_revision::merge_base::Flags>>,
        diff_resource_cache: &mut gix_diff::blob::Platform,
        blob_merge: &mut crate::blob::Platform,
        objects: &'objects (impl gix_object::FindObjectOrHeader + gix_object::Write),
        abbreviate_hash: &mut dyn FnMut(&gix_hash::oid) -> String,
        options: crate::tree::Options,
    ) -> Result<super::Outcome, crate::commit::Error> {
        virtual_merge_base_with_inner_merges(
            first_commit,
            second_commit,
            others,
            graph,
            diff_resource_cache,
            blob_merge,
            objects,
            abbreviate_hash,
            options,
        )
        .map(|(outcome, _inner)| outcome)
    }

    /// [`virtual_merge_base()`] which also hands back each base-merge it performed, as
    /// [`InnerMerge`](super::InnerMerge) describes.
    ///
    /// This is what a caller needs to reproduce `merge.verbosity >= 5`, where merge-ort stops
    /// discarding the recursion's `path_msg()` output and prints it alongside the outer merge's.
    /// The recursion beyond this loop is not reported: an inner merge whose own two merge-bases
    /// disagree recurses through [`commit()`](crate::commit()), which has no way to pass its
    /// conflicts back out, so git's `call_depth >= 2` messages have no counterpart here.
    #[expect(clippy::too_many_arguments)]
    pub fn virtual_merge_base_with_inner_merges<'objects>(
        first_commit: gix_hash::ObjectId,
        second_commit: gix_hash::ObjectId,
        mut others: Vec<gix_hash::ObjectId>,
        graph: &mut gix_revwalk::Graph<'_, '_, gix_revwalk::graph::Commit<gix_revision::merge_base::Flags>>,
        diff_resource_cache: &mut gix_diff::blob::Platform,
        blob_merge: &mut crate::blob::Platform,
        objects: &'objects (impl gix_object::FindObjectOrHeader + gix_object::Write),
        abbreviate_hash: &mut dyn FnMut(&gix_hash::oid) -> String,
        mut options: crate::tree::Options,
    ) -> Result<(super::Outcome, Vec<super::InnerMerge>), crate::commit::Error> {
        let mut merged_commit_id = first_commit;
        others.push(second_commit);

        options.tree_conflicts = Some(crate::tree::ResolveWith::Ancestor);
        options.blob_merge.is_virtual_ancestor = true;
        // ```c
        // if (opt->priv->call_depth) {
        //         ll_opts.virtual_ancestor = 1;
        //         ll_opts.variant = 0;
        // }
        // ```
        //
        // (`merge_3way()`, merge-ort.c.) A merge-base merge keeps its content conflicts:
        // the blob written into the virtual ancestor is the one *with* the markers, drawn
        // longer than the outer merge's so the two nest legibly. Resolving with one side
        // instead hands the outer merge a base that agrees with that side, which turns a
        // criss-cross conflict into a silent clean take of the other side. `variant = 0`
        // is why the outer `-X ours`/`-X theirs` does not reach here either.
        if !matches!(
            options.blob_merge.text.conflict,
            builtin_driver::text::Conflict::Keep { .. }
        ) {
            options.blob_merge.text.conflict = builtin_driver::text::Conflict::Keep {
                style: Default::default(),
                marker_size: builtin_driver::text::Conflict::DEFAULT_MARKER_SIZE
                    .try_into()
                    .expect("non-zero default"),
            };
        }
        let favor_ancestor = Some(builtin_driver::binary::ResolveWith::Ancestor);
        options.blob_merge.resolve_binary_with = favor_ancestor;
        options.symlink_conflicts = favor_ancestor;
        let labels = builtin_driver::text::Labels {
            current: Some("Temporary merge branch 1".into()),
            other: Some("Temporary merge branch 2".into()),
            ancestor: None,
        };
        // `merge_ort_internal()`'s loop brackets each iteration with
        // `opt->priv->call_depth++` / `opt->priv->call_depth--`
        // (merge-ort.c:5350-5368), so *every* base merged in this loop runs one
        // level below the caller — not one level below the previous iteration.
        // The marker size the content merge asks for is `call_depth * 2` on top
        // of the configured seven (merge-ort.c:4337), so incrementing per
        // iteration made the third and later bases draw markers two characters
        // longer than git's: `git merge-recursive cc-a cc-b main -- cc-left
        // cc-right` wrote a 90-byte stage-1 blob whose markers were eleven
        // characters wide where stock writes 84 bytes with nine.
        options.marker_size_multiplier = options.marker_size_multiplier.saturating_add(1);
        let mut virtual_merge_bases = Vec::new();
        let mut inner_merges = Vec::new();
        let mut tree_id = None;
        while let Some(next_commit_id) = others.pop() {
            // Recorded before the merge: this is `opt->branch1`'s tree for the iteration, which is
            // what attributes a reported path to "Temporary merge branch 1" or "…2".
            let our_tree_id = objects.find_commit(&merged_commit_id, &mut Vec::new())?.tree();
            let mut out = crate::commit(
                merged_commit_id,
                next_commit_id,
                labels,
                graph,
                diff_resource_cache,
                blob_merge,
                objects,
                abbreviate_hash,
                crate::commit::Options {
                    allow_missing_merge_base: false,
                    tree_merge: options.clone(),
                    use_first_merge_base: false,
                },
            )?;
            // Content conflicts are expected here and are the point of the exercise — git
            // records the marked-up blob and merges on. Only a tree-level conflict that
            // even `ResolveWith::Ancestor` could not decide is a real failure, which is
            // what a resolution *failure* is; `has_unresolved_conflicts()` cannot express
            // that on its own, as both of its content modes count marker resolutions.
            if out.tree_merge.conflicts.iter().any(|c| c.resolution.is_err()) {
                return Err(Error::VirtualMergeBaseConflict.into());
            }
            inner_merges.push(super::InnerMerge {
                our_tree_id,
                conflicts: out.tree_merge.conflicts.clone(),
            });
            let merged_tree_id = out
                .tree_merge
                .tree
                .write(|tree| objects.write(tree))
                .map_err(Error::WriteObject)?;

            tree_id = Some(merged_tree_id);
            merged_commit_id = create_virtual_commit(objects, merged_commit_id, next_commit_id, merged_tree_id)?;

            virtual_merge_bases.extend(out.virtual_merge_bases);
            virtual_merge_bases.push(merged_commit_id);
        }

        Ok((
            super::Outcome {
                virtual_merge_bases: nonempty::NonEmpty::from_vec(virtual_merge_bases)
                    .expect("the virtual merge-base process always creates at least one commit"),
                commit_id: merged_commit_id,
                tree_id: tree_id.map_or_else(
                    || {
                        let mut buf = Vec::new();
                        objects.find_commit(&merged_commit_id, &mut buf).map(|c| c.tree())
                    },
                    Ok,
                )?,
            },
            inner_merges,
        ))
    }

    fn create_virtual_commit(
        objects: &(impl gix_object::Find + gix_object::Write),
        parent_a: gix_hash::ObjectId,
        parent_b: gix_hash::ObjectId,
        tree_id: gix_hash::ObjectId,
    ) -> Result<gix_hash::ObjectId, Error> {
        let mut buf = Vec::new();
        let commit_ref = objects.find_commit(&parent_a, &mut buf)?;
        let mut commit = commit_ref.to_owned()?;
        commit.parents = vec![parent_a, parent_b].into();
        commit.tree = tree_id;
        objects.write(&commit).map_err(Error::WriteObject)
    }
}
