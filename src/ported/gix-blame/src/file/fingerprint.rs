//! Line fingerprints and the fuzzy line matcher that `git blame --ignore-rev` uses to guess where
//! the lines of an ignored commit came from.
//!
//! This is a port of `blame.c` from git 2.55.0, lines 359–1002: `struct fingerprint`,
//! `get_fingerprint`, `fingerprint_similarity`, `fingerprint_subtract`, `map_line_number`,
//! `get_similarity`, `find_best_line_matches`, `fuzzy_find_matching_lines_recurse` and
//! `fuzzy_find_matching_lines`.

use std::collections::HashMap;

/// `blame.c`: `#define CERTAIN_NOTHING_MATCHES -2`.
const CERTAIN_NOTHING_MATCHES: i32 = -2;
/// `blame.c`: `#define CERTAINTY_NOT_CALCULATED -1`.
const CERTAINTY_NOT_CALCULATED: i32 = -1;
/// `blame.c`: `#define FINGERPRINT_FILE_THRESHOLD 10`, the minimum similarity a whole-file scan
/// accepts.
const FINGERPRINT_FILE_THRESHOLD: i32 = 10;
/// `fuzzy_find_matching_lines`: how far from the linearly mapped line in the parent we look.
const MAX_SEARCH_DISTANCE_A: i32 = 10;

/// A fingerprint loosely represents a line, such that two fingerprints can be compared quickly to
/// give an indication of the similarity of the lines they represent.
///
/// It is the multiset of lower-cased byte pairs in the line, with whitespace added at each end,
/// whitespace normalized to `\0` and whitespace pairs dropped. `"Darth   Radar"` becomes
/// `{"\0d", "da", "da", "ar", "ar", "rt", "th", "h\0", "\0r", "ra", "ad", "r\0"}`. The multiset is
/// stored as a map from byte pair to its count.
#[derive(Clone, Default, Debug)]
pub(crate) struct Fingerprint {
    /// The byte pair `c0 | (c1 << 8)` mapped to how often it occurs.
    counts: HashMap<u32, i32>,
}

/// git's `isspace()` (`GIT_SPACE` in `ctype.c`), which is locale-independent.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

impl Fingerprint {
    /// `get_fingerprint()`: build the fingerprint of one line, `line` being the bytes from the
    /// start of the line up to (and including) its terminator.
    pub(crate) fn new(line: &[u8]) -> Self {
        let mut counts: HashMap<u32, i32> = HashMap::with_capacity(line.len() + 1);
        let mut c0: u32 = 0;
        // git iterates `p <= line_end`, so one extra step past the end contributes the trailing
        // whitespace pair that terminates the string.
        for i in 0..=line.len() {
            let c1: u32 = match line.get(i) {
                Some(&b) if !is_space(b) => u32::from(b.to_ascii_lowercase()),
                // Past the end, or whitespace: normalized to 0.
                _ => 0,
            };
            let hash = c0 | (c1 << 8);
            c0 = c1;
            // Ignore whitespace pairs.
            if hash == 0 {
                continue;
            }
            *counts.entry(hash).or_insert(0) += 1;
        }
        Fingerprint { counts }
    }

    /// `fingerprint_similarity()`: the size of the intersection of the two multisets, counting
    /// repeated elements. The similarity of `"cat mat"` and `"father rather"` is 2 because `"at"`
    /// occurs twice in both, while `"tim"` and `"mit"` have similarity 0.
    fn similarity(&self, other: &Fingerprint) -> i32 {
        let mut intersection = 0;
        for (pair, count_b) in &other.counts {
            if let Some(count_a) = self.counts.get(pair) {
                intersection += (*count_a).min(*count_b);
            }
        }
        intersection
    }

    /// `fingerprint_subtract()`: subtract the byte pairs of `other` from `self`, in place.
    fn subtract(&mut self, other: &Fingerprint) {
        for (pair, count_b) in &other.counts {
            if let Some(count_a) = self.counts.get_mut(pair) {
                if *count_a <= *count_b {
                    self.counts.remove(pair);
                } else {
                    *count_a -= *count_b;
                }
            }
        }
    }
}

/// `get_line_fingerprints()` over a whole blob, splitting it into lines the same way the blame
/// diff does (`find_line_starts()` in git).
pub(crate) fn line_fingerprints(data: &[u8]) -> Vec<Fingerprint> {
    data.split_inclusive(|b| *b == b'\n').map(Fingerprint::new).collect()
}

