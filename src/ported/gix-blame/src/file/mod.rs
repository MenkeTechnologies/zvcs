//! A module with low-level types and functions.

use std::{num::NonZeroU32, ops::Range};

use crate::{
    file::fingerprint::{Fingerprint, LineTracker, are_lines_adjacent, guess_line_blames, line_fingerprints},
    types::{BlameEntry, Change, Either, LineRange, Offset, PathTable, Suspect, UnblamedHunk},
};

pub(super) mod compact;
pub(super) mod fingerprint;
pub(super) mod copied;
pub(super) mod moved;
pub(super) mod function;

/// Compare a section from a potential *Source File* (`hunk`) with a change from a diff and see if
/// there is an intersection with `change`. Based on that intersection, we may generate a
/// [`BlameEntry`] for `out` and/or split the `hunk` into multiple.
///
/// This is the core of the blame implementation as it matches regions in *Blamed File* to
/// corresponding regions in one or more than one *Source File*.
fn process_change(
    new_hunks_to_blame: &mut Vec<UnblamedHunk>,
    offset: &mut Offset,
    suspect: Suspect,
    parent: Suspect,
    hunk: Option<UnblamedHunk>,
    change: Option<Change>,
) -> (Option<UnblamedHunk>, Option<Change>) {
    /// Since `range_with_end` is a range that is not inclusive at the end,
    /// `range_with_end.end` is not part of `range_with_end`.
    /// The first line that is `range_with_end.end - 1`.
    fn actual_end_in_range(test: &Range<u32>, containing_range: &Range<u32>) -> bool {
        (test.end - 1) >= containing_range.start && test.end <= containing_range.end
    }

    // # General Rules
    // 1. If there is no suspect, immediately reschedule `hunk` and redo processing of `change`.
    //
    // # Detailed Rules
    // 1. whenever we do *not* return `hunk`, it must be added to `new_hunks_to_blame`, shifted with `offset`
    // 2. return `hunk` if it is not fully covered by changes yet.
    // 3. `change` *must* be returned if it is not fully included in `hunk`.
    match (hunk, change) {
        (Some(hunk), Some(Change::Unchanged(unchanged))) => {
            let Some(range_in_suspect) = hunk.get_range(&suspect) else {
                // We don’t clone blame to `parent` as `suspect` has nothing to do with this
                // `hunk`.
                new_hunks_to_blame.push(hunk);
                return (None, Some(Change::Unchanged(unchanged)));
            };

            match (
                range_in_suspect.contains(&unchanged.start),
                actual_end_in_range(&unchanged, range_in_suspect),
            ) {
                (_, true) => {
                    //     <------>  (hunk)
                    // <------->     (unchanged)
                    //
                    // <---------->  (hunk)
                    //     <--->     (unchanged)

                    // skip over unchanged - there will be changes right after.
                    (Some(hunk), None)
                }
                (true, false) => {
                    // <-------->     (hunk)
                    //     <------->  (unchanged)

                    // Nothing to do with `hunk` except shifting it,
                    // but `unchanged` needs to be checked against the next hunk to catch up.
                    new_hunks_to_blame.push(hunk.passed_blame(suspect, parent).shift_by(parent, *offset));
                    (None, Some(Change::Unchanged(unchanged)))
                }
                (false, false) => {
                    // Any of the following cases are handled by this branch:
                    //    <--->      (hunk)
                    // <---------->  (unchanged)
                    //
                    //       <---->  (hunk)
                    // <-->          (unchanged)
                    //
                    // <-->          (hunk)
                    //       <---->  (unchanged)

                    if unchanged.end <= range_in_suspect.start {
                        //       <---->  (hunk)
                        // <-->          (unchanged)

                        // Let changes catch up with us.
                        (Some(hunk), None)
                    } else {
                        // <-->          (hunk)
                        //       <---->  (unchanged)
                        //
                        //    <--->      (hunk)
                        // <---------->  (unchanged)

                        // Nothing to do with `hunk` except shifting it,
                        // but `unchanged` needs to be checked against the next hunk to catch up.
                        new_hunks_to_blame.push(hunk.passed_blame(suspect, parent).shift_by(parent, *offset));
                        (None, Some(Change::Unchanged(unchanged)))
                    }
                }
            }
        }
        (Some(hunk), Some(Change::AddedOrReplaced(added, number_of_lines_deleted))) => {
            let Some(range_in_suspect) = hunk.get_range(&suspect).cloned() else {
                new_hunks_to_blame.push(hunk);
                return (None, Some(Change::AddedOrReplaced(added, number_of_lines_deleted)));
            };

            let suspect_contains_added_start = range_in_suspect.contains(&added.start);
            let suspect_contains_added_end = actual_end_in_range(&added, &range_in_suspect);
            match (suspect_contains_added_start, suspect_contains_added_end) {
                (true, true) => {
                    // A perfect match of lines to take out of the unblamed portion.
                    // <---------->  (hunk)
                    //     <--->     (added)
                    //     <--->     (blamed)
                    // <-->     <->  (new hunk)

                    // Split hunk at the start of added.
                    let hunk_starting_at_added = match hunk.split_at(suspect, added.start) {
                        Either::Left(hunk) => {
                            // `added` starts with `hunk`, nothing to split.
                            hunk
                        }
                        Either::Right((before, after)) => {
                            // requeue the left side `before` after offsetting it…
                            new_hunks_to_blame.push(before.passed_blame(suspect, parent).shift_by(parent, *offset));
                            // …and treat `after` as `new_hunk`, which contains the `added` range.
                            after
                        }
                    };

                    *offset += added.end - added.start;
                    *offset -= number_of_lines_deleted;

                    // The overlapping `added` section was successfully located.
                    // Re-split at the end of `added` to continue with what's after.
                    match hunk_starting_at_added.split_at(suspect, added.end) {
                        Either::Left(hunk) => {
                            new_hunks_to_blame.push(hunk);

                            // Nothing to split, so we are done with this hunk.
                            (None, None)
                        }
                        Either::Right((hunk, after)) => {
                            new_hunks_to_blame.push(hunk);

                            // Keep processing the unblamed range after `added`
                            (Some(after), None)
                        }
                    }
                }
                (true, false) => {
                    // Added overlaps towards the end of `hunk`.
                    // <-------->     (hunk)
                    //     <------->  (added)
                    //     <---->     (blamed)
                    // <-->           (new hunk)

                    let hunk_starting_at_added = match hunk.split_at(suspect, added.start) {
                        Either::Left(hunk) => hunk,
                        Either::Right((before, after)) => {
                            // Keep looking for the left side of the unblamed portion.
                            new_hunks_to_blame.push(before.passed_blame(suspect, parent).shift_by(parent, *offset));
                            after
                        }
                    };

                    // We can 'blame' the overlapping area of `added` and `hunk`.
                    new_hunks_to_blame.push(hunk_starting_at_added);
                    // Keep processing `added`, it's portion past `hunk` may still contribute.
                    (None, Some(Change::AddedOrReplaced(added, number_of_lines_deleted)))
                }
                (false, true) => {
                    // Added reaches into the hunk, so we blame only the overlapping portion of it.
                    //    <------->  (hunk)
                    // <------>      (added)
                    //    <--->      (blamed)
                    //         <-->  (new hunk)

                    *offset += added.end - added.start;
                    *offset -= number_of_lines_deleted;

                    match hunk.split_at(suspect, added.end) {
                        Either::Left(hunk) => {
                            new_hunks_to_blame.push(hunk);

                            (None, None)
                        }
                        Either::Right((before, after)) => {
                            new_hunks_to_blame.push(before);

                            (Some(after), None)
                        }
                    }
                }
                (false, false) => {
                    // Any of the following cases are handled by this branch:
                    //    <--->      (hunk)
                    // <---------->  (added)
                    //
                    //       <---->  (hunk)
                    // <-->          (added)
                    //
                    // <-->          (hunk)
                    //       <---->  (added)

                    if added.end <= range_in_suspect.start {
                        //       <---->  (hunk)
                        // <-->          (added)

                        *offset += added.end - added.start;
                        *offset -= number_of_lines_deleted;

                        // Let changes catchup with `hunk` after letting `added` contribute to the offset.
                        (Some(hunk), None)
                    } else if range_in_suspect.end <= added.start {
                        // <-->          (hunk)
                        //       <---->  (added)

                        // Retry `hunk` once there is overlapping changes to process.
                        new_hunks_to_blame.push(hunk.passed_blame(suspect, parent).shift_by(parent, *offset));

                        // Let hunks catchup with this change.
                        (
                            None,
                            Some(Change::AddedOrReplaced(added.clone(), number_of_lines_deleted)),
                        )
                    } else {
                        // Discard the left side of `added`, keep track of `blamed`, and continue with the
                        // right side of added that is going past `hunk`.
                        //    <--->      (hunk)
                        // <---------->  (added)
                        //    <--->      (blamed)

                        // Successfully blame the whole range.
                        new_hunks_to_blame.push(hunk);

                        // And keep processing `added` with future `hunks` that might be affected by it.
                        (
                            None,
                            Some(Change::AddedOrReplaced(added.clone(), number_of_lines_deleted)),
                        )
                    }
                }
            }
        }
        (Some(hunk), Some(Change::Deleted(line_number_in_destination, number_of_lines_deleted))) => {
            let Some(range_in_suspect) = hunk.get_range(&suspect) else {
                new_hunks_to_blame.push(hunk);
                return (
                    None,
                    Some(Change::Deleted(line_number_in_destination, number_of_lines_deleted)),
                );
            };

            if line_number_in_destination < range_in_suspect.start {
                //     <--->  (hunk)
                //  |         (line_number_in_destination)

                // Track the shift to `hunk` as it affects us, and keep catching up with changes.
                *offset -= number_of_lines_deleted;
                (Some(hunk), None)
            } else if line_number_in_destination < range_in_suspect.end {
                //  <----->  (hunk)
                //     |     (line_number_in_destination)

                let new_hunk = match hunk.split_at(suspect, line_number_in_destination) {
                    Either::Left(hunk) => {
                        // Nothing to split as `line_number_in_destination` is directly at start of `hunk`
                        hunk
                    }
                    Either::Right((before, after)) => {
                        // `before` isn't affected by deletion, so keep it for later.
                        new_hunks_to_blame.push(before.passed_blame(suspect, parent).shift_by(parent, *offset));
                        // after will be affected by offset, and we will see if there are more changes affecting it.
                        after
                    }
                };
                *offset -= number_of_lines_deleted;
                (Some(new_hunk), None)
            } else {
                //  <--->     (hunk)
                //         |  (line_number_in_destination)

                // Catchup with changes.
                new_hunks_to_blame.push(hunk.passed_blame(suspect, parent).shift_by(parent, *offset));

                (
                    None,
                    Some(Change::Deleted(line_number_in_destination, number_of_lines_deleted)),
                )
            }
        }
        (Some(hunk), None) => {
            // nothing to do - changes are exhausted, re-evaluate `hunk`.
            new_hunks_to_blame.push(hunk.passed_blame(suspect, parent).shift_by(parent, *offset));
            (None, None)
        }
        (None, Some(Change::Unchanged(_))) => {
            // Nothing changed past the blamed range - do nothing.
            (None, None)
        }
        (None, Some(Change::AddedOrReplaced(added, number_of_lines_deleted))) => {
            // Keep track of the shift to apply to hunks in the future.
            *offset += added.len() as u32;
            *offset -= number_of_lines_deleted;
            (None, None)
        }
        (None, Some(Change::Deleted(_, number_of_lines_deleted))) => {
            // Keep track of the shift to apply to hunks in the future.
            *offset -= number_of_lines_deleted;
            (None, None)
        }
        (None, None) => {
            // Noop, caller shouldn't do that, but not our problem.
            (None, None)
        }
    }
}

