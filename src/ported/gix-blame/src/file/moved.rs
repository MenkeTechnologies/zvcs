//! Port of git's `-M` line-movement detection, `blame.c` 1991-2200 (git 2.55.0).
//!
//! After the ordinary diff has handed every parent what it can, `pass_blame()` optionally offers
//! the leftovers to the same parents once more — but this time each leftover entry is diffed
//! against the parent's *whole* blob rather than against its own position in it, so lines that were
//! moved around inside the file are found where they went.

use std::ops::Range;

use gix_hash::ObjectId;

use gix_diff::blob::compact;

use super::{
    function::OriginFiles,
    function::{strip_whitespace_per_line, tokens_for_diffing},
};
use crate::{
    Error, Statistics,
    types::{Suspect, UnblamedHunk},
};

/// One parent's contribution to `sg_origin[]` in git's `pass_blame()`: the origin — the parent
/// commit and the path the blamed file lives at in it — plus the blob it has there.
pub(super) type ParentOrigin = (Suspect, ObjectId);

/// The byte offset of every line of the *Blamed File*, plus a final entry for its length, so that
/// the bytes of lines `a..b` are `blob[starts[a]..starts[b]]` — git's `sb->lineno`.
pub(super) fn line_starts(blob: &[u8]) -> Vec<usize> {
    use gix_diff::blob::TokenSource;
    let mut starts = Vec::new();
    let mut at = 0;
    for line in tokens_for_diffing(blob).tokenize() {
        starts.push(at);
        at += line.len();
    }
    starts.push(blob.len());
    starts
}

/// `blame_entry_score()`: how trivial the lines `range` of the *Blamed File* are.
///
/// A line like `\t}` occurs everywhere in any ordinary program, and it is not worth saying it was
/// moved from somewhere unrelated, so entries below the `-M` score are never offered to a parent.
pub(super) fn blame_entry_score(blob: &[u8], starts: &[usize], range: &Range<u32>) -> u32 {
    let (from, to) = (starts[range.start as usize], starts[range.end as usize]);
    let mut score = 1;
    for &byte in &blob[from..to] {
        if byte.is_ascii_alphanumeric() {
            score += 1;
        }
    }
    score
}

/// `split_overlap()`: how one entry is cut into the part before the moved chunk, the moved chunk
/// itself, and the part after it.
#[derive(Clone, Copy)]
pub(super) struct Split {
    /// Lines at the front of the entry that stay with the current suspect.
    pre: u32,
    /// Lines that the parent is to be blamed for; never zero for a split that exists at all.
    mid: u32,
    /// The line in the parent's blob the moved chunk was found at.
    parent_start: u32,
}

/// `handle_split()`: lines `tlno..same` of the entry match the parent starting at `plno`.
///
/// All line numbers are relative to the entry, as they are in git before `ent->s_lno` is added.
fn handle_split(num_lines: u32, tlno: u32, plno: u32, same: u32) -> Option<Split> {
    if num_lines <= tlno || tlno >= same {
        return None;
    }
    let chunk_end = same.min(num_lines);
    if chunk_end <= tlno {
        return None;
    }
    Some(Split {
        pre: tlno,
        mid: chunk_end - tlno,
        parent_start: plno,
    })
}

/// `copy_split_if_better()`: keep whichever split hands the parent the least trivial chunk.
///
/// git compares with `<`, so a later candidate of equal score replaces the earlier one.
pub(super) fn copy_split_if_better(best: &mut Option<(Split, u32)>, candidate: Split, score: u32) {
    match best {
        Some((_, best_score)) if score < *best_score => {}
        _ => *best = Some((candidate, score)),
    }
}

/// `find_copy_in_blob()`: diff the entry's lines, taken from the *Blamed File*, against the whole
/// parent blob and return the best chunk the parent can be blamed for.
#[expect(clippy::too_many_arguments)]
pub(super) fn find_copy_in_blob(
    hunk: &UnblamedHunk,
    parent_blob: &[u8],
    blamed_blob: &[u8],
    starts: &[usize],
    diff_algorithm: gix_diff::blob::Algorithm,
    ignore_whitespace: bool,
    stats: &mut Statistics,
) -> Option<(Split, u32)> {
    let range_in_blamed = hunk.range_in_blamed_file.clone();
    let num_lines = range_in_blamed.end - range_in_blamed.start;
    // git prepares an `mmfile_t` that spans exactly the entry's lines of the final image.
    let target = &blamed_blob[starts[range_in_blamed.start as usize]..starts[range_in_blamed.end as usize]];

    let (parent_norm, target_norm);
    let input = if ignore_whitespace {
        parent_norm = strip_whitespace_per_line(parent_blob);
        target_norm = strip_whitespace_per_line(target);
        gix_diff::blob::InternedInput::new(parent_norm.as_slice(), target_norm.as_slice())
    } else {
        gix_diff::blob::InternedInput::new(parent_blob, target)
    };
    let diff = gix_diff::blob::Diff::compute(diff_algorithm, &input);
    let compacted = compact::change_compact(
        &diff,
        diff_algorithm,
        &input.before,
        &input.after,
        &compact::line_indents(parent_blob),
        &compact::line_indents(target),
    );
    stats.blobs_diffed += 1;

    // Between two change hunks everything matches, which is what `handle_split_cb()` reports.
    let mut best = None;
    let mut consider = |tlno: u32, plno: u32, same: u32| {
        if let Some(split) = handle_split(num_lines, tlno, plno, same) {
            let moved = range_in_blamed.start + split.pre..range_in_blamed.start + split.pre + split.mid;
            let score = blame_entry_score(blamed_blob, starts, &moved);
            copy_split_if_better(&mut best, split, score);
        }
    };
    let (mut plno, mut tlno) = (0u32, 0u32);
    for (parent_hunk, target_hunk) in compacted.hunks() {
        consider(tlno, plno, target_hunk.start);
        plno = parent_hunk.end;
        tlno = target_hunk.end;
    }
    // The remainder, if any, all matches the parent.
    consider(tlno, plno, num_lines);

    best
}