/// `struct line_number_mapping`: linearly maps a line number in one half of a diff chunk onto the
/// half of the chunk that is closest in terms of its position as a fraction of the chunk length.
struct LineNumberMapping {
    destination_start: i32,
    destination_length: i32,
    source_start: i32,
    source_length: i32,
}

/// `map_line_number()`. The arithmetic is widened to 64 bits; C's `int` version overflows on very
/// large chunks, which is undefined behaviour rather than a behaviour worth reproducing.
fn map_line_number(line_number: i32, mapping: &LineNumberMapping) -> i32 {
    let numerator = (i64::from(line_number - mapping.source_start) * 2 + 1) * i64::from(mapping.destination_length);
    (numerator / (i64::from(mapping.source_length) * 2)) as i32 + mapping.destination_start
}

/// The mutable state of one `fuzzy_find_matching_lines()` run.
///
/// The C original threads raw pointers into `similarities`, `certainties`, `result` and
/// `second_best_result` through the recursion, re-basing them on each call. Here the arrays stay
/// whole and are indexed by the line's offset from the *top-level* `start_b`, which is the same
/// element the re-based pointer would have addressed.
struct Fuzzy<'a> {
    /// Fingerprints of every line of the parent, indexed absolutely. Mutated by `subtract`.
    fp_a: &'a mut [Fingerprint],
    /// Fingerprints of every line of the target, indexed absolutely.
    fp_b: &'a [Fingerprint],
    /// Similarity of a line in B with the nearby lines in A, `-1` when not yet calculated. See
    /// `get_similarity()` in git for the layout.
    similarities: Vec<i32>,
    /// How strongly a line in B is matched with some line in A.
    certainties: Vec<i32>,
    /// Absolute index in A of the second-closest match of a line in B.
    second_best_result: Vec<i32>,
    /// Absolute index in A of the closest match of a line in B.
    result: Vec<i32>,
    max_search_distance_a: i32,
    max_search_distance_b: i32,
    /// `max_search_distance_a * 2 + 1`, the width of one row of `similarities`.
    row_len: usize,
    /// The `start_b` of the top-level call, which all array indices are relative to.
    top_start_b: i32,
    mapping: LineNumberMapping,
}

