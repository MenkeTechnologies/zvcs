use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::rc::Rc;

use gix_diff::{blob::TokenSource, tree::Visit};
use gix_hash::ObjectId;
use gix_object::{
    FindExt,
    bstr::{BStr, BString},
};
use gix_traverse::commit::find as find_commit;
use smallvec::SmallVec;

use gix_diff::blob::compact;

use super::{Change, UnblamedHunk, copied, moved, process_changes, process_ignored_changes};
use crate::{
    BlameEntry, Error, Options, Outcome, Statistics,
    types::{BlamePathEntry, Children, OriginKey, PathTable, Suspect},
};

/// git's `blame_origin::file`: the blob an origin holds on to, so the next diff that needs it does
/// not read it from the object database again.
///
/// An entry lives exactly as long as the origin does in git, which is what makes
/// [`Statistics::num_read_blob`] a count of cache misses rather than of diffs: `pass_blame()`
/// releases the origin it just processed with `drop_origin_blob()` and releases every scapegoat
/// that came away with nothing (`blame.c:2572-2579`), while `pass_whole_blame()` moves the buffer
/// from one origin to the next instead of letting the second read the same bytes
/// (`blame.c:2347-2351`).
#[derive(Default)]
pub(super) struct OriginFiles {
    files: HashMap<Suspect, Rc<[u8]>>,
}

impl OriginFiles {
    /// `fill_origin_blob()` (`blame.c:1031`): the origin's blob, read — and counted — only when the
    /// origin does not already hold it.
    pub(super) fn fill(
        &mut self,
        origin: Suspect,
        blob_id: &gix_hash::oid,
        odb: &impl gix_object::Find,
        stats: &mut Statistics,
    ) -> Result<Rc<[u8]>, Error> {
        if let Some(file) = self.files.get(&origin) {
            return Ok(file.clone());
        }
        stats.num_read_blob += 1;
        let mut buf = Vec::new();
        let file: Rc<[u8]> = Rc::from(odb.find_blob(blob_id, &mut buf)?.data);
        self.files.insert(origin, file.clone());
        Ok(file)
    }

    /// Give `origin` a buffer it did not read itself — what the fake working-tree commit of
    /// [`Options::fake_commit`] leaves on the origin at `suspect`.
    pub(super) fn seed(&mut self, origin: Suspect, file: Rc<[u8]>) {
        self.files.entry(origin).or_insert(file);
    }

    /// `pass_whole_blame()`'s "Steal its file" (`blame.c:2347-2351`): the parent origin takes over
    /// the buffer, if it has none of its own and this one has one.
    pub(super) fn steal(&mut self, from: Suspect, to: Suspect) {
        if self.files.contains_key(&to) {
            return;
        }
        if let Some(file) = self.files.remove(&from) {
            self.files.insert(to, file);
        }
    }

    /// `drop_origin_blob()` (`blame.c:1065`).
    pub(super) fn drop_blob(&mut self, origin: &Suspect) {
        self.files.remove(origin);
    }
}