/// Consume `hunks_to_blame` and `changes` to pair up matches ranges (also overlapping) with each other.
/// Once a match is found, it's pushed onto `out`.
///
/// `process_changes` assumes that ranges coming from the same *Source File* can and do
/// occasionally overlap. If it were a desirable property of the blame algorithm as a whole to
/// never have two different lines from a *Blamed File* mapped to the same line in a *Source File*,
/// this property would need to be enforced at a higher level than `process_changes`.
/// Then the nested loops could potentially be flattened into one.
fn process_changes(
    hunks_to_blame: Vec<UnblamedHunk>,
    changes: Vec<Change>,
    suspect: Suspect,
    parent: Suspect,
) -> Vec<UnblamedHunk> {
    let mut new_hunks_to_blame = Vec::new();

    for mut hunk in hunks_to_blame.into_iter().map(Some) {
        let mut offset_in_destination = Offset::Added(0);

        let mut changes_iter = changes.iter().cloned();
        let mut change = changes_iter.next();

        loop {
            (hunk, change) = process_change(
                &mut new_hunks_to_blame,
                &mut offset_in_destination,
                suspect,
                parent,
                hunk,
                change,
            );

            change = change.or_else(|| changes_iter.next());

            if hunk.is_none() {
                break;
            }
        }
    }
    new_hunks_to_blame
}

