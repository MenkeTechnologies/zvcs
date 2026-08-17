use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString};
use smallvec::SmallVec;
use std::ops::RangeInclusive;
use std::{
    num::NonZeroU32,
    ops::{AddAssign, Range, SubAssign},
};

use crate::Error;
use crate::file::function::tokens_for_diffing;

/// A type to represent one or more line ranges to blame in a file.
///
/// It handles the conversion between git's 1-based inclusive ranges and the internal
/// 0-based exclusive ranges used by the blame algorithm.
///
/// # Examples
///
/// ```rust
/// use gix_blame::BlameRanges;
///
/// // Blame lines 20 through 40 (inclusive)
/// let range = BlameRanges::from_one_based_inclusive_range(20..=40);
///
/// // Blame multiple ranges
/// let mut ranges = BlameRanges::from_one_based_inclusive_ranges(vec![
///     1..=4, // Lines 1-4
///    10..=14, // Lines 10-14
/// ]
/// );
/// ```
///
/// # Line Number Representation
///
/// This type uses 1-based inclusive ranges to mirror `git`'s behaviour:
/// - A range of `20..=40` represents 21 lines, spanning from line 20 up to and including line 40
/// - This will be converted to `19..40` internally as the algorithm uses 0-based ranges that are exclusive at the end
///
/// # Empty Ranges
/// You can blame the entire file by calling `BlameRanges::default()`, or by passing an empty vector to `from_one_based_inclusive_ranges`.
#[derive(Debug, Clone, Default)]
pub enum BlameRanges {
    /// Blame the entire file.
    #[default]
    WholeFile,
    /// Blame ranges in 0-based exclusive format.
    PartialFile(Vec<Range<u32>>),
}

/// Lifecycle
impl BlameRanges {
    /// Create from a single 0-based range.
    ///
    /// Note that the input range is 1-based inclusive, as used by git, and
    /// the output is a zero-based `BlameRanges` instance.
    pub fn from_one_based_inclusive_range(range: RangeInclusive<u32>) -> Result<Self, Error> {
        let zero_based_range = Self::inclusive_to_zero_based_exclusive(range)?;
        Ok(Self::PartialFile(vec![zero_based_range]))
    }

    /// Create from multiple 0-based ranges.
    ///
    /// Note that the input ranges are 1-based inclusive, as used by git, and
    /// the output is a zero-based `BlameRanges` instance.
    ///
    /// If the input vector is empty, the result will be `WholeFile`.
    pub fn from_one_based_inclusive_ranges(ranges: Vec<RangeInclusive<u32>>) -> Result<Self, Error> {
        if ranges.is_empty() {
            return Ok(Self::WholeFile);
        }

        let zero_based_ranges = ranges
            .into_iter()
            .map(Self::inclusive_to_zero_based_exclusive)
            .collect::<Vec<_>>();
        let mut result = Self::PartialFile(vec![]);
        for range in zero_based_ranges {
            result.merge_zero_based_exclusive_range(range?);
        }
        Ok(result)
    }

    /// Convert a 1-based inclusive range to a 0-based exclusive range.
    fn inclusive_to_zero_based_exclusive(range: RangeInclusive<u32>) -> Result<Range<u32>, Error> {
        if range.start() == &0 {
            return Err(Error::InvalidOneBasedLineRange);
        }
        let start = range.start() - 1;
        let end = *range.end();
        Ok(start..end)
    }
}

impl BlameRanges {
    /// Add a single range to blame.
    ///
    /// The new range will be merged with any overlapping existing ranges.
    pub fn add_one_based_inclusive_range(&mut self, new_range: RangeInclusive<u32>) -> Result<(), Error> {
        let zero_based_range = Self::inclusive_to_zero_based_exclusive(new_range)?;
        self.merge_zero_based_exclusive_range(zero_based_range);

        Ok(())
    }

    /// Adds a new ranges, merging it with any existing overlapping ranges.
    fn merge_zero_based_exclusive_range(&mut self, new_range: Range<u32>) {
        match self {
            Self::PartialFile(ranges) => {
                // Partition ranges into those that don't overlap and those that do.
                let (mut non_overlapping, overlapping): (Vec<_>, Vec<_>) = ranges
                    .drain(..)
                    .partition(|range| new_range.end < range.start || range.end < new_range.start);

                let merged_range = overlapping.into_iter().fold(new_range, |acc, range| {
                    acc.start.min(range.start)..acc.end.max(range.end)
                });

                non_overlapping.push(merged_range);

                *ranges = non_overlapping;
                ranges.sort_by_key(|a| a.start);
            }
            Self::WholeFile => *self = Self::PartialFile(vec![new_range]),
        }
    }

