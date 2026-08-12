//! Port of git's `xdiff/xpatience.c`.
//!
//! The idea is to find lines that are unique in both files: intuitively those are the
//! ones a reader wants to see as common. The maximal ordered sequence of such line pairs
//! defines an initial set of common lines, which is then grown outwards over identical
//! neighbours. Between those common lines the algorithm recurses, and a region with no
//! unique line pairs left is handed to Myers.
//!
//! Two things carry over from upstream unchanged and are worth naming, because both
//! decide output rather than just performance:
//!
//! * A record's identity is its interned token, which is exactly what
//!   `xrecord_t::minimal_perfect_hash` is after `xdl_classify_record()`. `match()` in
//!   upstream compares those hashes, so token equality is the same test.
//! * Candidate entries are visited in order of first appearance in the *before* side —
//!   upstream's `map->first`/`next` chain, built as `insert_record()` walks file 1. The
//!   longest-increasing-subsequence pass depends on that order, so it is preserved here
//!   with a plain `Vec` rather than reproducing upstream's open-addressed table.
//!
//! `--anchored` (`xpp->anchors`) is the one upstream input with no equivalent here: this
//! crate has no way to pass anchor prefixes in, so `is_anchor()` is constantly false and
//! upstream's `anchor_i` stays -1. Every branch it guards is therefore dead and omitted.

use std::collections::hash_map::Entry as MapEntry;
use std::collections::HashMap;
use std::ops::Range;

use crate::intern::Token;
use crate::{Algorithm, Diff};

/// `xdl_do_patience_diff()`.
///
/// `removed`/`added` are `xdf1.changed`/`xdf2.changed`: one flag per token of the
/// (already trimmed) sequences, which the recursion below sets for every line that is
/// not part of a common sequence.
pub fn diff(before: &[Token], after: &[Token], removed: &mut [bool], added: &mut [bool]) {
    patience_diff(before, after, removed, added, 0..before.len(), 0..after.len());
}

/// Where the sole occurrence of a candidate line sits on the *after* side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line2 {
    /// `entry->line2 == 0`: the line was never seen on the after side.
    Unset,
    /// The single after-side line, 0-based. Upstream stores this 1-based.
    Unique(usize),
    /// `NON_UNIQUE`: seen more than once on one side or the other.
    NonUnique,
}

/// One `struct entry`, minus the list pointers the `Vec` order already encodes.
struct Entry {
    /// The sole (or first) before-side line, 0-based.
    line1: usize,
    line2: Line2,
}

/// `fill_hashmap()`: every before-side line in order of first appearance, each tagged
/// with the after-side line it uniquely pairs with.
///
/// The `bool` is upstream's `map->has_matches` — whether *any* before-side line occurs on
/// the after side at all, unique or not.
fn fill_hashmap(
    before: &[Token],
    after: &[Token],
    r1: &Range<usize>,
    r2: &Range<usize>,
) -> (Vec<Entry>, bool) {
    let mut order: Vec<Entry> = Vec::with_capacity(r1.len());
    let mut index: HashMap<Token, usize> = HashMap::with_capacity(r1.len());

    // Pass 1, over the before side: the first sighting of a token claims an entry, and
    // any repeat immediately disqualifies it.
    for line in r1.clone() {
        match index.entry(before[line]) {
            MapEntry::Occupied(slot) => order[*slot.get()].line2 = Line2::NonUnique,
            MapEntry::Vacant(slot) => {
                slot.insert(order.len());
                order.push(Entry { line1: line, line2: Line2::Unset });
            }
        }
    }

    // Pass 2, over the after side. Upstream never adds entries here — a token absent
    // from the before side is simply skipped — but any hit means the two sides share a
    // line, which is what decides whether the region is diffable at all.
    let mut has_matches = false;
    for line in r2.clone() {
        let Some(&slot) = index.get(&after[line]) else {
            continue;
        };
        has_matches = true;
        order[slot].line2 = match order[slot].line2 {
            Line2::Unset => Line2::Unique(line),
            // Already claimed, or already disqualified by pass 1: either way it is no
            // longer a unique pair.
            _ => Line2::NonUnique,
        };
    }
    (order, has_matches)
}

/// `binary_search()`: the index of the longest sequence whose last element still has a
/// smaller `line2`, or -1 when even the shortest does not.
fn binary_search(sequence: &[usize], order: &[Entry], line2: usize) -> isize {
    let (mut left, mut right) = (-1isize, sequence.len() as isize);
    while left + 1 < right {
        let middle = left + (right - left) / 2;
        // By construction no two entries can be equal.
        if line2_of(order, sequence[middle as usize]) > line2 {
            right = middle;
        } else {
            left = middle;
        }
    }
    left
}

/// The after-side line of a candidate already known to be unique.
fn line2_of(order: &[Entry], slot: usize) -> usize {
    match order[slot].line2 {
        Line2::Unique(l) => l,
        _ => unreachable!("only unique candidates enter the sequence"),
    }
}