/// One hunk of the blob diff, in the terms git's `blame_chunk()` uses: `tlno..same` are the changed
/// lines in the target, and the corresponding lines in the parent start at `parent_slno` and are
/// `parent_len` long.
struct DiffChunk {
    tlno: u32,
    same: u32,
    parent_slno: u32,
    parent_len: u32,
}

/// Recover the per-chunk coordinates git's xdiff callback receives from the [`Change`] list
/// [`blob_changes`](function) produced.
///
/// The list covers the whole target file in order, so walking it while tracking the position in the
/// parent reconstructs `start_a`/`count_a` for every chunk.
fn diff_chunks(changes: &[Change]) -> Vec<DiffChunk> {
    let mut chunks = Vec::new();
    let mut parent_pos = 0u32;
    for change in changes {
        match change {
            Change::Unchanged(range) => parent_pos += range.end - range.start,
            Change::AddedOrReplaced(added, num_deleted) => {
                chunks.push(DiffChunk {
                    tlno: added.start,
                    same: added.end,
                    parent_slno: parent_pos,
                    parent_len: *num_deleted,
                });
                parent_pos += num_deleted;
            }
            // A pure deletion has no lines in the target, so `same == tlno` and git's
            // `guess_line_blames()` is never called for it.
            Change::Deleted(_, num_deleted) => parent_pos += num_deleted,
        }
    }
    chunks
}