impl Fuzzy<'_> {
    /// `get_similarity()`: the slot holding the similarity of the line `delta` lines away from the
    /// closest line in A, for the line in B at `row` (relative to the top-level `start_b`).
    fn similarity_index(&self, row: i32, delta: i32) -> usize {
        debug_assert!(delta.abs() <= self.max_search_distance_a);
        row as usize * self.row_len + (delta + self.max_search_distance_a) as usize
    }

    /// `find_best_line_matches()`: calculate this line's similarities with the nearby lines in A if
    /// not already done, then record the most similar and second most similar lines along with the
    /// resulting certainty.
    fn find_best_line_matches(&mut self, start_a: i32, length_a: i32, start_b: i32, local_line_b: i32) {
        let row = start_b - self.top_start_b + local_line_b;
        // Certainty has already been calculated so no need to redo the work.
        if self.certainties[row as usize] != CERTAINTY_NOT_CALCULATED {
            return;
        }

        let closest_local_line_a = map_line_number(local_line_b + start_b, &self.mapping) - start_a;
        let search_start = (closest_local_line_a - self.max_search_distance_a).max(0);
        let search_end = (closest_local_line_a + self.max_search_distance_a + 1).min(length_a);

        let (mut best_similarity, mut second_best_similarity) = (0, 0);
        let (mut best_similarity_index, mut second_best_similarity_index) = (0, 0);

        for i in search_start..search_end {
            let index = self.similarity_index(row, i - closest_local_line_a);
            if self.similarities[index] == -1 {
                // Scale the similarity by (1000 - distance from the closest line) to act as a tie
                // break between lines that are otherwise equally similar.
                self.similarities[index] = self.fp_b[(start_b + local_line_b) as usize]
                    .similarity(&self.fp_a[(start_a + i) as usize])
                    * (1000 - (i - closest_local_line_a).abs());
            }
            let similarity = self.similarities[index];
            if similarity > best_similarity {
                second_best_similarity = best_similarity;
                second_best_similarity_index = best_similarity_index;
                best_similarity = similarity;
                best_similarity_index = i;
            } else if similarity > second_best_similarity {
                second_best_similarity = similarity;
                second_best_similarity_index = i;
            }
        }

        if best_similarity == 0 {
            // This line definitely doesn't match anything. Mark it with this special value so it
            // doesn't get invalidated and won't be recalculated.
            self.certainties[row as usize] = CERTAIN_NOTHING_MATCHES;
            self.result[row as usize] = -1;
        } else {
            // Matching well with two lines reduces the certainty, but a line matching very well
            // with two lines should still be prioritised over one matching poorly with one line,
            // hence doubling `best_similarity`.
            self.certainties[row as usize] = best_similarity * 2 - second_best_similarity;
            self.result[row as usize] = start_a + best_similarity_index;
            self.second_best_result[row as usize] = start_a + second_best_similarity_index;
        }
    }

    /// `fuzzy_find_matching_lines_recurse()`: find the line that can be matched with the most
    /// confidence, use it as a partition, and recurse on the lines to either side of it. This
    /// avoids lines appearing out of order and retains a sensible line ordering.
    fn recurse(&mut self, start_a: i32, start_b: i32, length_a: i32, length_b: i32) {
        let mut most_certain_local_line_b = -1;
        let mut most_certain_line_certainty = -1;
        for i in 0..length_b {
            self.find_best_line_matches(start_a, length_a, start_b, i);
            let row = (start_b - self.top_start_b + i) as usize;
            if self.certainties[row] > most_certain_line_certainty {
                most_certain_line_certainty = self.certainties[row];
                most_certain_local_line_b = i;
            }
        }

        // No matches.
        if most_certain_local_line_b == -1 {
            return;
        }

        let most_certain_row = (start_b - self.top_start_b + most_certain_local_line_b) as usize;
        let most_certain_line_a = self.result[most_certain_row];

        // Subtract the most certain line's fingerprint in B from the matched fingerprint in A, so
        // other lines in B can't also match the same parts of the line in A.
        let subtrahend = self.fp_b[(start_b + most_certain_local_line_b) as usize].clone();
        self.fp_a[most_certain_line_a as usize].subtract(&subtrahend);

        // Invalidate results that may be affected by the choice of the most certain line.
        let invalidate_min = (most_certain_local_line_b - self.max_search_distance_b).max(0);
        let invalidate_max = (most_certain_local_line_b + self.max_search_distance_b + 1).min(length_b);

        // As the fingerprint in A has changed, discard previously calculated similarity values
        // with that fingerprint.
        for i in invalidate_min..invalidate_max {
            let closest_local_line_a = map_line_number(i + start_b, &self.mapping) - start_a;
            let delta = most_certain_line_a - start_a - closest_local_line_a;
            // Check that the lines in A and B are close enough that there is a similarity value
            // for them.
            if delta.abs() > self.max_search_distance_a {
                continue;
            }
            let index = self.similarity_index(start_b - self.top_start_b + i, delta);
            self.similarities[index] = -1;
        }

        // Discard the matches for lines in B that are currently matched with a line in A such that
        // their ordering contradicts the ordering imposed by the choice of the most certain line.
        for i in (invalidate_min..most_certain_local_line_b).rev() {
            let row = (start_b - self.top_start_b + i) as usize;
            if self.certainties[row] >= 0
                && (self.result[row] >= most_certain_line_a || self.second_best_result[row] >= most_certain_line_a)
            {
                self.certainties[row] = CERTAINTY_NOT_CALCULATED;
            }
        }
        for i in (most_certain_local_line_b + 1)..invalidate_max {
            let row = (start_b - self.top_start_b + i) as usize;
            if self.certainties[row] >= 0
                && (self.result[row] <= most_certain_line_a || self.second_best_result[row] <= most_certain_line_a)
            {
                self.certainties[row] = CERTAINTY_NOT_CALCULATED;
            }
        }

        // Repeat the matching process for lines before the most certain line.
        if most_certain_local_line_b > 0 {
            self.recurse(
                start_a,
                start_b,
                most_certain_line_a + 1 - start_a,
                most_certain_local_line_b,
            );
        }
        // Repeat the matching process for lines after the most certain line.
        if most_certain_local_line_b + 1 < length_b {
            let second_half_start_a = most_certain_line_a;
            let second_half_start_b = start_b + most_certain_local_line_b + 1;
            self.recurse(
                second_half_start_a,
                second_half_start_b,
                length_a + start_a - second_half_start_a,
                length_b + start_b - second_half_start_b,
            );
        }
    }
}