    /// Gets zero-based exclusive ranges.
    pub fn to_zero_based_exclusive_ranges(&self, max_lines: u32) -> Vec<Range<u32>> {
        match self {
            Self::WholeFile => {
                let full_range = 0..max_lines;
                vec![full_range]
            }
            Self::PartialFile(ranges) => ranges
                .iter()
                .filter_map(|range| {
                    if range.end < max_lines {
                        return Some(range.clone());
                    }

                    if range.start < max_lines {
                        Some(range.start..max_lines)
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }
}

/// Options to be passed to [`file()`](crate::file()).
#[derive(Default, Debug, Clone)]
pub struct Options {
    /// The algorithm to use for diffing.
    pub diff_algorithm: gix_diff::blob::Algorithm,
    /// The ranges to blame in the file.
    pub ranges: BlameRanges,
    /// Don't consider commits before the given date.
    pub since: Option<gix_date::Time>,
    /// The commits the walk must not dig past — git's `UNINTERESTING` flag, which is what a
    /// bottom-ended revision range (`git blame <rev>..<rev>`, `git blame ^<rev>`) means.
    ///
    /// `assign_blame()` (`blame.c:2612-2620`) tests it in the same breath as [`Self::since`]:
    ///
    /// ```c
    /// if (sb->reverse ||
    ///     (!(commit->object.flags & UNINTERESTING) &&
    ///      !(revs->max_age != -1 && commit->date < revs->max_age)))
    ///         pass_blame(sb, suspect, opt);
    /// else {
    ///         commit->object.flags |= UNINTERESTING;
    ///         if (commit->object.parsed)
    ///                 mark_parents_uninteresting(sb->revs, commit);
    /// }
    /// ```
    ///
    /// So a commit in this set does not pass blame to anything: it keeps whatever is still
    /// suspected of it, which is both why the range's oldest reachable commits collect the lines
    /// the range did not touch and why they print with blame's boundary marker.
    ///
    /// The set must already be closed under ancestry, because git closes it in two places the
    /// blame walk cannot see: `limit_list()` marks the ancestors of every bottom while
    /// `prepare_revision_walk()` runs, and `mark_parents_uninteresting()` above extends it as the
    /// walk goes. Propagating here instead would miss an ancestor the walk reaches by some other
    /// path — the far side of a merge — which git has already marked.
    pub bottom: std::collections::HashSet<ObjectId>,
    /// Determine if rename tracking should be performed, and how.
    pub rewrites: Option<gix_diff::Rewrites>,
    /// Collect debug information whenever there's a diff or rename that affects the outcome of a
    /// blame.
    pub debug_track_path: bool,
    /// Ignore whitespace when diffing revisions (`git blame -w`): a line that changed only in
    /// whitespace is attributed to the earlier commit, not the whitespace-only change.
    pub ignore_whitespace: bool,
    /// Also blame the parents for lines that moved within the file (`git blame -M[<score>]`).
    ///
    /// The value is git's `sb->move_score` (`BLAME_DEFAULT_MOVE_SCORE`, 20, for a bare `-M`): the
    /// minimum [`blame_entry_score`] a chunk must exceed before it is handed to a parent it was
    /// only found in by searching the whole blob. It keeps a line like `\t}` — which occurs
    /// everywhere — from being credited to wherever it happens to also appear.
    ///
    /// [`blame_entry_score`]: https://github.com/git/git/blob/v2.55.0/blame.c#L1991
    pub detect_moved: Option<u32>,
    /// Commits whose changes should not be attributed to them (`git blame --ignore-rev`).
    ///
    /// After the usual diff has passed everything it can to the parents, the lines that are left
    /// over for an ignored commit are matched against the parent's lines by similarity, and the
    /// ones that find a match are passed on to the parent as well. This is git's `sb->ignore_list`.
    pub ignore_revs: std::collections::HashSet<ObjectId>,
    /// Also blame the parents for lines that were copied out of *another* file
    /// (`git blame -C[<score>]`), i.e. git's `PICKAXE_BLAME_COPY`.
    pub detect_copied: Option<CopyDetection>,
    /// Follow only the first parent of every commit (`git blame --first-parent`).
    ///
    /// This is git's `revs->first_parent_only`, which blame applies in
    /// [`first_scapegoat()`](https://github.com/git/git/blob/v2.55.0/blame.c#L2367): before a
    /// commit's parents are used as scapegoats, everything after the first one is dropped, so a
    /// merge only ever passes blame back along its first-parent line and the side branch is never
    /// entered.
    ///
    /// Ignored under [`Self::children`]: `first_scapegoat()` only consults `first_parent_only` on
    /// the forward side, and in reverse the caller expresses it by handing over a children map
    /// that follows the first-parent chain (`blame.c:2842-2859`).
    pub first_parent: bool,
    /// Walk the history *forwards* instead of backwards (`git blame --reverse <rev>..<rev>`).
    ///
    /// This is git's `sb->reverse` and the `revs->children` decoration it goes with, which is the
    /// whole of the inversion: with it set,
    /// [`first_scapegoat()`](https://github.com/git/git/blob/v2.55.0/blame.c#L2367) returns
    /// `lookup_decoration(&revs->children, commit)` instead of `commit->parents`, and
    /// `setup_scoreboard()` orders the commit queue by `compare_commits_by_reverse_commit_date`
    /// so the oldest commit is examined first. Everything else — the diff, the splitting, the
    /// handing over of entries — is the same code as forwards, which is why a line ends up
    /// attributed to the *last* commit that still had it.
    ///
    /// The map is `revs->children` itself: for every commit in the range, the commits in that same
    /// range that have it as a parent. git builds it in `set_children()` (`revision.c`) while
    /// `prepare_revision_walk()` runs, so it covers exactly the commits the range selected and
    /// stops the walk at the range's tips, where a commit has no children.
    ///
    /// The final image is the blob at `suspect`, which for `A..B` is the range's *oldest*
    /// endpoint `A` — git's `find_single_initial()`.
    pub children: Option<Children>,
    /// The synthetic commit `setup_scoreboard()` builds on top of `suspect` when the final image
    /// does not come out of `suspect`'s tree — git's
    /// [`fake_working_tree_commit()`](https://github.com/git/git/blob/v2.55.0/blame.c#L188), which
    /// is how `git blame <path>` blames the working tree and how `--contents` blames an arbitrary
    /// file.
    ///
    /// [`file()`](crate::file()) does not attribute the overlay's own lines — it starts at
    /// `suspect`, and mapping the overlay onto the attribution it produces is the caller's job.
    /// What is modelled here is the rest of what that commit is in git: an ordinary origin taking
    /// part in `pass_blame()`, which is why it shows up in [`Statistics`] and in the refcount graph
    /// behind [`Outcome::suspect_refcounts`].
    pub fake_commit: Option<FakeCommit>,
}

/// git's `revs->children`: the commits of the walked range that have a given commit as a parent,
/// in the order `first_scapegoat()` hands them to `pass_blame()`.
pub type Children = std::collections::HashMap<ObjectId, Vec<ObjectId>>;

/// The part of git's `fake_working_tree_commit()` origin that outlives the caller's own overlay
/// handling — see [`Options::fake_commit`].
///
/// The one thing it says is decided by the comparison the caller makes anyway, between the
/// overlay's bytes and the blob at `suspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeCommit {
    /// The overlay is byte-identical to `suspect`'s blob, so the fake commit's `pass_blame()` ends
    /// in [`pass_whole_blame()`](https://github.com/git/git/blob/v2.55.0/blame.c#L2342): it counts
    /// no commit and no patch, and hands `suspect`'s origin the buffer it had already read
    /// ("Steal its file") rather than letting it read the blob again.
    ///
    /// When this is `false` the fake commit diffs against `suspect` like any other commit: one
    /// `num_commits`, one `num_get_patch` and one `num_read_blob` for the blob it reads into
    /// `suspect`'s origin — which is cached either way, so the walk itself never re-reads it. It
    /// also gains a `blame_origin::previous` pointing at that origin, which the caller adds to
    /// [`Outcome::suspect_previous`] before asking [`suspect_refcounts()`](crate::suspect_refcounts())
    /// for the counts.
    pub passes_whole_blame: bool,
}

/// How an origin is named outside the walk: the commit, and the *Source File* name the entries of
/// that origin carry — `None` for the path being blamed. git's `fake_working_tree_commit()` has a
/// null object id, so that is the key its origin goes under.
pub type OriginKey = (ObjectId, Option<BString>);

/// How hard `git blame -C` looks for the file a chunk was copied from, i.e. which of git's
/// `PICKAXE_BLAME_COPY*` bits are set.
///
/// Each further `-C` widens the set of paths in the parent that a leftover chunk is compared
/// against, as `blame_copy_callback()` describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyDetection {
    /// git's `sb->copy_score` (`BLAME_DEFAULT_COPY_SCORE`, 40, for a bare `-C`): the minimum score
    /// a chunk must exceed before a parent is blamed for it.
    pub score: u32,
    /// `-C -C`: also compare against files the commit did *not* touch, but only while blaming a
    /// path the parent does not have under the same name.
    pub harder: bool,
    /// `-C -C -C`: compare against every file in the parent, always.
    pub hardest: bool,
}

/// Names a path the walk has seen, so that a [`Suspect`] stays `Copy`.
///
/// [`PathId::BLAMED`] is the path the blame was started on, which is the one git leaves out of the
/// *Source File* column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathId(u32);

impl PathId {
    /// The path that is being blamed, i.e. `sb->path`.
    pub const BLAMED: PathId = PathId(0);
}

/// The paths [`PathId`]s refer to, in the order the walk first saw them.
#[derive(Debug, Clone)]
pub(crate) struct PathTable {
    paths: Vec<BString>,
}

impl PathTable {
    pub(crate) fn new(blamed_path: &BStr) -> Self {
        Self {
            paths: vec![blamed_path.to_owned()],
        }
    }

    /// Return the id of `path`, adding it to the table if it is new.
    pub(crate) fn intern(&mut self, path: &BStr) -> PathId {
        match self.paths.iter().position(|known| known == path) {
            Some(index) => PathId(index as u32),
            None => {
                self.paths.push(path.to_owned());
                PathId((self.paths.len() - 1) as u32)
            }
        }
    }

    pub(crate) fn path(&self, id: PathId) -> &BStr {
        self.paths[id.0 as usize].as_ref()
    }

    /// The *Source File* name a [`BlameEntry`] carries for `id`: `None` for the blamed path itself,
    /// which is what git leaves out of its output.
    pub(crate) fn source_file_name(&self, id: PathId) -> Option<BString> {
        (id != PathId::BLAMED).then(|| self.paths[id.0 as usize].clone())
    }
}

/// git's `blame_origin`: a commit *and* the path the *Source File* lives at in it.
///
/// The path is part of the identity because `-C` can make one commit responsible for chunks that
/// came from several different files at once, each of which is a separate origin there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Suspect {
    /// The commit the chunk is suspected to have come from.
    pub commit_id: ObjectId,
    /// The path the *Source File* has in `commit_id`.
    pub path: PathId,
}

impl Suspect {
    /// An origin for `commit_id` at the path that is being blamed.
    pub fn new(commit_id: ObjectId) -> Self {
        Self {
            commit_id,
            path: PathId::BLAMED,
        }
    }