/// `ignore_blame_entry()`: decide which parts of `hunk` go to the parent and which stay with the
/// target, given the per-line answers in `line_blames` (indexed from `tlno`).
///
/// New hunks are carved off the front of `hunk`, each covering a contiguous run of lines from the
/// same origin. A run matched to the parent is passed to it at the matched line and marked
/// `ignored`; a run that matched nothing stays with the target and is marked `unblamable`.
fn ignore_blame_entry(
    hunk: UnblamedHunk,
    suspect: Suspect,
    parent: Suspect,
    tlno: u32,
    line_blames: &[LineTracker],
    out: &mut Vec<UnblamedHunk>,
) {
    let Some(range) = hunk.get_range(&suspect).cloned() else {
        out.push(hunk);
        return;
    };
    let base = (range.start - tlno) as usize;
    let nr_lines = (range.end - range.start) as usize;

    let mut remaining = hunk;
    let mut run_start = range.start;
    let mut i = 0usize;
    while i < nr_lines {
        // We are often adjacent to the next line - only split the hunk when we have to.
        let mut run = 1usize;
        while i + run < nr_lines && are_lines_adjacent(&line_blames[base + i + run - 1], &line_blames[base + i + run])
        {
            run += 1;
        }

        let (mut this, rest) = if i + run < nr_lines {
            match remaining.split_at(suspect, run_start + run as u32) {
                Either::Right((before, after)) => (before, Some(after)),
                // Unreachable for a split strictly inside the range, but degrading to "no split"
                // keeps the loop total rather than panicking.
                Either::Left(whole) => (whole, None),
            }
        } else {
            (remaining, None)
        };

        let tracker = line_blames[base + i];
        if tracker.is_parent {
            this.ignored = true;
            // `parent` carries the path the *Source File* has in the parent, so a hunk handed over
            // here is renamed across a rewrite exactly as the ordinary pass renames one.
            if let Some(entry) = this.suspects.iter_mut().find(|entry| entry.0 == suspect) {
                entry.0 = parent;
                entry.1 = tracker.s_lno..tracker.s_lno + run as u32;
            }
        } else {
            // `s_lno` is already in the target's address space, so the range stays as it is.
            this.unblamable = true;
        }
        out.push(this);

        match rest {
            Some(rest) => {
                remaining = rest;
                run_start += run as u32;
                i += run;
            }
            None => break,
        }
    }
}