/// `fuzzy_find_matching_lines()`: find the lines in the parent range `parent_slno..parent_slno +
/// parent_len` that most closely match the target lines `tlno..same`, choosing the best matches
/// that preserve the line ordering.
///
/// Returns one absolute parent line index per target line, or `-1` where nothing matched, and
/// `None` when the parent range is empty. `fp_a` is mutated exactly as git mutates the parent
/// origin's fingerprints, which is observable by the whole-file fallback scan that follows.
fn fuzzy_find_matching_lines(
    fp_a: &mut [Fingerprint],
    fp_b: &[Fingerprint],
    tlno: i32,
    parent_slno: i32,
    same: i32,
    parent_len: i32,
) -> Option<Vec<i32>> {
    // "A" is the left hand side of the diff (the parent), "B" the right hand side (the target).
    let (start_a, length_a) = (parent_slno, parent_len);
    let (start_b, length_b) = (tlno, same - tlno);

    if length_a <= 0 {
        return None;
    }

    // Given a line in B, compare it to the line in A closest to its position and to the lines in A
    // no more than `max_search_distance_a` away from that one. `max_search_distance_b` is an upper
    // bound on the distance between lines in B that are compared with the same line in A.
    let mut max_search_distance_a = MAX_SEARCH_DISTANCE_A;
    if max_search_distance_a >= length_a {
        max_search_distance_a = length_a - 1;
    }
    let max_search_distance_b = ((2 * max_search_distance_a + 1) * length_b - 1) / length_a;

    let row_len = (max_search_distance_a * 2 + 1) as usize;
    let mut fuzzy = Fuzzy {
        fp_a,
        fp_b,
        similarities: vec![-1; length_b as usize * row_len],
        certainties: vec![CERTAINTY_NOT_CALCULATED; length_b as usize],
        second_best_result: vec![-1; length_b as usize],
        result: vec![-1; length_b as usize],
        max_search_distance_a,
        max_search_distance_b,
        row_len,
        top_start_b: start_b,
        mapping: LineNumberMapping {
            destination_start: start_a,
            destination_length: length_a,
            source_start: start_b,
            source_length: length_b,
        },
    };

    fuzzy.recurse(start_a, start_b, length_a, length_b);

    Some(fuzzy.result)
}

/// `scan_parent_range()`: the line in `from..from + nr_lines` of the parent most similar to the
/// target line `t_idx`, or `-1` if none reaches the threshold. Ties are broken by proximity to the
/// target line number.
fn scan_parent_range(fp_a: &[Fingerprint], fp_b: &[Fingerprint], t_idx: i32, from: i32, nr_lines: i32) -> i32 {
    let mut best_sim_val = FINGERPRINT_FILE_THRESHOLD;
    let mut best_sim_idx = -1;

    for p_idx in from..from + nr_lines {
        let sim = fp_b[t_idx as usize].similarity(&fp_a[p_idx as usize]);
        if sim < best_sim_val {
            continue;
        }
        // Break ties with the closest-to-target line number.
        if sim == best_sim_val && best_sim_idx != -1 && (best_sim_idx - t_idx).abs() < (p_idx - t_idx).abs() {
            continue;
        }
        best_sim_val = sim;
        best_sim_idx = p_idx;
    }
    best_sim_idx
}

/// Where one line of the target came from, as decided by [`guess_line_blames`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineTracker {
    /// Whether the line was matched to a line in the parent.
    pub is_parent: bool,
    /// The line index — in the parent when `is_parent`, otherwise in the target.
    pub s_lno: u32,
}

/// `guess_line_blames()`: decide, for every line of the diff chunk `tlno..same`, whether it can be
/// attributed to a line of the parent.
///
/// The first pass checks the chunk against the parent's diff chunk. If that fails for a line, the
/// second pass tries to match that line against any part of the parent file, which catches changes
/// that were broken into two chunks by context.
///
/// `offset` is git's chunk offset, i.e. the parent chunk starts at `tlno + offset`.
pub(crate) fn guess_line_blames(
    fp_parent: &mut [Fingerprint],
    fp_target: &[Fingerprint],
    tlno: u32,
    offset: i32,
    same: u32,
    parent_len: u32,
) -> Vec<LineTracker> {
    let tlno = tlno as i32;
    let same = same as i32;
    let parent_slno = tlno + offset;
    let parent_num_lines = fp_parent.len() as i32;

    let fuzzy_matches = fuzzy_find_matching_lines(fp_parent, fp_target, tlno, parent_slno, same, parent_len as i32);

    let mut line_blames = Vec::with_capacity((same - tlno) as usize);
    for i in 0..(same - tlno) {
        let target_idx = tlno + i;
        let best_idx = match fuzzy_matches.as_ref().map(|m| m[i as usize]) {
            Some(idx) if idx >= 0 => idx,
            _ => scan_parent_range(fp_parent, fp_target, target_idx, 0, parent_num_lines),
        };
        line_blames.push(if best_idx >= 0 {
            LineTracker {
                is_parent: true,
                s_lno: best_idx as u32,
            }
        } else {
            LineTracker {
                is_parent: false,
                s_lno: target_idx as u32,
            }
        });
    }
    line_blames
}