    /// The same commit, but with the *Source File* at `path`.
    pub fn at(commit_id: ObjectId, path: PathId) -> Self {
        Self { commit_id, path }
    }
}

/// Represents a change during history traversal for blame. It is supposed to capture enough
/// information to allow reconstruction of the way a blame was performed, i. e. the path the
/// history traversal, combined with repeated diffing of two subsequent states in this history, has
/// taken.
///
/// This is intended for debugging purposes.
#[derive(Clone, Debug)]
pub struct BlamePathEntry {
    /// The path to the *Source File* in the blob after the change.
    pub source_file_path: BString,
    /// The path to the *Source File* in the blob before the change. Allows
    /// detection of renames. `None` for root commits.
    pub previous_source_file_path: Option<BString>,
    /// The commit id associated with the state after the change.
    pub commit_id: ObjectId,
    /// The blob id associated with the state after the change.
    pub blob_id: ObjectId,
    /// The blob id associated with the state before the change.
    pub previous_blob_id: ObjectId,
    /// When there is more than one `BlamePathEntry` for a commit, this indicates to which parent
    /// commit the change is related.
    pub parent_index: usize,
}

/// The outcome of [`file()`](crate::file()).
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// One entry in sequential order, to associate a hunk in the blamed file with the source commit (and its lines)
    /// that introduced it.
    pub entries: Vec<BlameEntry>,
    /// The same attribution as [`Self::entries`], but neither sorted by line nor coalesced: the
    /// entries appear in the order the history walk took responsibility for them, which is what
    /// git's `found_guilty_entry()` callback sees and what `git blame --incremental` streams.
    ///
    /// Within one commit the entries are ordered by their first line in the *Blamed File*, which is
    /// the order git's `origin->suspects` list is kept in by `blame_merge()`.
    pub uncoalesced_entries: Vec<BlameEntry>,
    /// A buffer with the file content of the *Blamed File*, ready for tokenization.
    pub blob: Vec<u8>,
    /// Additional information about the amount of work performed to produce the blame.
    pub statistics: Statistics,
    /// Contains a log of all changes that affected the outcome of this blame.
    pub blame_path: Option<Vec<BlamePathEntry>>,
    /// git's `blame_origin::refcnt` for the origins [`Self::entries`] keeps alive, as
    /// [`suspect_refcounts()`](crate::suspect_refcounts()) computes it.
    ///
    /// A caller that changes which entries end up in the output — by laying a working-tree overlay
    /// over them, or by applying `-L` afterwards — has a different entry list from this one and
    /// should recompute from that list instead, since a reference is exactly one blame entry.
    pub suspect_refcounts: std::collections::BTreeMap<OriginKey, u32>,
    /// git's `blame_origin::previous`: for each origin that handed entries to a scapegoat, the
    /// first scapegoat it handed them to (`blame.c:2480-2483`).
    ///
    /// This is the origin `git blame --porcelain` names in its `previous` line
    /// (`builtin/blame.c:393-399`), which walking backwards is a parent and walking forwards is a
    /// child.
    pub suspect_previous: std::collections::BTreeMap<OriginKey, OriginKey>,
}