/// The second `pass_blame_to_parent()` git runs for a commit on the `--ignore-rev` list: everything
/// the ordinary diff could not hand to `parent` is matched against the parent's lines by
/// similarity, and whatever finds a match is handed over after all.
///
/// `hunks_to_blame` is the result of the ordinary [`process_changes`] for the same `(suspect,
/// parent)` pair, so the hunks that still carry `suspect` are exactly the entries git's second pass
/// would find left in `origin->suspects`.
pub(super) fn process_ignored_changes(
    mut hunks_to_blame: Vec<UnblamedHunk>,
    changes: &[Change],
    suspect: Suspect,
    parent: Suspect,
    parent_blob: &[u8],
    target_blob: &[u8],
) -> Vec<UnblamedHunk> {
    if !hunks_to_blame.iter().any(|hunk| hunk.has_suspect(&suspect)) {
        return hunks_to_blame;
    }

    let mut fp_parent: Vec<Fingerprint> = line_fingerprints(parent_blob);
    let fp_target: Vec<Fingerprint> = line_fingerprints(target_blob);

    let mut out: Vec<UnblamedHunk> = Vec::with_capacity(hunks_to_blame.len());
    for chunk in diff_chunks(changes) {
        if chunk.same <= chunk.tlno {
            continue;
        }
        // git computes the guesses for every chunk, whether or not an entry falls into it, and
        // `guess_line_blames()` mutates the parent's fingerprints as it goes. Skipping a chunk here
        // would leave those fingerprints in a state the C code never has.
        let offset = i64::from(chunk.parent_slno) - i64::from(chunk.tlno);
        let line_blames = guess_line_blames(
            &mut fp_parent,
            &fp_target,
            chunk.tlno,
            offset as i32,
            chunk.same,
            chunk.parent_len,
        );

        let mut still_to_check = Vec::with_capacity(hunks_to_blame.len());
        for hunk in hunks_to_blame {
            match hunk.get_range(&suspect) {
                Some(range) if range.start >= chunk.tlno && range.end <= chunk.same => {
                    ignore_blame_entry(
                        hunk,
                        suspect,
                        parent,
                        chunk.tlno,
                        &line_blames,
                        &mut out,
                    );
                }
                _ => still_to_check.push(hunk),
            }
        }
        hunks_to_blame = still_to_check;
    }

    out.extend(hunks_to_blame);
    out
}

