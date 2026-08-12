//! Port of git's `-C` cross-file copy detection, `blame.c` 2204-2336 (git 2.55.0).
//!
//! `-M` only ever offers a leftover chunk back to the blob the *same* file has in a parent.
//! `find_copy_in_parent()` widens that to other files: the parent's side of the tree diff (or, with
//! more `-C`s, every file the parent has) is walked, and each of those blobs is offered every
//! leftover chunk. Whichever file yields the least trivial match becomes the chunk's *Source File*,
//! which is why a suspect has to be a commit *and* a path.

use gix_hash::ObjectId;
use gix_object::{FindExt, bstr::BString};

use super::function::OriginFiles;
use super::moved::{self, Split};
use crate::{
    Error, Statistics,
    types::{CopyDetection, PathId, PathTable, Suspect, UnblamedHunk},
};

/// One scapegoat as `find_copy_in_parent()` receives it: `sg->item` plus the `porigin` the earlier
/// passes found for it, if any.
pub(super) struct CopyParent {
    /// The parent commit.
    pub commit_id: ObjectId,
    /// The parent's tree, for the diff that produces the candidate paths.
    pub tree_id: ObjectId,
    /// `porigin->path`: the path this parent already offered the blamed file at, which
    /// `find_move_in_parent()` has dealt with, so it is skipped here. `None` when no parent origin
    /// was found, which is what makes `-C -C` widen the search.
    pub porigin_path: Option<PathId>,
}

/// A file in the parent a chunk may have been copied from.
type Candidate = (BString, ObjectId);