/// Additional information about the performed operations.
#[derive(Debug, Default, Copy, Clone)]
pub struct Statistics {
    /// The amount of commits it traversed until the blame was complete.
    pub commits_traversed: usize,
    /// The amount of trees that were decoded to find the entry of the file to blame.
    pub trees_decoded: usize,
    /// The amount of tree-diffs to see if the filepath was added, deleted or modified. These diffs
    /// are likely partial as they are cancelled as soon as a change to the blamed file is
    /// detected.
    pub trees_diffed: usize,
    /// The amount of tree-diffs to see if the file was moved (or rewritten, in git terminology).
    /// These diffs are likely partial as they are cancelled as soon as a change to the blamed file
    /// is detected.
    pub trees_diffed_with_rewrites: usize,
    /// The amount of blobs there were compared to each other to learn what changed between commits.
    /// Note that in order to diff a blob, one needs to load both versions from the database.
    pub blobs_diffed: usize,
    /// git's `blame_scoreboard::num_read_blob`, the first of the three counters
    /// `git blame --show-stats` prints.
    ///
    /// One for the final image `setup_scoreboard()` reads (`blame.c:2889`), plus one for every
    /// [`fill_origin_blob()`](https://github.com/git/git/blob/v2.55.0/blame.c#L1031) that found
    /// `blame_origin::file` empty (`blame.c:1039`). It is therefore a count of *misses* of a
    /// per-origin blob cache, not of diffs: the same blob is read once per origin that needs it and
    /// keeps needing it, and an origin that is handed a buffer by `pass_whole_blame()` never reads
    /// at all.
    pub num_read_blob: u32,
    /// git's `blame_scoreboard::num_get_patch`: one per
    /// [`pass_blame_to_parent()`](https://github.com/git/git/blob/v2.55.0/blame.c#L1944) that
    /// found entries left to hand over (`blame.c:1965`), including the second, `ignore_diffs` pass
    /// an `--ignore-rev` commit makes over the same parents.
    ///
    /// The `-M` and `-C` searches do not count here: they diff with `find_copy_in_blob()` rather
    /// than through `pass_blame_to_parent()`.
    pub num_get_patch: u32,
    /// git's `blame_scoreboard::num_commits`: one per
    /// [`pass_blame()`](https://github.com/git/git/blob/v2.55.0/blame.c#L2416) that reached
    /// `blame.c:2473`, i.e. per origin that had at least one scapegoat and did not already give
    /// everything away through `pass_whole_blame()`.
    ///
    /// It counts origins rather than commits: under `-C` one commit can be the suspect for chunks
    /// that came from several of its files, and each of those is its own `pass_blame()`.
    pub num_commits: u32,
}