/// Produce a list of consecutive [`BlameEntry`] instances to indicate in which commits the ranges of the file
/// at `suspect:<file_path>` originated in.
///
/// ## Parameters
///
/// * `odb`
///    - Access to database objects, also for used for diffing.
///    - Should have an object cache for good diff performance.
/// * `suspect`
///    - The first commit to be responsible for parts of `file_path`.
/// * `cache`
///    - Optionally, the commitgraph cache.
/// * `resource_cache`
///    - Used for diffing trees.
/// * `file_path`
///    - A *slash-separated* worktree-relative path to the file to blame.
/// * `options`
///    - An instance of [`Options`].
///
/// ## The algorithm
///
/// *For brevity, `HEAD` denotes the starting point of the blame operation. It could be any commit, or even commits that
/// represent the worktree state.
///
/// We begin with one or more *Unblamed Hunks* and a single suspect, usually the `HEAD` commit as the commit containing the
/// *Blamed File*, so that it contains the entire file, with the first commit being a candidate for the entire *Blamed File*.
/// We traverse the commit graph starting at the first suspect, and see if there have been changes to `file_path`.
/// If so, we have found a *Source File* and a *Suspect* commit, and have hunks that represent these changes.
/// Now the *Unblamed Hunk* is split at the boundaries of each matching change, creating a new *Unblamed Hunk* on each side,
/// along with a [`BlameEntry`] to represent the match.
/// This is repeated until there are no non-empty *Unblamed Hunk*s left.
///
/// At a high level, what we want to do is the following:
///
/// - get the commit
/// - walk through its parents
///   - for each parent, do a diff and mark lines that don’t have a suspect yet (this is the term
///     used in `libgit2`), but that have been changed in this commit
///
/// The algorithm in `libgit2` works by going through parents and keeping a linked list of blame
/// suspects. It can be visualized as follows:
///
/// <---------------------------------------->
/// <---------------><----------------------->
/// <---><----------><----------------------->
/// <---><----------><-------><-----><------->
/// <---><---><-----><-------><-----><------->
/// <---><---><-----><-------><-----><-><-><->
pub fn file(
    odb: impl gix_object::Find + gix_object::FindHeader,
    suspect: ObjectId,
    cache: Option<gix_commitgraph::Graph>,
    resource_cache: &mut gix_diff::blob::Platform,
    file_path: &BStr,
    options: Options,
) -> Result<Outcome, Error> {
    let _span = gix_trace::coarse!("gix_blame::file()", ?file_path, ?suspect);

    let mut stats = Statistics::default();
    let (mut buf, mut buf2, mut buf3) = (Vec::new(), Vec::new(), Vec::new());
    let blamed_file_entry_id = find_path_entry_in_commit(
        &odb,
        &suspect,
        file_path,
        cache.as_ref(),
        &mut buf,
        &mut buf2,
        &mut stats,
    )?
    .ok_or_else(|| Error::FileMissing {
        file_path: file_path.to_owned(),
        commit_id: suspect,
    })?;
    let blamed_file_blob = odb.find_blob(&blamed_file_entry_id, &mut buf)?.data.to_vec();
    // `setup_scoreboard()` reads the final image into `sb->final_buf` and counts it
    // (`blame.c:2889`). It does *not* fill the starting origin's `file`, which is why the same
    // blame started at a commit reads one blob more than one started on the working tree, where a
    // fake commit hands that origin its buffer — see `options.fake_commit` below.
    stats.num_read_blob += 1;
    let num_lines_in_blamed = tokens_for_diffing(&blamed_file_blob).tokenize().count() as u32;

    // Binary or otherwise empty? `assign_blame()` then finds an origin with no entries and stops,
    // but `setup_scoreboard()` has already read the final image, so the counters are not empty.
    if num_lines_in_blamed == 0 {
        return Ok(Outcome {
            statistics: stats,
            ..Default::default()
        });
    }

    // git's `sb->lineno`, which `-M` needs to cut the entry's lines out of the final image.
    let blamed_line_starts = moved::line_starts(&blamed_file_blob);

    // git's `blame_origin` is a commit *and* a path; the path lives here so that a `Suspect` stays
    // a cheap `Copy` id. Index 0 is the path being blamed.
    let mut paths = PathTable::new(file_path);

    let ranges_to_blame = options.ranges.to_zero_based_exclusive_ranges(num_lines_in_blamed);
    let mut hunks_to_blame = ranges_to_blame
        .into_iter()
        .map(|range| UnblamedHunk::new(range, Suspect::new(suspect)))
        .collect::<Vec<_>>();

    // git's `blame_origin` graph, the two halves of it this port needs: the blob each origin is
    // holding, and the first scapegoat each origin managed to hand entries to.
    let mut origin_files = OriginFiles::default();
    let mut origin_previous: HashMap<Suspect, Suspect> = HashMap::new();

    // git's `fake_working_tree_commit()`, whose `pass_blame()` runs before the walk below starts.
    // Either way it leaves `suspect`'s origin holding the final image: through
    // `pass_whole_blame()`'s steal when the overlay matches the blob, and through the blob its
    // `pass_blame_to_parent()` read into that origin when it does not.
    if let Some(fake) = options.fake_commit {
        origin_files.seed(Suspect::new(suspect), Rc::from(blamed_file_blob.as_slice()));
        if !fake.passes_whole_blame {
            stats.num_commits += 1;
            stats.num_get_patch += 1;
            stats.num_read_blob += 1;
        }
    }

    let (mut buf, mut buf2) = (Vec::new(), Vec::new());
    let commit = find_commit(cache.as_ref(), &odb, &suspect, &mut buf)?;
    let mut queue: gix_revwalk::PriorityQueue<gix_date::SecondsSinceUnixEpoch, ObjectId> =
        gix_revwalk::PriorityQueue::new();
    // `setup_scoreboard()` picks the queue's ordering with `sb->commits.compare`:
    // `compare_commits_by_commit_date` walking backwards, `compare_commits_by_reverse_commit_date`
    // walking forwards (`blame.c:2784-2790`). This queue always pops the largest key, so the
    // forward walk is expressed by negating the commit time.
    let reverse = options.children.is_some();
    let queue_key = |time: gix_date::SecondsSinceUnixEpoch| if reverse { -time } else { time };
    queue.insert(queue_key(commit.commit_time()?), suspect);

    let mut out = Vec::new();
    let mut diff_state = gix_diff::tree::State::default();
    let mut previous_entry: Option<(Suspect, ObjectId)> = None;
    let mut blame_path = if options.debug_track_path {
        Some(Vec::new())
    } else {
        None
    };

    'outer: while let Some(commit_id) = queue.pop_value() {
        stats.commits_traversed += 1;
        if hunks_to_blame.is_empty() {
            break;
        }

        let commit = find_commit(cache.as_ref(), &odb, &commit_id, &mut buf)?;
        let commit_time = commit.commit_time()?;
        let target_tree_id = commit.tree_id()?;
        // git's `first_scapegoat()` (`blame.c:2367-2380`). Walking backwards it is the commit's
        // parents, with `revs->first_parent_only` freeing `commit->parents->next` first so that
        // every later lookup on this commit sees exactly one parent and the side branches of a
        // merge are never walked. Walking forwards it is `revs->children` instead, which the
        // caller built over the range and which already reflects `--first-parent`.
        let parent_ids: ParentIds = match &options.children {
            Some(children) => collect_children(children, &commit_id, &odb, cache.as_ref(), &mut buf2)?,
            None => {
                // `first_scapegoat()` reads `commit->parents`, which
                // `parse_commit_buffer()` already replaced from the graft table
                // (commit.c:554-590) — so the substitution belongs before the
                // `--first-parent` truncation, exactly as it does in git.
                let grafted = options.grafts.as_ref().and_then(|g| g.parents_of(&commit_id));
                let mut ids = match grafted {
                    Some(grafted) => parent_ids_with_times(grafted, &odb, &mut buf2),
                    None => collect_parents(commit, &odb, cache.as_ref(), &mut buf2)?,
                };
                if options.first_parent {
                    ids.truncate(1);
                }
                ids
            }
        };
        let parent_ids = parent_ids;

        // git's `assign_blame()` does not pop the commit while any of its origins still has
        // entries: it picks one origin, runs `pass_blame()` for it, takes responsibility for
        // whatever is left, and looks at the same commit again. With `-C` a commit really can be
        // the suspect for chunks that came from several of its files, so each of those paths gets
        // its own round here.
        'origin: while let Some(suspect) = hunks_to_blame
            .iter()
            .find_map(|hunk| hunk.first_suspect_of(&commit_id))
        {
            let current_file_path = paths.path(suspect.path).to_owned();

            // `assign_blame()`'s `!(commit->object.flags & UNINTERESTING)` and its
            // `!(revs->max_age != -1 && commit->date < revs->max_age)`, which are one condition in
            // git and so are one block here: either way `pass_blame()` is skipped and the commit
            // keeps what is still suspected of it. See [`Options::bottom`].
            if options.bottom.contains(&commit_id)
                || options.since.is_some_and(|since| commit_time < since.seconds)
            {
                if unblamed_to_out_is_done(&mut hunks_to_blame, &mut out, suspect, &paths) {
                    break 'outer;
                }

                continue 'origin;
            }

            // A parentless commit has nothing to pass anything to, so it keeps whatever is
            // still suspected of it — git's `assign_blame()` runs `pass_blame()` (which walks
            // no scapegoats here) and then hands every entry left on the origin to
            // `found_guilty_entry()`. This must not wait for the queue to run dry: a root
            // reached while other commits are still pending would otherwise keep its suspect,
            // and the `'origin` loop, which re-selects that same suspect, would never end. Any
            // repository whose history merges reaches its root that way.
            if parent_ids.is_empty() {
                let done = unblamed_to_out_is_done(&mut hunks_to_blame, &mut out, suspect, &paths);
                if let Some(ref mut blame_path) = blame_path {
                    let entry = previous_entry
                        .take()
                        .filter(|(id, _)| *id == suspect)
                        .map(|(_, entry)| entry);

                    let blame_path_entry = BlamePathEntry {
                        source_file_path: current_file_path.clone(),
                        previous_source_file_path: None,
                        commit_id,
                        blob_id: entry.unwrap_or(gix_hash::Kind::shortest().null()),
                        previous_blob_id: gix_hash::Kind::shortest().null(),
                        parent_index: 0,
                    };
                    blame_path.push(blame_path_entry);
                }
                if done {
                    break 'outer;
                }
                // There is more, keep looking.
                continue 'origin;
            }

            let mut entry = previous_entry
                .take()
                .filter(|(id, _)| *id == suspect)
                .map(|(_, entry)| entry);
            if entry.is_none() {
                entry = find_path_entry_in_commit(
                    &odb,
                    &commit_id,
                    current_file_path.as_ref(),
                    cache.as_ref(),
                    &mut buf,
                    &mut buf2,
                    &mut stats,
                )?;
            }

            let Some(entry_id) = entry else {
                // The origin's path is not in this commit's tree, so there is nothing to diff and
                // nothing to pass on. git's `assign_blame()` then simply takes responsibility for
                // the entries that are left.
                if unblamed_to_out_is_done(&mut hunks_to_blame, &mut out, suspect, &paths) {
                    break 'outer;
                }
                continue 'origin;
            };

        // This block asserts that, for every `UnblamedHunk`, all lines in the *Blamed File* are
        // identical to the corresponding lines in the *Source File*.
        //
        // Under `-w` the walk diffs whitespace-normalized text (see `blob_changes`), so a hunk may
        // legitimately survive a commit that only re-indented it. The invariant then holds modulo
        // the very normalization the diff used, which is what is compared here.
        #[cfg(debug_assertions)]
        {
            let source_blob = odb.find_blob(&entry_id, &mut buf)?.data.to_vec();
            let normalize = |token: &[u8]| -> BString {
                if options.ignore_whitespace {
                    BString::new(strip_whitespace_per_line(token))
                } else {
                    BString::new(token.into())
                }
            };
            let mut source_interner = gix_diff::blob::Interner::new(source_blob.len() / 100);
            let source_lines_as_tokens: Vec<_> = tokens_for_diffing(&source_blob)
                .tokenize()
                .map(|token| source_interner.intern(token))
                .collect();

            let mut blamed_interner = gix_diff::blob::Interner::new(blamed_file_blob.len() / 100);
            let blamed_lines_as_tokens: Vec<_> = tokens_for_diffing(&blamed_file_blob)
                .tokenize()
                .map(|token| blamed_interner.intern(token))
                .collect();

            for hunk in hunks_to_blame.iter() {
                // A hunk that the `--ignore-rev` re-attribution moved to a fuzzily matched line in
                // the parent is, by construction, only *similar* to the blamed lines.
                if hunk.ignored || hunk.unblamable {
                    continue;
                }
                if let Some(range_in_suspect) = hunk.get_range(&suspect) {
                    let range_in_blamed_file = hunk.range_in_blamed_file.clone();

                    let source_lines = range_in_suspect
                        .clone()
                        .map(|i| normalize(source_interner[source_lines_as_tokens[i as usize]]))
                        .collect::<Vec<_>>();
                    let blamed_lines = range_in_blamed_file
                        .clone()
                        .map(|i| normalize(blamed_interner[blamed_lines_as_tokens[i as usize]]))
                        .collect::<Vec<_>>();

                    assert_eq!(source_lines, blamed_lines);
                }
            }
        }

            let mut passed_whole_blame = false;
            for (pid, (parent_id, parent_commit_time)) in parent_ids.iter().enumerate() {
                if let Some(parent_entry_id) = find_path_entry_in_commit(
                    &odb,
                    parent_id,
                    current_file_path.as_ref(),
                    cache.as_ref(),
                    &mut buf,
                    &mut buf2,
                    &mut stats,
                )? {
                    let no_change_in_entry = entry_id == parent_entry_id;
                    if pid == 0 {
                        previous_entry = Some((Suspect::at(*parent_id, suspect.path), parent_entry_id));
                    }
                    if no_change_in_entry {
                        // git's `pass_whole_blame()` followed by `goto finish`. It happens before
                        // `sb->num_commits++`, so a commit that gave everything away this way is
                        // not counted, and "Steal its file" hands the parent origin whatever blob
                        // this one had rather than letting it read the same bytes again.
                        let parent_origin = Suspect::at(*parent_id, suspect.path);
                        origin_files.steal(suspect, parent_origin);
                        pass_blame_from_to(suspect, parent_origin, &mut hunks_to_blame);
                        queue.insert(queue_key(*parent_commit_time), *parent_id);
                        passed_whole_blame = true;
                        break;
                    }
                }
            }
            if passed_whole_blame {
                continue 'origin;
            }

            // git's `pass_blame()` runs the ordinary diff against every parent first, and only then, for
            // an ignored commit, re-runs it against every parent with `ignore_diffs` set. What each of
            // those second passes needs is recorded here as the first pass goes.
            let suspect_is_ignored = options.ignore_revs.contains(&commit_id);
            // `(parent origin, changes, parent blob, target blob)`
            type IgnoredPass = (Suspect, Vec<Change>, Vec<u8>, Vec<u8>);
            let mut ignored_passes: Vec<IgnoredPass> = Vec::new();
            // `sg_origin[]` in git's `pass_blame()`: the origin each parent holds the blamed file
            // at, and the blob it has there. `-M` re-diffs whatever is left over against these.
            let mut move_parents: Vec<moved::ParentOrigin> = Vec::new();
            // What `find_copy_in_parent()` needs from every scapegoat, `porigin` included — unlike
            // `-M`, `-C` also visits the parents that do not have the blamed file at all.
            let mut copy_parents: Vec<copied::CopyParent> = Vec::new();

            // The second shape `pass_whole_blame()` comes in: `find_origin()` found nothing at the
            // origin's own path, but `find_rename()` found the file under another name with the
            // very same blob, so the rename carried no change at all and the whole origin is
            // handed over (`blame.c:2452-2459`).
            let mut renamed_whole_blame = false;

            // `sb->num_commits++` (`blame.c:2473`): one per origin that had scapegoats and did not
            // give everything away first. git resolves every scapegoat before it gets here, so it
            // already knows about both shapes of `pass_whole_blame()`; here the rename shape only
            // becomes visible once the tree diff in the loop below has run, which is why that
            // branch takes the count back off again.
            stats.num_commits += 1;

            let more_than_one_parent = parent_ids.len() > 1;
            for (index, (parent_id, parent_commit_time)) in parent_ids.iter().enumerate() {
                queue.insert(queue_key(*parent_commit_time), *parent_id);
                // `pass_blame()` stops offering scapegoats the moment the origin has nothing left
                // (`blame.c:2485-2486`), and `pass_blame_to_parent()` would return without
                // counting anyway (`blame.c:1952`).
                if !hunks_to_blame.iter().any(|hunk| hunk.has_suspect(&suspect)) {
                    continue;
                }
                if options.detect_copied.is_some() {
                    let tree_id = find_commit(cache.as_ref(), &odb, parent_id, &mut buf)?.tree_id()?;
                    copy_parents.push(copied::CopyParent {
                        commit_id: *parent_id,
                        tree_id,
                        porigin_path: None,
                    });
                }
                let changes_for_file_path = tree_diff_at_file_path(
                    &odb,
                    current_file_path.as_ref(),
                    commit_id,
                    *parent_id,
                    cache.as_ref(),
                    &mut stats,
                    &mut diff_state,
                    resource_cache,
                    &mut buf,
                    &mut buf2,
                    &mut buf3,
                    options.rewrites,
                )?;
                let Some(modification) = changes_for_file_path else {
                    if more_than_one_parent {
                        // None of the changes affected the file we’re currently blaming.
                        // Copy blame to parent.
                        for unblamed_hunk in &mut hunks_to_blame {
                            unblamed_hunk.clone_blame(suspect, Suspect::at(*parent_id, suspect.path));
                        }
                    } else {
                        pass_blame_from_to(suspect, Suspect::at(*parent_id, suspect.path), &mut hunks_to_blame);
                    }
                    continue;
                };

                match modification {
                    TreeDiffChange::Addition { id } => {
                        if more_than_one_parent {
                            // Do nothing under the assumption that this always (or almost always)
                            // implies that the file comes from a different parent, compared to which
                            // it was modified, not added.
                        } else if options.detect_copied.is_some() {
                            // The file is new here, so no parent origin exists to pass anything to
                            // — which is exactly the case `-C -C` widens the search for. Leave the
                            // entries where they are and let the copy pass below have them; what it
                            // does not place is finalized by the `retain_mut` at the end.
                        } else if unblamed_to_out_is_done(&mut hunks_to_blame, &mut out, suspect, &paths) {
                            if let Some(ref mut blame_path) = blame_path {
                                let blame_path_entry = BlamePathEntry {
                                    source_file_path: current_file_path.clone(),
                                    previous_source_file_path: None,
                                    commit_id,
                                    blob_id: id,
                                    previous_blob_id: gix_hash::Kind::shortest().null(),
                                    parent_index: index,
                                };
                                blame_path.push(blame_path_entry);
                            }

                            break 'outer;
                        }
                    }
                    TreeDiffChange::Deletion => {
                        unreachable!("We already found file_path in suspect^{{tree}}, so it can't be deleted")
                    }
                    TreeDiffChange::Modification { previous_id, id } => {
                        let parent_origin = Suspect::at(*parent_id, suspect.path);
                        push_move_parent(&mut move_parents, parent_origin, previous_id);
                        if let Some(parent) = copy_parents.last_mut() {
                            parent.porigin_path = Some(suspect.path);
                        }
                        // `origin->previous` (`blame.c:2480-2483`) is the first scapegoat this
                        // origin hands entries to, and it holds a reference for as long as this
                        // origin lives — which is what keeps that scapegoat in the refcount graph
                        // after every entry of its own has moved on.
                        origin_previous.entry(suspect).or_insert(parent_origin);
                        let parent_file = origin_files.fill(parent_origin, previous_id.as_ref(), &odb, &mut stats)?;
                        let target_file = origin_files.fill(suspect, id.as_ref(), &odb, &mut stats)?;
                        stats.num_get_patch += 1;
                        let (changes, data) = blob_changes(
                            &parent_file,
                            &target_file,
                            options.diff_algorithm,
                            options.ignore_whitespace,
                            options.indent_heuristic,
                            &mut stats,
                            suspect_is_ignored,
                        );
                        hunks_to_blame = process_changes(hunks_to_blame, changes.clone(), suspect, parent_origin);
                        if let Some((old_data, new_data)) = data {
                            ignored_passes.push((parent_origin, changes.clone(), old_data, new_data));
                        }
                        if let Some(ref mut blame_path) = blame_path {
                            let has_blame_been_passed =
                                hunks_to_blame.iter().any(|hunk| hunk.has_suspect(&parent_origin));

                            if has_blame_been_passed {
                                let blame_path_entry = BlamePathEntry {
                                    source_file_path: current_file_path.clone(),
                                    previous_source_file_path: Some(current_file_path.clone()),
                                    commit_id,
                                    blob_id: id,
                                    previous_blob_id: previous_id,
                                    parent_index: index,
                                };
                                blame_path.push(blame_path_entry);
                            }
                        }
                    }
                    TreeDiffChange::Rewrite {
                        source_location,
                        source_id,
                        id,
                    } => {
                        // The parent keeps the file under a different name, so its origin is a
                        // different path — which is what makes the *Source File* column appear.
                        let source_path = paths.intern(source_location.as_ref());
                        let parent_origin = Suspect::at(*parent_id, source_path);
                        if source_id == id {
                            origin_files.steal(suspect, parent_origin);
                            pass_blame_from_to(suspect, parent_origin, &mut hunks_to_blame);
                            renamed_whole_blame = true;
                            break;
                        }
                        push_move_parent(&mut move_parents, parent_origin, source_id);
                        if let Some(parent) = copy_parents.last_mut() {
                            parent.porigin_path = Some(source_path);
                        }
                        origin_previous.entry(suspect).or_insert(parent_origin);
                        let parent_file = origin_files.fill(parent_origin, source_id.as_ref(), &odb, &mut stats)?;
                        let target_file = origin_files.fill(suspect, id.as_ref(), &odb, &mut stats)?;
                        stats.num_get_patch += 1;
                        let (changes, data) = blob_changes(
                            &parent_file,
                            &target_file,
                            options.diff_algorithm,
                            options.ignore_whitespace,
                            options.indent_heuristic,
                            &mut stats,
                            suspect_is_ignored,
                        );
                        hunks_to_blame = process_changes(hunks_to_blame, changes.clone(), suspect, parent_origin);
                        if let Some((old_data, new_data)) = data {
                            ignored_passes.push((parent_origin, changes, old_data, new_data));
                        }

                        if let Some(ref mut blame_path) = blame_path {
                            let has_blame_been_passed =
                                hunks_to_blame.iter().any(|hunk| hunk.has_suspect(&parent_origin));

                            if has_blame_been_passed {
                                let blame_path_entry = BlamePathEntry {
                                    source_file_path: current_file_path.clone(),
                                    previous_source_file_path: Some(source_location.clone()),
                                    commit_id,
                                    blob_id: id,
                                    previous_blob_id: source_id,
                                    parent_index: index,
                                };
                                blame_path.push(blame_path_entry);
                            }
                        }
                    }
                }
            }

            if renamed_whole_blame {
                // `goto finish` before `sb->num_commits++`. A merge whose *later* scapegoat is the
                // one holding the file under a new name would also have had git skip the earlier
                // scapegoats' `pass_blame_to_parent()`, since it resolves all of them before it
                // diffs against any; those patches are counted here.
                stats.num_commits -= 1;
                continue 'origin;
            }

            // "Pass remaining suspects for ignored commits to their parents." (`blame.c`, `pass_blame`)
            for (parent_origin, changes, parent_blob, target_blob) in &ignored_passes {
                if !hunks_to_blame.iter().any(|hunk| hunk.has_suspect(&suspect)) {
                    break;
                }
                // A second `pass_blame_to_parent()`, so a second `sb->num_get_patch++`
                // (`blame.c:2500`). Both origins still hold the blobs the first pass read, so it
                // reads nothing; git then drops the parent's "so we can refresh the fingerprints
                // if we use the parent again" (`blame.c:2502-2506`).
                stats.num_get_patch += 1;
                hunks_to_blame = process_ignored_changes(
                    hunks_to_blame,
                    changes,
                    suspect,
                    *parent_origin,
                    parent_blob,
                    target_blob,
                );
                origin_files.drop_blob(parent_origin);
            }

            // "Optionally find moves in parents' files." (`blame.c`, `pass_blame`)
            if let Some(move_score) = options.detect_moved {
                hunks_to_blame = moved::find_moves_in_parents(
                    hunks_to_blame,
                    suspect,
                    &move_parents,
                    move_score,
                    &blamed_file_blob,
                    &blamed_line_starts,
                    &odb,
                    &mut origin_files,
                    options.diff_algorithm,
                    options.ignore_whitespace,
                    options.indent_heuristic,
                    &mut stats,
                )?;
            }

            // "Optionally find copies from parents' files." (`blame.c`, `pass_blame`)
            if let Some(copy) = options.detect_copied {
                hunks_to_blame = copied::find_copies_in_parents(
                    hunks_to_blame,
                    suspect,
                    target_tree_id,
                    &copy_parents,
                    copy,
                    &blamed_file_blob,
                    &blamed_line_starts,
                    &mut paths,
                    &odb,
                    &mut origin_files,
                    &mut diff_state,
                    options.diff_algorithm,
                    options.ignore_whitespace,
                    options.indent_heuristic,
                    &mut stats,
                )?;
            }

            // git's `assign_blame()` hands the entries a commit stayed responsible for to
            // `found_guilty_entry()` right here, in `origin->suspects` order — which `blame_merge()`
            // keeps sorted by the line in the *Blamed File*. `sort_batch_from` restores that order for
            // the entries this iteration contributes, so `Outcome::uncoalesced_entries` matches the
            // sequence git streams.
            let batch_start = out.len();
            hunks_to_blame.retain_mut(|unblamed_hunk| {
                if unblamed_hunk.suspects.len() == 1 {
                    if let Some(entry) = BlameEntry::from_unblamed_hunk(unblamed_hunk, suspect, &paths) {
                        // At this point, we have copied blame for every hunk to a parent. Hunks
                        // that have only `suspect` left in `suspects` have not passed blame to any
                        // parent, and so they can be converted to a `BlameEntry` and moved to
                        // `out`.
                        out.push(entry);
                        return false;
                    }
                }
                unblamed_hunk.remove_blame(suspect);
                true
            });
            sort_batch_from(&mut out, batch_start);

            // `pass_blame()`'s `finish:` (`blame.c:2572-2579`): every scapegoat that came away
            // with nothing releases its blob, and so does the origin that was just processed —
            // which is why the same blob can be read more than once over a walk, and why
            // [`Statistics::num_read_blob`] is larger than the number of distinct blobs involved.
            for (parent_origin, _) in &move_parents {
                if !hunks_to_blame.iter().any(|hunk| hunk.has_suspect(parent_origin)) {
                    origin_files.drop_blob(parent_origin);
                }
            }
            origin_files.drop_blob(&suspect);
        }
    }

    debug_assert_eq!(
        hunks_to_blame,
        vec![],
        "only if there is no portion of the file left we have completed the blame"
    );

    // `out` is in the order the walk finalized the entries, which is what
    // `--incremental` streams; the sorted-and-coalesced form is a separate view of it.
    let uncoalesced_entries = out.clone();
    // I don’t know yet whether it would make sense to use a data structure instead that preserves
    // order on insertion.
    out.sort_by_key(|a| a.start_in_blamed_file);
    let entries = coalesce_blame_entries(out);
    let key_of = |origin: &Suspect| -> OriginKey { (origin.commit_id, paths.source_file_name(origin.path)) };
    let suspect_previous: BTreeMap<OriginKey, OriginKey> = origin_previous
        .iter()
        .map(|(origin, parent)| (key_of(origin), key_of(parent)))
        .collect();
    let suspect_refcounts = suspect_refcounts(&entry_counts(&entries), &suspect_previous);
    Ok(Outcome {
        entries,
        uncoalesced_entries,
        blob: blamed_file_blob,
        statistics: stats,
        blame_path,
        suspect_refcounts,
        suspect_previous,
    })
}