/// The paths in the parent that a leftover chunk is compared against, in git's `diff_queued_diff`
/// order (tree order, which for full paths is byte order).
///
/// Without "find copies harder" this is the parent side of the ordinary tree diff — every file the
/// commit changed or removed, since a file only present in the target cannot be copied *from*.
/// git skips `p->one` that is not `DIFF_FILE_VALID` or is a gitlink, which is what the mode checks
/// below do.
fn changed_in_parent(
    odb: &(impl gix_object::Find + gix_object::FindHeader),
    parent_tree_id: &ObjectId,
    target_tree_id: &ObjectId,
    state: &mut gix_diff::tree::State,
    stats: &mut Statistics,
) -> Result<Vec<Candidate>, Error> {
    let (mut lhs_buf, mut rhs_buf) = (Vec::new(), Vec::new());
    let parent_tree_iter = odb.find_tree_iter(parent_tree_id, &mut lhs_buf)?;
    let target_tree_iter = odb.find_tree_iter(target_tree_id, &mut rhs_buf)?;
    stats.trees_decoded += 2;

    let mut recorder = gix_diff::tree::Recorder::default();
    gix_diff::tree(parent_tree_iter, target_tree_iter, state, odb, &mut recorder)
        .map_err(Error::DiffTree)?;
    stats.trees_diffed += 1;

    let mut candidates: Vec<Candidate> = recorder
        .records
        .into_iter()
        .filter_map(|change| {
            use gix_diff::tree::recorder::Change;
            match change {
                // Only present in the target, so `!DIFF_FILE_VALID(p->one)`.
                Change::Addition { .. } => None,
                Change::Deletion {
                    entry_mode, oid, path, ..
                }
                | Change::Modification {
                    previous_entry_mode: entry_mode,
                    previous_oid: oid,
                    path,
                    ..
                } => entry_mode.is_blob_or_symlink().then_some((path, oid)),
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(candidates)
}

/// Every file the parent has, which is what `diff_opts.flags.find_copies_harder` makes
/// `diff_tree_oid()` feed to the queue: with it set, a filepair is emitted even for the paths the
/// two trees agree on.
fn everything_in_parent(
    odb: &(impl gix_object::Find + gix_object::FindHeader),
    parent_tree_id: &ObjectId,
    stats: &mut Statistics,
) -> Result<Vec<Candidate>, Error> {
    let mut buf = Vec::new();
    let parent_tree_iter = odb.find_tree_iter(parent_tree_id, &mut buf)?;
    stats.trees_decoded += 1;

    let mut recorder = gix_traverse::tree::Recorder::default();
    gix_traverse::tree::breadthfirst(
        parent_tree_iter,
        gix_traverse::tree::breadthfirst::State::default(),
        odb,
        &mut recorder,
    )
    .map_err(Error::TraverseTree)?;

    let mut candidates: Vec<Candidate> = recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob_or_symlink())
        .map(|entry| (entry.filepath, entry.oid))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(candidates)
}

/// `find_copy_in_parent()` for one scapegoat: offer everything `suspect` is still suspected for to
/// every file the parent has that is worth looking at.
#[expect(clippy::too_many_arguments)]
fn find_copy_in_parent(
    unblamed: &mut Vec<UnblamedHunk>,
    toosmall: &mut Vec<UnblamedHunk>,
    blamed: &mut Vec<UnblamedHunk>,
    suspect: Suspect,
    parent: &CopyParent,
    target_tree_id: &ObjectId,
    copy: CopyDetection,
    blamed_blob: &[u8],
    starts: &[usize],
    paths: &mut PathTable,
    odb: &(impl gix_object::Find + gix_object::FindHeader),
    origin_files: &mut OriginFiles,
    diff_state: &mut gix_diff::tree::State,
    diff_algorithm: gix_diff::blob::Algorithm,
    ignore_whitespace: bool,
    stats: &mut Statistics,
) -> Result<(), Error> {
    if unblamed.is_empty() {
        return Ok(());
    }

    // "-C enables copy from removed files; -C -C enables copy from existing files, but only when
    // blaming a new file; -C -C -C enables copy from existing files for everybody."
    let find_copies_harder = copy.hardest
        || (copy.harder && parent.porigin_path.is_none_or(|porigin| porigin != suspect.path));
    let candidates = if find_copies_harder {
        everything_in_parent(odb, &parent.tree_id, stats)?
    } else {
        changed_in_parent(odb, &parent.tree_id, target_tree_id, diff_state, stats)?
    };

    let porigin_path = parent.porigin_path.map(|id| paths.path(id).to_owned());
    let mut leftover = Vec::new();
    // Each round hands out at most one chunk per entry, leaving up to two smaller entries behind;
    // those go around again until a round finds nothing, exactly as git's `do { } while (unblamed)`.
    while !unblamed.is_empty() {
        // git rebuilds `blame_list` — one best-split slot per entry — at the top of every round.
        let mut best: Vec<Option<(Split, u32, Suspect)>> = vec![None; unblamed.len()];

        for (path, blob_id) in &candidates {
            // `find_move` already dealt with this path.
            if porigin_path.as_ref() == Some(path) {
                continue;
            }
            let parent_origin = Suspect::at(parent.commit_id, paths.intern(path.as_ref()));
            // `norigin = get_origin(parent, p->one->path)` followed by `fill_origin_blob()`
            // (`blame.c:2300-2304`). The origin is shared, so a candidate that is still around
            // from an earlier round — because it holds entries, or because it is the best split so
            // far — hands the blob over instead of it being read again.
            let parent_blob = origin_files.fill(parent_origin, blob_id.as_ref(), odb, stats)?;

            for (slot, hunk) in best.iter_mut().zip(unblamed.iter()) {
                let Some((split, score)) = moved::find_copy_in_blob(
                    hunk,
                    &parent_blob,
                    blamed_blob,
                    starts,
                    diff_algorithm,
                    ignore_whitespace,
                    stats,
                ) else {
                    continue;
                };
                // `copy_split_if_better()`: a later candidate of equal score replaces the earlier
                // one, so the comparison keeps the old best only when it is strictly better.
                match slot {
                    Some((_, best_score, _)) if score < *best_score => {}
                    _ => *slot = Some((split, score, parent_origin)),
                }
            }

            // `blame_origin_decref(norigin)` (`blame.c:2315`). The only references a candidate
            // origin can be under here are the splits that named it as their best so far
            // (`split_overlap()` increfs it) and the entries an earlier round handed it; without
            // either it is freed on the spot and the next round reads its blob again.
            let still_referenced = best
                .iter()
                .any(|slot| matches!(slot, Some((_, _, origin)) if *origin == parent_origin))
                || unblamed.iter().any(|hunk| hunk.has_suspect(&parent_origin))
                || blamed.iter().any(|hunk| hunk.has_suspect(&parent_origin));
            if !still_referenced {
                origin_files.drop_blob(&parent_origin);
            }
        }

        let mut requeued = Vec::new();
        for (hunk, slot) in std::mem::take(unblamed).into_iter().zip(best) {
            match slot {
                Some((split, score, parent_origin)) if copy.score < score => {
                    moved::split_blame(hunk, suspect, parent_origin, split, blamed, &mut requeued);
                }
                _ => leftover.push(hunk),
            }
        }
        *unblamed = moved::filter_small(blamed_blob, starts, toosmall, requeued, copy.score);
    }
    *unblamed = leftover;
    Ok(())
}

/// The `PICKAXE_BLAME_COPY` block of git's `pass_blame()`.
#[expect(clippy::too_many_arguments)]
pub(super) fn find_copies_in_parents(
    hunks_to_blame: Vec<UnblamedHunk>,
    suspect: Suspect,
    target_tree_id: ObjectId,
    parents: &[CopyParent],
    copy: CopyDetection,
    blamed_blob: &[u8],
    starts: &[usize],
    paths: &mut PathTable,
    odb: &(impl gix_object::Find + gix_object::FindHeader),
    origin_files: &mut OriginFiles,
    diff_state: &mut gix_diff::tree::State,
    diff_algorithm: gix_diff::blob::Algorithm,
    ignore_whitespace: bool,
    stats: &mut Statistics,
) -> Result<Vec<UnblamedHunk>, Error> {
    let mut out = Vec::with_capacity(hunks_to_blame.len());
    let mut unblamed = Vec::new();
    for hunk in hunks_to_blame {
        if hunk.has_suspect(&suspect) {
            unblamed.push(hunk);
        } else {
            out.push(hunk);
        }
    }

    // git keeps one `toosmall` list across the move and copy passes and re-filters it here against
    // `copy_score`. The move pass has already put its own leftovers back into `hunks_to_blame`, so
    // filtering the whole lot at `copy_score` reproduces all three branches of that code.
    let mut toosmall = Vec::new();
    unblamed = moved::filter_small(blamed_blob, starts, &mut toosmall, unblamed, copy.score);

    for parent in parents {
        if unblamed.is_empty() {
            break;
        }
        find_copy_in_parent(
            &mut unblamed,
            &mut toosmall,
            &mut out,
            suspect,
            parent,
            &target_tree_id,
            copy,
            blamed_blob,
            starts,
            paths,
            odb,
            origin_files,
            diff_state,
            diff_algorithm,
            ignore_whitespace,
            stats,
        )?;
    }

    out.append(&mut toosmall);
    out.append(&mut unblamed);
    Ok(out)
}