impl Outcome {
    /// Return an iterator over each entry in [`Self::entries`], along with its lines, line by line.
    ///
    /// Note that [`Self::blob`] must be tokenized in exactly the same way as the tokenizer that was used
    /// to perform the diffs, which is what this method assures.
    pub fn entries_with_lines(&self) -> impl Iterator<Item = (BlameEntry, Vec<BString>)> + '_ {
        use gix_diff::blob::TokenSource;
        let mut interner = gix_diff::blob::Interner::new(self.blob.len() / 100);
        let lines_as_tokens: Vec<_> = tokens_for_diffing(&self.blob)
            .tokenize()
            .map(|token| interner.intern(token))
            .collect();
        self.entries.iter().map(move |e| {
            (
                e.clone(),
                lines_as_tokens[e.range_in_blamed_file()]
                    .iter()
                    .map(|token| BString::new(interner[*token].into()))
                    .collect(),
            )
        })
    }
}

/// Describes the offset of a particular hunk relative to the *Blamed File*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Offset {
    /// The amount of lines to add.
    Added(u32),
    /// The amount of lines to remove.
    Deleted(u32),
}

impl Offset {
    /// Shift the given `range` according to our offset.
    pub fn shifted_range(&self, range: &Range<u32>) -> Range<u32> {
        match self {
            Offset::Added(added) => {
                debug_assert!(range.start >= *added, "{self:?} {range:?}");
                Range {
                    start: range.start - added,
                    end: range.end - added,
                }
            }
            Offset::Deleted(deleted) => Range {
                start: range.start + deleted,
                end: range.end + deleted,
            },
        }
    }
}