/// `split_blame()`: hand the middle part of `split` to `parent` and requeue the rest.
///
/// The parts that stay behind go to `unblamed` to be offered to the parent again on the next round,
/// exactly as git requeues `split[0]` and `split[2]`.
pub(super) fn split_blame(
    hunk: UnblamedHunk,
    suspect: Suspect,
    parent: Suspect,
    split: Split,
    blamed: &mut Vec<UnblamedHunk>,
    unblamed: &mut Vec<UnblamedHunk>,
) {
    let start_in_suspect = match hunk.get_range(&suspect) {
        Some(range) => range.start,
        None => {
            unblamed.push(hunk);
            return;
        }
    };

    let rest = match hunk.split_at(suspect, start_in_suspect + split.pre) {
        crate::types::Either::Right((before, after)) => {
            unblamed.push(before);
            after
        }
        crate::types::Either::Left(whole) => whole,
    };
    let mut middle = match rest.split_at(suspect, start_in_suspect + split.pre + split.mid) {
        crate::types::Either::Right((middle, after)) => {
            unblamed.push(after);
            middle
        }
        crate::types::Either::Left(whole) => whole,
    };

    // `parent` already names the path the parent keeps the file at, so the *Source File* of the
    // moved chunk follows from the origin it is handed to.
    if let Some(entry) = middle.suspects.iter_mut().find(|entry| entry.0 == suspect) {
        entry.0 = parent;
        entry.1 = split.parent_start..split.parent_start + split.mid;
    }
    blamed.push(middle);
}

/// `filter_small()`: move every entry whose score is at most `score_min` out of `source`.
pub(super) fn filter_small(
    blamed_blob: &[u8],
    starts: &[usize],
    small: &mut Vec<UnblamedHunk>,
    source: Vec<UnblamedHunk>,
    score_min: u32,
) -> Vec<UnblamedHunk> {
    let mut kept = Vec::with_capacity(source.len());
    for hunk in source {
        if blame_entry_score(blamed_blob, starts, &hunk.range_in_blamed_file) <= score_min {
            small.push(hunk);
        } else {
            kept.push(hunk);
        }
    }
    kept
}

/// The `PICKAXE_BLAME_MOVE` block of git's `pass_blame()`: offer everything `suspect` is still
/// suspected for to each parent once more, matched anywhere in the parent's blob.
#[expect(clippy::too_many_arguments)]
pub(super) fn find_moves_in_parents(
    hunks_to_blame: Vec<UnblamedHunk>,
    suspect: Suspect,
    parents: &[ParentOrigin],
    move_score: u32,
    blamed_blob: &[u8],
    starts: &[usize],
    odb: &impl gix_object::Find,
    origin_files: &mut OriginFiles,
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

    let mut toosmall = Vec::new();
    unblamed = filter_small(blamed_blob, starts, &mut toosmall, unblamed, move_score);

    for (parent_origin, parent_blob_id) in parents {
        if unblamed.is_empty() {
            break;
        }
        // `fill_origin_blob()` again (`blame.c:2173`), on the same origin the ordinary diff
        // already used, so this normally costs nothing beyond the lookup.
        let parent_blob = origin_files.fill(*parent_origin, parent_blob_id.as_ref(), odb, stats)?;

        // Whatever a round hands to the parent leaves behind up to two smaller entries, and those
        // are offered to the same parent again until a round finds nothing.
        let mut leftover = Vec::new();
        while !unblamed.is_empty() {
            let mut requeued = Vec::new();
            for hunk in std::mem::take(&mut unblamed) {
                let best = find_copy_in_blob(
                    &hunk,
                    &parent_blob,
                    blamed_blob,
                    starts,
                    diff_algorithm,
                    ignore_whitespace,
                    stats,
                );
                match best {
                    Some((split, score)) if move_score < score => {
                        split_blame(hunk, suspect, *parent_origin, split, &mut out, &mut requeued)
                    }
                    _ => leftover.push(hunk),
                }
            }
            unblamed = filter_small(blamed_blob, starts, &mut toosmall, requeued, move_score);
        }
        unblamed = leftover;
    }

    out.append(&mut toosmall);
    out.append(&mut unblamed);
    Ok(out)
}