/// `find_longest_common_sequence()`: the longest run of unique pairs whose order agrees
/// on both sides, as `(before, after)` line numbers ascending. `None` when there is none.
///
/// Upstream keeps one candidate per sequence *length* — the one with the smallest last
/// `line2` — and threads the winners together through `previous`; that is patience
/// sorting, and it is reproduced here with the same two arrays.
fn find_longest_common_sequence(order: &[Entry]) -> Option<Vec<(usize, usize)>> {
    // `sequence[k]` is the entry ending the best sequence of length `k + 1`.
    let mut sequence: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; order.len()];

    for (slot, entry) in order.iter().enumerate() {
        let Line2::Unique(line2) = entry.line2 else {
            continue;
        };
        let i = match sequence.last() {
            Some(&last) if line2 <= line2_of(order, last) => binary_search(&sequence, order, line2),
            // Extends the longest sequence found so far, so it appends.
            _ => sequence.len() as isize - 1,
        };
        previous[slot] = (i >= 0).then(|| sequence[i as usize]);
        let i = (i + 1) as usize;
        match i == sequence.len() {
            true => sequence.push(slot),
            false => sequence[i] = slot,
        }
    }

    // Walk back from the last element, which upstream does by rewriting `next`.
    let mut slot = Some(*sequence.last()?);
    let mut chain: Vec<(usize, usize)> = Vec::with_capacity(sequence.len());
    while let Some(s) = slot {
        chain.push((order[s].line1, line2_of(order, s)));
        slot = previous[s];
    }
    chain.reverse();
    Some(chain)
}

/// `walk_common_sequence()`: grow each anchor outwards over identical neighbours and
/// recurse into the gaps left between them.
fn walk_common_sequence(
    before: &[Token],
    after: &[Token],
    removed: &mut [bool],
    added: &mut [bool],
    chain: &[(usize, usize)],
    r1: Range<usize>,
    r2: Range<usize>,
) {
    let (end1, end2) = (r1.end, r2.end);
    let (mut line1, mut line2) = (r1.start, r2.start);
    let mut k = 0usize;

    loop {
        // Grow the anchor's range upwards over lines that already match, so the gap the
        // recursion sees is as small as possible. Past the last anchor the "gap" simply
        // runs to the end of the region.
        let (next1, next2) = match chain.get(k) {
            Some(&(a, b)) => {
                let (mut a, mut b) = (a, b);
                while a > line1 && b > line2 && before[a - 1] == after[b - 1] {
                    a -= 1;
                    b -= 1;
                }
                (a, b)
            }
            None => (end1, end2),
        };
        // And downwards from where the previous anchor left off.
        while line1 < next1 && line2 < next2 && before[line1] == after[line2] {
            line1 += 1;
            line2 += 1;
        }
        if next1 > line1 || next2 > line2 {
            patience_diff(before, after, removed, added, line1..next1, line2..next2);
        }
        if chain.get(k).is_none() {
            return;
        }
        // Consecutive anchors need no gap between them, so skip over the whole run.
        while k + 1 < chain.len()
            && chain[k + 1].0 == chain[k].0 + 1
            && chain[k + 1].1 == chain[k].1 + 1
        {
            k += 1;
        }
        (line1, line2) = (chain[k].0 + 1, chain[k].1 + 1);
        k += 1;
    }
}

/// `fall_back_to_classic_diff()`: a region with no unique pairs is Myers' problem.
///
/// Upstream clears the algorithm bits and re-enters `xdl_do_diff()`; here that is a
/// sub-`Diff` over the two slices whose flags are copied back, the same shape
/// `compact::fall_back_diff` already uses for the histogram fallback.
fn fall_back(
    before: &[Token],
    after: &[Token],
    removed: &mut [bool],
    added: &mut [bool],
    r1: Range<usize>,
    r2: Range<usize>,
) {
    let (sub_before, sub_after) = (&before[r1.clone()], &after[r2.clone()]);
    // Only the histogram algorithm reads this, and Myers is what runs here.
    let num_tokens = sub_before
        .iter()
        .chain(sub_after)
        .map(|t| u32::from(*t) + 1)
        .max()
        .unwrap_or(0);
    let mut sub = Diff::default();
    sub.compute_with(Algorithm::Myers, sub_before, sub_after, num_tokens);
    for (line, slot) in r1.zip(0u32..) {
        removed[line] = sub.is_removed(slot);
    }
    for (line, slot) in r2.zip(0u32..) {
        added[line] = sub.is_added(slot);
    }
}

/// `patience_diff()`: the recursion itself.
fn patience_diff(
    before: &[Token],
    after: &[Token],
    removed: &mut [bool],
    added: &mut [bool],
    r1: Range<usize>,
    r2: Range<usize>,
) {
    // Trivial case: one side is empty, so the other is wholly inserted or deleted.
    if r1.is_empty() {
        added[r2].fill(true);
        return;
    }
    if r2.is_empty() {
        removed[r1].fill(true);
        return;
    }

    let (order, has_matches) = fill_hashmap(before, after, &r1, &r2);
    // Not one line in common: there is nothing for either algorithm to anchor on.
    if !has_matches {
        removed[r1].fill(true);
        added[r2].fill(true);
        return;
    }

    match find_longest_common_sequence(&order) {
        Some(chain) => walk_common_sequence(before, after, removed, added, &chain, r1, r2),
        None => fall_back(before, after, removed, added, r1, r2),
    }
}