impl AddAssign<u32> for Offset {
    fn add_assign(&mut self, rhs: u32) {
        match self {
            Self::Added(added) => *self = Self::Added(*added + rhs),
            Self::Deleted(deleted) => {
                if rhs > *deleted {
                    *self = Self::Added(rhs - *deleted);
                } else {
                    *self = Self::Deleted(*deleted - rhs);
                }
            }
        }
    }
}

impl SubAssign<u32> for Offset {
    fn sub_assign(&mut self, rhs: u32) {
        match self {
            Self::Added(added) => {
                if rhs > *added {
                    *self = Self::Deleted(rhs - *added);
                } else {
                    *self = Self::Added(*added - rhs);
                }
            }
            Self::Deleted(deleted) => *self = Self::Deleted(*deleted + rhs),
        }
    }
}

/// A mapping of a section of the *Blamed File* to the section in a *Source File* that introduced it.
///
/// Both ranges are of the same size, but may use different [starting points](Range::start). Naturally,
/// they have the same content, which is the reason they are in what is returned by [`file()`](crate::file()).
#[derive(Clone, Debug, PartialEq)]
pub struct BlameEntry {
    /// The index of the token in the *Blamed File* (typically lines) where this entry begins.
    pub start_in_blamed_file: u32,
    /// The index of the token in the *Source File* (typically lines) where this entry begins.
    ///
    /// This is possibly offset compared to `start_in_blamed_file`.
    pub start_in_source_file: u32,
    /// The amount of lines the hunk is spanning.
    pub len: NonZeroU32,
    /// The commit that introduced the section into the *Source File*.
    pub commit_id: ObjectId,
    /// The *Source File*'s name, in case it differs from *Blamed File*'s name.
    /// This happens when the file was renamed.
    pub source_file_name: Option<BString>,
    /// The lines were passed to this commit by the `--ignore-rev` re-attribution rather than by a
    /// diff, so they only *resemble* the lines of the ignored commit. git's `blame_entry::ignored`.
    pub ignored: bool,
    /// The lines belong to an ignored commit but no similar line was found in any parent, so no
    /// better origin than the ignored commit itself could be determined. git's
    /// `blame_entry::unblamable`.
    pub unblamable: bool,
}