/// How many blame entries name each origin, which is how many references those entries hold.
pub(super) fn entry_counts(entries: &[BlameEntry]) -> BTreeMap<OriginKey, u32> {
    let mut counts: BTreeMap<OriginKey, u32> = BTreeMap::new();
    for entry in entries {
        *counts
            .entry((entry.commit_id, entry.source_file_name.clone()))
            .or_default() += 1;
    }
    counts
}

/// git's `blame_origin::refcnt` for every origin still alive once the walk is over — the `%02d`
/// column of `git blame --score-debug` (`builtin/blame.c:535`).
///
/// The references an origin can still be under at that point are the blame entries that name it —
/// one each, since `blame_coalesce()` drops a reference for every entry it merges away
/// (`blame.c:1201`), so `entries` must be the *coalesced* list the output is printed from — and
/// the [`Outcome::suspect_previous`] pointers of other origins. A `previous` pointer only counts
/// while the origin holding it is itself alive, because `blame_origin_decref()` follows the chain
/// down as it frees (`blame.c:48-49`); so liveness is a closure over `previous`, seeded by the
/// origins that have entries. Origins outside that closure have already been freed and are absent
/// from the result.
///
/// A caller that lays a working-tree overlay over the walk's entries counts the fake commit's own
/// entries under the null id and, unless it passed the whole blame down
/// ([`FakeCommit::passes_whole_blame`](crate::FakeCommit::passes_whole_blame)), adds its `previous`
/// edge from that key to the origin at the suspect.
pub fn suspect_refcounts(
    entries: &BTreeMap<OriginKey, u32>,
    previous: &BTreeMap<OriginKey, OriginKey>,
) -> BTreeMap<OriginKey, u32> {
    let mut live: std::collections::BTreeSet<OriginKey> = entries.keys().cloned().collect();
    let mut pending: Vec<OriginKey> = live.iter().cloned().collect();
    while let Some(key) = pending.pop() {
        if let Some(parent) = previous.get(&key) {
            if live.insert(parent.clone()) {
                pending.push(parent.clone());
            }
        }
    }

    let mut refcounts = entries.clone();
    for key in &live {
        if let Some(parent) = previous.get(key) {
            *refcounts.entry(parent.clone()).or_default() += 1;
        }
    }
    refcounts.retain(|key, _| live.contains(key));
    refcounts
}