/// `are_lines_adjacent()`: two trackers describe a contiguous run from the same origin.
pub(crate) fn are_lines_adjacent(first: &LineTracker, second: &LineTracker) -> bool {
    first.is_parent == second.is_parent && first.s_lno + 1 == second.s_lno
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two examples given in the `fingerprint_similarity()` comment in `blame.c`.
    #[test]
    fn similarity_matches_gits_documented_examples() {
        let cat_mat = Fingerprint::new(b"cat mat");
        let father_rather = Fingerprint::new(b"father rather");
        assert_eq!(cat_mat.similarity(&father_rather), 2);

        let tim = Fingerprint::new(b"tim");
        let mit = Fingerprint::new(b"mit");
        assert_eq!(tim.similarity(&mit), 0);
    }

    /// The multiset spelled out in the `struct fingerprint` comment for `"Darth   Radar"`.
    #[test]
    fn fingerprint_is_the_documented_multiset() {
        let fp = Fingerprint::new(b"Darth   Radar");
        let pair = |a: u8, b: u8| u32::from(a) | (u32::from(b) << 8);
        let expected = [
            (pair(0, b'd'), 1),
            (pair(b'd', b'a'), 2),
            (pair(b'a', b'r'), 2),
            (pair(b'r', b't'), 1),
            (pair(b't', b'h'), 1),
            (pair(b'h', 0), 1),
            (pair(0, b'r'), 1),
            (pair(b'r', b'a'), 1),
            (pair(b'a', b'd'), 1),
            (pair(b'r', 0), 1),
        ];
        assert_eq!(fp.counts.len(), expected.len());
        for (key, count) in expected {
            assert_eq!(fp.counts.get(&key), Some(&count), "byte pair {key:#06x}");
        }
    }

    /// Whitespace-only differences and case must not change a fingerprint, which is what lets the
    /// matcher see a re-indented line as the same line.
    #[test]
    fn whitespace_and_case_are_normalized() {
        assert_eq!(
            Fingerprint::new(b"    let x = 1;\n").similarity(&Fingerprint::new(b"let X = 1;\n")),
            Fingerprint::new(b"let x = 1;\n").similarity(&Fingerprint::new(b"let x = 1;\n"))
        );
    }

    /// A reformatted block must map back onto the original lines in order, which is the whole point
    /// of the recursion in `fuzzy_find_matching_lines_recurse`.
    #[test]
    fn fuzzy_matching_preserves_order_across_a_reindent() {
        let parent: Vec<&[u8]> = vec![b"fn one() {\n", b"    alpha();\n", b"    beta();\n", b"}\n"];
        let target: Vec<&[u8]> = vec![b"fn one() {\n", b"\talpha();\n", b"\tbeta();\n", b"}\n"];
        let mut fp_a: Vec<_> = parent.iter().map(|l| Fingerprint::new(l)).collect();
        let fp_b: Vec<_> = target.iter().map(|l| Fingerprint::new(l)).collect();

        let trackers = guess_line_blames(&mut fp_a, &fp_b, 1, 0, 3, 2);
        assert_eq!(
            trackers,
            vec![
                LineTracker {
                    is_parent: true,
                    s_lno: 1
                },
                LineTracker {
                    is_parent: true,
                    s_lno: 2
                }
            ]
        );
    }

    /// A line with no counterpart anywhere in the parent stays with the target and is reported as
    /// unblamable rather than being forced onto an unrelated parent line.
    #[test]
    fn a_line_without_a_counterpart_stays_with_the_target() {
        let parent: Vec<&[u8]> = vec![b"alpha alpha alpha\n"];
        let target: Vec<&[u8]> = vec![b"zzzzzzzzzzzzzzzzz\n"];
        let mut fp_a: Vec<_> = parent.iter().map(|l| Fingerprint::new(l)).collect();
        let fp_b: Vec<_> = target.iter().map(|l| Fingerprint::new(l)).collect();

        let trackers = guess_line_blames(&mut fp_a, &fp_b, 0, 0, 1, 1);
        assert_eq!(
            trackers,
            vec![LineTracker {
                is_parent: false,
                s_lno: 0
            }]
        );
    }
}