impl UnblamedHunk {
    fn shift_by(mut self, suspect: Suspect, offset: Offset) -> Self {
        if let Some(entry) = self.suspects.iter_mut().find(|entry| entry.0 == suspect) {
            entry.1 = entry.1.shift_by(offset);
        }
        self
    }

    fn split_at(self, suspect: Suspect, line_number_in_destination: u32) -> Either<Self, (Self, Self)> {
        match self.get_range(&suspect) {
            None => Either::Left(self),
            Some(range_in_suspect) => {
                if !range_in_suspect.contains(&line_number_in_destination) {
                    return Either::Left(self);
                }

                let split_at_from_start = line_number_in_destination - range_in_suspect.start;
                if split_at_from_start > 0 {
                    let new_suspects_before = self
                        .suspects
                        .iter()
                        .map(|(suspect, range)| (*suspect, range.start..(range.start + split_at_from_start)));

                    let new_suspects_after = self
                        .suspects
                        .iter()
                        .map(|(suspect, range)| (*suspect, (range.start + split_at_from_start)..range.end));

                    // git's `split_blame_at()` copies `ignored`/`unblamable` onto the new entry.
                    let new_hunk_before = Self {
                        range_in_blamed_file: self.range_in_blamed_file.start
                            ..(self.range_in_blamed_file.start + split_at_from_start),
                        suspects: new_suspects_before.collect(),
                        ignored: self.ignored,
                        unblamable: self.unblamable,
                    };
                    let new_hunk_after = Self {
                        range_in_blamed_file: (self.range_in_blamed_file.start + split_at_from_start)
                            ..(self.range_in_blamed_file.end),
                        suspects: new_suspects_after.collect(),
                        ignored: self.ignored,
                        unblamable: self.unblamable,
                    };

                    Either::Right((new_hunk_before, new_hunk_after))
                } else {
                    Either::Left(self)
                }
            }
        }
    }

    /// This is like [`Self::pass_blame()`], but easier to use in places where the 'passing' is
    /// done 'inline'.
    fn passed_blame(mut self, from: Suspect, to: Suspect) -> Self {
        if let Some(entry) = self.suspects.iter_mut().find(|entry| entry.0 == from) {
            entry.0 = to;
        }
        self
    }

    /// Transfer all ranges from the commit at `from` to the commit at `to`.
    fn pass_blame(&mut self, from: Suspect, to: Suspect) {
        if let Some(entry) = self.suspects.iter_mut().find(|entry| entry.0 == from) {
            entry.0 = to;
        }
    }

    fn clone_blame(&mut self, from: Suspect, to: Suspect) {
        if let Some(range_in_suspect) = self.get_range(&from) {
            self.suspects.push((to, range_in_suspect.clone()));
        }
    }

    fn remove_blame(&mut self, suspect: Suspect) {
        self.suspects.retain(|entry| entry.0 != suspect);
    }
}

impl BlameEntry {
    /// Create an offset from a portion of the *Blamed File*.
    fn from_unblamed_hunk(unblamed_hunk: &UnblamedHunk, suspect: Suspect, paths: &PathTable) -> Option<Self> {
        let range_in_source_file = unblamed_hunk.get_range(&suspect)?;

        Some(Self {
            start_in_blamed_file: unblamed_hunk.range_in_blamed_file.start,
            start_in_source_file: range_in_source_file.start,
            len: force_non_zero(range_in_source_file.len() as u32),
            commit_id: suspect.commit_id,
            source_file_name: paths.source_file_name(suspect.path),
            ignored: unblamed_hunk.ignored,
            unblamable: unblamed_hunk.unblamable,
        })
    }
}

fn force_non_zero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("BUG: hunks are never empty")
}

#[cfg(test)]
mod tests;