/// Order the entries `out` gained since it was `batch_start` long by their position in the
/// *Blamed File*.
///
/// One iteration of the walk contributes one such batch, and git emits the corresponding entries
/// from `origin->suspects`, a list `blame_merge()` keeps sorted by that same position.
fn sort_batch_from(out: &mut [BlameEntry], batch_start: usize) {
    out[batch_start..].sort_by_key(|entry| entry.start_in_blamed_file);
}

/// Record one parent's `sg_origin[]` entry for `-M`.
///
/// git skips a parent whose blob an earlier parent already contributed (`pass_blame`'s `same`
/// check), so that a merge does not offer the same content twice.
fn push_move_parent(parents: &mut Vec<moved::ParentOrigin>, parent_origin: Suspect, blob_id: ObjectId) {
    if parents.iter().any(|(_, blob)| *blob == blob_id) {
        return;
    }
    parents.push((parent_origin, blob_id));
}

/// Pass ownership of each unblamed hunk of `from` to `to`.
///
/// This happens when `from` didn't actually change anything in the blamed file.
fn pass_blame_from_to(from: Suspect, to: Suspect, hunks_to_blame: &mut Vec<UnblamedHunk>) {
    for unblamed_hunk in hunks_to_blame {
        unblamed_hunk.pass_blame(from, to);
    }
}