impl BlameEntry {
    /// Create a new instance.
    pub fn new(
        range_in_blamed_file: Range<u32>,
        range_in_source_file: Range<u32>,
        commit_id: ObjectId,
        source_file_name: Option<BString>,
    ) -> Self {
        debug_assert!(
            range_in_blamed_file.end > range_in_blamed_file.start,
            "{range_in_blamed_file:?}"
        );
        debug_assert!(
            range_in_source_file.end > range_in_source_file.start,
            "{range_in_source_file:?}"
        );
        debug_assert_eq!(range_in_source_file.len(), range_in_blamed_file.len());

        Self {
            start_in_blamed_file: range_in_blamed_file.start,
            start_in_source_file: range_in_source_file.start,
            len: NonZeroU32::new(range_in_blamed_file.len() as u32).expect("BUG: hunks are never empty"),
            commit_id,
            source_file_name,
            ignored: false,
            unblamable: false,
        }
    }
}

impl BlameEntry {
    /// Return the range of tokens this entry spans in the *Blamed File*.
    pub fn range_in_blamed_file(&self) -> Range<usize> {
        let start = self.start_in_blamed_file as usize;
        start..start + self.len.get() as usize
    }
    /// Return the range of tokens this entry spans in the *Source File*.
    pub fn range_in_source_file(&self) -> Range<usize> {
        let start = self.start_in_source_file as usize;
        start..start + self.len.get() as usize
    }
}

pub(crate) trait LineRange {
    fn shift_by(&self, offset: Offset) -> Self;
}

impl LineRange for Range<u32> {
    fn shift_by(&self, offset: Offset) -> Self {
        offset.shifted_range(self)
    }
}

/// Tracks the hunks in the *Blamed File* that are not yet associated with the commit that introduced them.
#[derive(Debug, PartialEq)]
pub struct UnblamedHunk {
    /// The range in the file that is being blamed that this hunk represents.
    pub range_in_blamed_file: Range<u32>,
    /// Maps an origin — a commit *and* the path the file has in it — to the range in that source
    /// file that is equal to `range_in_blamed_file`. Since `suspects` rarely contains more than 1
    /// item, it can efficiently be stored as a `SmallVec`.
    pub suspects: SmallVec<[(Suspect, Range<u32>); 1]>,
    /// See [`BlameEntry::ignored`]. Sticky: it survives every later split of this hunk, as it does
    /// in git where `split_blame_at()` copies it onto the new entry.
    pub ignored: bool,
    /// See [`BlameEntry::unblamable`]. Sticky, like [`Self::ignored`].
    pub unblamable: bool,
}

impl UnblamedHunk {
    pub(crate) fn new(from_range_in_blamed_file: Range<u32>, suspect: Suspect) -> Self {
        let range_start = from_range_in_blamed_file.start;
        let range_end = from_range_in_blamed_file.end;

        UnblamedHunk {
            range_in_blamed_file: range_start..range_end,
            suspects: [(suspect, range_start..range_end)].into(),
            ignored: false,
            unblamable: false,
        }
    }

    pub(crate) fn has_suspect(&self, suspect: &Suspect) -> bool {
        self.suspects.iter().any(|entry| entry.0 == *suspect)
    }

    /// The first origin of `commit_id` this hunk is suspected for, whatever path it names.
    ///
    /// This is what git's `assign_blame()` finds when it walks `get_blame_suspects(commit)` for the
    /// next origin with entries left.
    pub(crate) fn first_suspect_of(&self, commit_id: &ObjectId) -> Option<Suspect> {
        self.suspects
            .iter()
            .find(|entry| entry.0.commit_id == *commit_id)
            .map(|entry| entry.0)
    }

    pub(crate) fn get_range(&self, suspect: &Suspect) -> Option<&Range<u32>> {
        self.suspects
            .iter()
            .find(|entry| entry.0 == *suspect)
            .map(|entry| &entry.1)
    }
}

#[derive(Debug)]
pub(crate) enum Either<T, U> {
    Left(T),
    Right(U),
}

/// A single change between two blobs, or an unchanged region.
///
/// Line numbers refer to the file that is referred to as `after` or `NewOrDestination`, depending
/// on the context.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A range of tokens that wasn't changed.
    Unchanged(Range<u32>),
    /// `(added_line_range, num_deleted_in_before)`
    AddedOrReplaced(Range<u32>, u32),
    /// `(line_to_start_deletion_at, num_deleted_in_before)`
    Deleted(u32, u32),
}