/// Convert each of the unblamed hunk in `hunks_to_blame` into a [`BlameEntry`], consuming them in the process.
///
/// Return `true` if we are done because `hunks_to_blame` is empty.
fn unblamed_to_out_is_done(
    hunks_to_blame: &mut Vec<UnblamedHunk>,
    out: &mut Vec<BlameEntry>,
    suspect: Suspect,
    paths: &PathTable,
) -> bool {
    let mut without_suspect = Vec::new();
    let batch_start = out.len();
    out.extend(hunks_to_blame.drain(..).filter_map(|hunk| {
        BlameEntry::from_unblamed_hunk(&hunk, suspect, paths).or_else(|| {
            without_suspect.push(hunk);
            None
        })
    }));
    sort_batch_from(out, batch_start);
    *hunks_to_blame = without_suspect;
    hunks_to_blame.is_empty()
}

/// This function merges adjacent blame entries. It merges entries that are adjacent both in the
/// blamed file and in the source file that introduced them. This follows `git`’s
/// behaviour. `libgit2`, as of 2024-09-19, only checks whether two entries are adjacent in the
/// blamed file which can result in different blames in certain edge cases. See [the commit][1]
/// that introduced the extra check into `git` for context. See [this commit][2] for a way to test
/// for this behaviour in `git`.
///
/// [1]: https://github.com/git/git/commit/c2ebaa27d63bfb7c50cbbdaba90aee4efdd45d0a
/// [2]: https://github.com/git/git/commit/6dbf0c7bebd1c71c44d786ebac0f2b3f226a0131
fn coalesce_blame_entries(lines_blamed: Vec<BlameEntry>) -> Vec<BlameEntry> {
    let len = lines_blamed.len();
    lines_blamed
        .into_iter()
        .fold(Vec::with_capacity(len), |mut acc, entry| {
            let previous_entry = acc.last();

            if let Some(previous_entry) = previous_entry {
                let previous_blamed_range = previous_entry.range_in_blamed_file();
                let current_blamed_range = entry.range_in_blamed_file();
                let previous_source_range = previous_entry.range_in_source_file();
                let current_source_range = entry.range_in_source_file();
                if previous_entry.commit_id == entry.commit_id
                    && previous_blamed_range.end == current_blamed_range.start
                    // As of 2024-09-19, the check below only is in `git`, but not in `libgit2`.
                    && previous_source_range.end == current_source_range.start
                    // `blame_coalesce()` refuses to merge entries that differ in these, so that the
                    // `--ignore-rev` markers stay attached to exactly the lines they describe.
                    && previous_entry.ignored == entry.ignored
                    && previous_entry.unblamable == entry.unblamable
                {
                    let coalesced_entry = BlameEntry {
                        start_in_blamed_file: previous_blamed_range.start as u32,
                        start_in_source_file: previous_source_range.start as u32,
                        len: NonZeroU32::new((current_source_range.end - previous_source_range.start) as u32)
                            .expect("BUG: hunks are never zero-sized"),
                        commit_id: previous_entry.commit_id,
                        source_file_name: previous_entry.source_file_name.clone(),
                        ignored: previous_entry.ignored,
                        unblamable: previous_entry.unblamable,
                    };

                    acc.pop();
                    acc.push(coalesced_entry);
                } else {
                    acc.push(entry);
                }

                acc
            } else {
                acc.push(entry);

                acc
            }
        })
}

/// The union of [`gix_diff::tree::recorder::Change`] and [`gix_diff::tree_with_rewrites::Change`],
/// keeping only the blame-relevant information.
enum TreeDiffChange {
    Addition {
        id: ObjectId,
    },
    Deletion,
    Modification {
        previous_id: ObjectId,
        id: ObjectId,
    },
    Rewrite {
        source_location: BString,
        source_id: ObjectId,
        id: ObjectId,
    },
}

impl From<gix_diff::tree::recorder::Change> for TreeDiffChange {
    fn from(value: gix_diff::tree::recorder::Change) -> Self {
        use gix_diff::tree::recorder::Change;

        match value {
            Change::Addition { oid, .. } => Self::Addition { id: oid },
            Change::Deletion { .. } => Self::Deletion,
            Change::Modification { previous_oid, oid, .. } => Self::Modification {
                previous_id: previous_oid,
                id: oid,
            },
        }
    }
}

impl From<gix_diff::tree_with_rewrites::Change> for TreeDiffChange {
    fn from(value: gix_diff::tree_with_rewrites::Change) -> Self {
        use gix_diff::tree_with_rewrites::Change;

        match value {
            Change::Addition { id, .. } => Self::Addition { id },
            Change::Deletion { .. } => Self::Deletion,
            Change::Modification { previous_id, id, .. } => Self::Modification { previous_id, id },
            Change::Rewrite {
                source_location,
                source_id,
                id,
                ..
            } => Self::Rewrite {
                source_location,
                source_id,
                id,
            },
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn tree_diff_at_file_path(
    odb: impl gix_object::Find + gix_object::FindHeader,
    file_path: &BStr,
    id: ObjectId,
    parent_id: ObjectId,
    cache: Option<&gix_commitgraph::Graph>,
    stats: &mut Statistics,
    state: &mut gix_diff::tree::State,
    resource_cache: &mut gix_diff::blob::Platform,
    commit_buf: &mut Vec<u8>,
    lhs_tree_buf: &mut Vec<u8>,
    rhs_tree_buf: &mut Vec<u8>,
    rewrites: Option<gix_diff::Rewrites>,
) -> Result<Option<TreeDiffChange>, Error> {
    let parent_tree_id = find_commit(cache, &odb, &parent_id, commit_buf)?.tree_id()?;

    let parent_tree_iter = odb.find_tree_iter(&parent_tree_id, lhs_tree_buf)?;
    stats.trees_decoded += 1;

    let tree_id = find_commit(cache, &odb, &id, commit_buf)?.tree_id()?;

    let tree_iter = odb.find_tree_iter(&tree_id, rhs_tree_buf)?;
    stats.trees_decoded += 1;

    let result = tree_diff_without_rewrites_at_file_path(&odb, file_path, stats, state, parent_tree_iter, tree_iter)?;

    // Here, we follow git’s behaviour. We return when we’ve found a `Modification`. We try a
    // second time with rename tracking when the change is either an `Addition` or a `Deletion`
    // because those can turn out to have been a `Rewrite`.
    // TODO(perf): renames are usually rare enough to not care about the work duplication done here.
    //             But in theory, a rename tracker could be used by us, on demand, and we could stuff the
    //             changes in there and have it find renames, without repeating the diff.
    if matches!(result, Some(TreeDiffChange::Modification { .. })) {
        return Ok(result);
    }
    let Some(rewrites) = rewrites else {
        return Ok(result);
    };

    let result = tree_diff_with_rewrites_at_file_path(
        &odb,
        file_path,
        stats,
        state,
        resource_cache,
        parent_tree_iter,
        tree_iter,
        rewrites,
    )?;

    Ok(result)
}

fn tree_diff_without_rewrites_at_file_path(
    odb: impl gix_object::Find + gix_object::FindHeader,
    file_path: &BStr,
    stats: &mut Statistics,
    state: &mut gix_diff::tree::State,
    parent_tree_iter: gix_object::TreeRefIter<'_>,
    tree_iter: gix_object::TreeRefIter<'_>,
) -> Result<Option<TreeDiffChange>, Error> {
    struct FindChangeToPath {
        inner: gix_diff::tree::Recorder,
        interesting_path: BString,
        change: Option<gix_diff::tree::recorder::Change>,
    }

    impl FindChangeToPath {
        fn new(interesting_path: BString) -> Self {
            let inner =
                gix_diff::tree::Recorder::default().track_location(Some(gix_diff::tree::recorder::Location::Path));

            FindChangeToPath {
                inner,
                interesting_path,
                change: None,
            }
        }
    }

    impl Visit for FindChangeToPath {
        fn pop_front_tracked_path_and_set_current(&mut self) {
            self.inner.pop_front_tracked_path_and_set_current();
        }

        fn push_back_tracked_path_component(&mut self, component: &BStr) {
            self.inner.push_back_tracked_path_component(component);
        }

        fn push_path_component(&mut self, component: &BStr) {
            self.inner.push_path_component(component);
        }

        fn pop_path_component(&mut self) {
            self.inner.pop_path_component();
        }

        fn visit(&mut self, change: gix_diff::tree::visit::Change) -> gix_diff::tree::visit::Action {
            use gix_diff::tree::visit::Change::*;

            if self.inner.path() == self.interesting_path {
                self.change = Some(match change {
                    Deletion {
                        entry_mode,
                        oid,
                        relation,
                    } => gix_diff::tree::recorder::Change::Deletion {
                        entry_mode,
                        oid,
                        path: self.inner.path_clone(),
                        relation,
                    },
                    Addition {
                        entry_mode,
                        oid,
                        relation,
                    } => gix_diff::tree::recorder::Change::Addition {
                        entry_mode,
                        oid,
                        path: self.inner.path_clone(),
                        relation,
                    },
                    Modification {
                        previous_entry_mode,
                        previous_oid,
                        entry_mode,
                        oid,
                    } => gix_diff::tree::recorder::Change::Modification {
                        previous_entry_mode,
                        previous_oid,
                        entry_mode,
                        oid,
                        path: self.inner.path_clone(),
                    },
                });

                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }

    let mut recorder = FindChangeToPath::new(file_path.into());
    let result = gix_diff::tree(parent_tree_iter, tree_iter, state, &odb, &mut recorder);
    stats.trees_diffed += 1;

    match result {
        Ok(_) | Err(gix_diff::tree::Error::Cancelled) => Ok(recorder.change.map(Into::into)),
        Err(error) => Err(Error::DiffTree(error)),
    }
}

#[expect(clippy::too_many_arguments)]
fn tree_diff_with_rewrites_at_file_path(
    odb: impl gix_object::Find + gix_object::FindHeader,
    file_path: &BStr,
    stats: &mut Statistics,
    state: &mut gix_diff::tree::State,
    resource_cache: &mut gix_diff::blob::Platform,
    parent_tree_iter: gix_object::TreeRefIter<'_>,
    tree_iter: gix_object::TreeRefIter<'_>,
    rewrites: gix_diff::Rewrites,
) -> Result<Option<TreeDiffChange>, Error> {
    let mut change: Option<gix_diff::tree_with_rewrites::Change> = None;

    let options: gix_diff::tree_with_rewrites::Options = gix_diff::tree_with_rewrites::Options {
        location: Some(gix_diff::tree::recorder::Location::Path),
        rewrites: Some(rewrites),
    };
    let result = gix_diff::tree_with_rewrites(
        parent_tree_iter,
        tree_iter,
        resource_cache,
        state,
        &odb,
        |change_ref| -> Result<_, std::convert::Infallible> {
            if change_ref.location() == file_path {
                change = Some(change_ref.into_owned());
                Ok(std::ops::ControlFlow::Break(()))
            } else {
                Ok(std::ops::ControlFlow::Continue(()))
            }
        },
        options,
    );
    stats.trees_diffed_with_rewrites += 1;

    match result {
        Ok(_) | Err(gix_diff::tree_with_rewrites::Error::Diff(gix_diff::tree::Error::Cancelled)) => {
            Ok(change.map(Into::into))
        }
        Err(error) => Err(Error::DiffTreeWithRewrites(error)),
    }
}

/// What [`blob_changes`] returns: the diff as [`Change`]s, plus — when the caller asked for it —
/// the exact `(old, new)` bytes that were diffed, for the `--ignore-rev` fingerprints.
type BlobChanges = (Vec<Change>, Option<(Vec<u8>, Vec<u8>)>);

/// `pass_blame_to_parent()`'s `diff_hunks()` (`blame.c:1967`) over the two blobs the origins are
/// holding.
///
/// The bytes come straight out of [`OriginFiles`] rather than through a
/// [`gix_diff::blob::Platform`], because `git blame` never consults the `diff` attribute:
/// `fill_origin_blob()` (`blame.c:1031-1058`) reads the blob with `odb_read_object()` and hands
/// those bytes to the diff, so a path marked `-diff` (or matched by the `binary` macro) is still
/// diffed line by line. `prepare_diff()` does apply that attribute — it classifies such a resource
/// as `Data::Binary`, whose `as_slice()` is `None` — and diffing the two empty buffers that left
/// produced no hunks at all, so a hunk spanning more lines than the parent's blob has was passed
/// to that parent unchanged and the walk then indexed the parent's line table out of bounds
/// (`blame -s sub/nested.txt` on a `-diff` path panicked rather than printing a blame).
fn blob_changes(
    old_data: &[u8],
    new_data: &[u8],
    diff_algorithm: gix_diff::blob::Algorithm,
    ignore_whitespace: bool,
    indent_heuristic: bool,
    stats: &mut Statistics,
    collect_data: bool,
) -> BlobChanges {
    // The `--ignore-rev` re-attribution fingerprints exactly the bytes that were diffed here, as
    // git does by reusing `blame_origin::file` for both.
    let data = collect_data.then(|| (old_data.to_vec(), new_data.to_vec()));
    // `git blame -w` (XDF_IGNORE_WHITESPACE): compare lines with all whitespace removed,
    // so a whitespace-only change is not a change. Normalizing per line preserves the
    // line count, so the resulting hunk line-indices still map to the original lines.
    let (old_norm, new_norm);
    let input = if ignore_whitespace {
        old_norm = strip_whitespace_per_line(old_data);
        new_norm = strip_whitespace_per_line(new_data);
        gix_diff::blob::InternedInput::new(old_norm.as_slice(), new_norm.as_slice())
    } else {
        gix_diff::blob::InternedInput::new(old_data, new_data)
    };

    let diff = gix_diff::blob::Diff::compute(diff_algorithm, &input);
    // Which of several equally minimal placements a slider ends up in decides which commit a line
    // is blamed on, so the edit script is compacted exactly as git's `xdl_change_compact()` does it
    // rather than by `imara-diff`'s postprocessor. The indent heuristic measures the *original*
    // lines even when `-w` had the compared ones stripped, as git's `get_indent()` reads
    // `xdf->recs[i]->ptr`.
    // `xdl_change_compact()`'s indent scoring is `XDF_INDENT_HEURISTIC`, which
    // `cmd_blame()` takes from `revs.diffopt.xdl_opts` and nothing else
    // (`builtin/blame.c:952`: `xdl_opts |= revs.diffopt.xdl_opts & XDF_INDENT_HEURISTIC`), so
    // `--no-indent-heuristic` reaches every diff of the dig. `xdiff/xdiffi.c:876` guards the whole
    // scoring branch on the flag, which is what `None` here is.
    let compacted = {
        let (indent_before, indent_after);
        let indents = if indent_heuristic {
            indent_before = compact::line_indents(old_data);
            indent_after = compact::line_indents(new_data);
            Some((indent_before.as_slice(), indent_after.as_slice()))
        } else {
            None
        };
        let mut removed: Vec<bool> =
            (0..input.before.len() as u32).map(|i| diff.is_removed(i)).collect();
        let mut added: Vec<bool> = (0..input.after.len() as u32).map(|i| diff.is_added(i)).collect();
        compact::compact_flags(
            &mut removed,
            &mut added,
            diff_algorithm,
            &input.before,
            &input.after,
            indents,
        );
        compact::Changed { removed, added }
    };

    let mut last_seen_after_end = 0;
    let mut changes = compacted
        .hunks()
        .into_iter()
        .fold(Vec::new(), |mut hunks, (before, after)| {
            // This checks for unchanged hunks.
            if after.start > last_seen_after_end {
                hunks.push(Change::Unchanged(last_seen_after_end..after.start));
            }

            match (!before.is_empty(), !after.is_empty()) {
                (_, true) => {
                    hunks.push(Change::AddedOrReplaced(
                        after.start..after.end,
                        before.end - before.start,
                    ));
                }
                (true, false) => {
                    hunks.push(Change::Deleted(after.start, before.end - before.start));
                }
                (false, false) => unreachable!("BUG: the edit script has no empty hunks"),
            }

            last_seen_after_end = after.end;

            hunks
        });

    let total_number_of_lines = input.after.len() as u32;
    if input.after.len() > last_seen_after_end as usize {
        changes.push(Change::Unchanged(last_seen_after_end..total_number_of_lines));
    }

    stats.blobs_diffed += 1;
    (changes, data)
}

fn find_path_entry_in_commit(
    odb: &impl gix_object::Find,
    commit: &gix_hash::oid,
    file_path: &BStr,
    cache: Option<&gix_commitgraph::Graph>,
    buf: &mut Vec<u8>,
    buf2: &mut Vec<u8>,
    stats: &mut Statistics,
) -> Result<Option<ObjectId>, Error> {
    let tree_id = find_commit(cache, odb, commit, buf)?.tree_id()?;
    let tree_iter = odb.find_tree_iter(&tree_id, buf)?;
    stats.trees_decoded += 1;

    let res = tree_iter.lookup_entry(
        odb,
        buf2,
        file_path.split(|b| *b == b'/').inspect(|_| stats.trees_decoded += 1),
    )?;
    stats.trees_decoded -= 1;
    Ok(res.map(|e| e.oid))
}

type ParentIds = SmallVec<[(gix_hash::ObjectId, i64); 2]>;

/// Pair each id with the commit time the blame queue orders by, the way
/// [`collect_parents`] does for a commit read from the object database.
///
/// A parent that cannot be read gets time 0 — the same fallback, which matters
/// because a graft may well name a commit this repository does not have.
fn parent_ids_with_times(ids: &[gix_hash::ObjectId], odb: &impl gix_object::Find, buf: &mut Vec<u8>) -> ParentIds {
    ids.iter()
        .map(|id| {
            let time = odb
                .find_commit_iter(id.as_ref(), buf)
                .ok()
                .and_then(|parent| parent.committer().ok().map(|committer| committer.seconds()))
                .unwrap_or_default();
            (*id, time)
        })
        .collect()
}

fn collect_parents(
    commit: gix_traverse::commit::Either<'_, '_>,
    odb: &impl gix_object::Find,
    cache: Option<&gix_commitgraph::Graph>,
    buf: &mut Vec<u8>,
) -> Result<ParentIds, Error> {
    let mut parent_ids: ParentIds = Default::default();
    match commit {
        gix_traverse::commit::Either::CachedCommit(commit) => {
            let cache = cache
                .as_ref()
                .expect("find returned a cached commit, so we expect cache to be present");
            for parent_pos in commit.iter_parents() {
                let parent = cache.commit_at(parent_pos?);
                parent_ids.push((parent.id().to_owned(), parent.committer_timestamp() as i64));
            }
        }
        gix_traverse::commit::Either::CommitRefIter(commit_ref_iter) => {
            for id in commit_ref_iter.parent_ids() {
                let parent = odb.find_commit_iter(id.as_ref(), buf).ok();
                let parent_commit_time = parent
                    .and_then(|parent| parent.committer().ok().map(|committer| committer.seconds()))
                    .unwrap_or_default();
                parent_ids.push((id, parent_commit_time));
            }
        }
    }
    Ok(parent_ids)
}

/// `first_scapegoat()` under `sb->reverse` (`blame.c:2379`):
/// `lookup_decoration(&revs->children, commit)`, paired with each child's commit time so the queue
/// can be fed the same way the parents feed it walking backwards.
fn collect_children(
    children: &Children,
    commit_id: &ObjectId,
    odb: &impl gix_object::Find,
    cache: Option<&gix_commitgraph::Graph>,
    buf: &mut Vec<u8>,
) -> Result<ParentIds, Error> {
    let mut child_ids: ParentIds = Default::default();
    for child in children.get(commit_id).into_iter().flatten() {
        let commit_time = find_commit(cache, odb, child, buf)?.commit_time()?;
        child_ids.push((*child, commit_time));
    }
    Ok(child_ids)
}

/// Return an iterator over tokens for use in diffing. These are usually lines, but it's important
/// to unify them so the later access shows the right thing.
pub(crate) fn tokens_for_diffing(data: &[u8]) -> impl TokenSource<Token = &[u8]> {
    gix_diff::blob::sources::byte_lines(data)
}

/// Remove all in-line whitespace (space, tab, CR, form-feed, vertical-tab) while keeping every
/// `\n`, so `git blame -w` (`XDF_IGNORE_WHITESPACE`) treats a whitespace-only line change as no
/// change. Keeping every `\n` preserves the line count, so a diff of the stripped data yields
/// hunk line-indices that still map one-to-one onto the original lines.
pub fn strip_whitespace_per_line(data: &[u8]) -> Vec<u8> {
    data.iter()
        .copied()
        .filter(|&b| b == b'\n' || !matches!(b, b' ' | b'\t' | b'\r' | 0x0c | 0x0b))
        .collect()
}
