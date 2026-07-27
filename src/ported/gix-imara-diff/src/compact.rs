//! Port of git's change compaction, `xdiff/xdiffi.c` 390-1000 and `xdl_fall_back_diff()`
//! (`xdiff/xutils.c` 453-479), git 2.55.0.
//!
//! [`Diff::compute`] produces the raw edit script; which of several equally minimal placements a
//! slider ends up in is decided afterwards. This module is the direct port of the decision git
//! makes, `xdl_change_compact()`, and it is what every consumer that has to agree with git
//! line-for-line uses — notably `gix-blame`, where the commit a line is attributed to *is* the
//! boundary the slider settled on.
//!
//! It differs from [`Diff::postprocess_lines`] in taking the indentation of the *original* lines
//! as an explicit argument rather than deriving it from the interned tokens: under `-w` the tokens
//! that are compared are whitespace-stripped, while git's `get_indent()` measures the unstripped
//! record (`xdf->recs[i]->ptr`).
//!
//! `git blame` always passes `XDF_INDENT_HEURISTIC` — `builtin/blame.c:1036` ORs it in from
//! `revs.diffopt.xdl_opts`, where `diff.indentHeuristic` puts it by default — so the heuristic is
//! not optional here.

use std::ops::Range;

use crate::intern::Token;
use crate::{Algorithm, Diff};

/// If a line is indented more than this, [`get_indent`] just returns this value.
const MAX_INDENT: i32 = 200;
/// How far to look for a non-blank line on either side of a split.
const MAX_BLANKS: i32 = 20;
/// How far a group is slid at most for the indent heuristic.
const INDENT_HEURISTIC_MAX_SLIDING: usize = 100;

/// Penalty if there are no non-blank lines before the split.
const START_OF_FILE_PENALTY: i32 = 1;
/// Penalty if there are no non-blank lines after the split.
const END_OF_FILE_PENALTY: i32 = 21;
/// Multiplier for the number of blank lines around the split.
const TOTAL_BLANK_WEIGHT: i32 = -30;
/// Multiplier for the number of blank lines after the split.
const POST_BLANK_WEIGHT: i32 = 6;
/// Penalty if the line is indented more than its predecessor.
const RELATIVE_INDENT_PENALTY: i32 = -4;
/// Penalty if the line is indented more than its predecessor, with blank lines around the split.
const RELATIVE_INDENT_WITH_BLANK_PENALTY: i32 = 10;
/// Penalty if the line is indented less than both its predecessor and its successor.
const RELATIVE_OUTDENT_PENALTY: i32 = 24;
/// Penalty for the outdent case, with blank lines around the split.
const RELATIVE_OUTDENT_WITH_BLANK_PENALTY: i32 = 17;
/// Penalty if the line is indented less than its predecessor but not less than its successor.
const RELATIVE_DEDENT_PENALTY: i32 = 23;
/// Penalty for the dedent case, with blank lines around the split.
const RELATIVE_DEDENT_WITH_BLANK_PENALTY: i32 = 17;
/// Weight of the effective-indent comparison against the accumulated penalties.
const INDENT_WEIGHT: i32 = 60;

/// `XDL_ISSPACE()`, `xdiff/xmacros.h:33` — C `isspace()` over the ASCII range.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `get_indent()`: the indentation of `line`, a tab counting to the next multiple of 8.
///
/// `-1` if the line is empty or contains only whitespace; clamped at [`MAX_INDENT`].
fn get_indent(line: &[u8]) -> i32 {
    let mut ret = 0;
    for &c in line {
        if !is_space(c) {
            return ret;
        } else if c == b' ' {
            ret += 1;
        } else if c == b'\t' {
            ret += 8 - ret % 8;
        }
        // Other whitespace characters are ignored.

        if ret >= MAX_INDENT {
            return MAX_INDENT;
        }
    }
    // The line contains only whitespace.
    -1
}

/// [`get_indent`] for every line of `data`, split the way the blame diff splits it.
///
/// Under `-w` the tokens that are *compared* are whitespace-stripped, but git measures the
/// indentation of the original record (`xdf->recs[i]->ptr`), so this is computed from the
/// unstripped blob.
pub fn line_indents(data: &[u8]) -> Vec<i32> {
    use crate::intern::TokenSource;
    crate::sources::byte_lines(data).tokenize().map(get_indent).collect()
}

/// `struct split_measurement`: what a hypothetical split above a line looks like.
struct SplitMeasurement {
    /// Is the split at the end of the file?
    end_of_file: bool,
    /// How much is the line following the split indented, or `-1` if it is blank?
    indent: i32,
    /// How many consecutive blank lines precede the split?
    pre_blank: i32,
    /// How much is the nearest non-blank line before the split indented, `-1` if there is none?
    pre_indent: i32,
    /// How many blank lines follow the line that follows the split?
    post_blank: i32,
    /// How much is the nearest non-blank line after those indented, `-1` if there is none?
    post_indent: i32,
}

/// `struct split_score`: smaller is preferred in both components.
#[derive(Default, Clone, Copy)]
struct SplitScore {
    effective_indent: i32,
    penalty: i32,
}

/// `measure_split()`: describe a hypothetical split of the file above line `split`.
fn measure_split(indents: &[i32], split: isize) -> SplitMeasurement {
    let nrec = indents.len() as isize;
    let (end_of_file, indent) = if split >= nrec {
        (true, -1)
    } else {
        (false, indents[split.max(0) as usize])
    };
    let mut m = SplitMeasurement {
        end_of_file,
        indent,
        pre_blank: 0,
        pre_indent: -1,
        post_blank: 0,
        post_indent: -1,
    };

    let mut i = split - 1;
    while i >= 0 {
        m.pre_indent = indents[i as usize];
        if m.pre_indent != -1 {
            break;
        }
        m.pre_blank += 1;
        if m.pre_blank == MAX_BLANKS {
            m.pre_indent = 0;
            break;
        }
        i -= 1;
    }

    let mut i = split + 1;
    while i < nrec {
        m.post_indent = indents[i.max(0) as usize];
        if m.post_indent != -1 {
            break;
        }
        m.post_blank += 1;
        if m.post_blank == MAX_BLANKS {
            m.post_indent = 0;
            break;
        }
        i += 1;
    }

    m
}

/// `score_add_split()`: add the badness of the split described by `m` to `s`.
fn score_add_split(m: &SplitMeasurement, s: &mut SplitScore) {
    if m.pre_indent == -1 && m.pre_blank == 0 {
        s.penalty += START_OF_FILE_PENALTY;
    }
    if m.end_of_file {
        s.penalty += END_OF_FILE_PENALTY;
    }

    // The number of blank lines following the split, including the line right after it.
    let post_blank = if m.indent == -1 { 1 + m.post_blank } else { 0 };
    let total_blank = m.pre_blank + post_blank;

    s.penalty += TOTAL_BLANK_WEIGHT * total_blank;
    s.penalty += POST_BLANK_WEIGHT * post_blank;

    let indent = if m.indent != -1 { m.indent } else { m.post_indent };
    let any_blanks = total_blank != 0;

    // Note that the effective indent is -1 at the end of the file.
    s.effective_indent += indent;

    if indent == -1 || m.pre_indent == -1 || indent == m.pre_indent {
        // No additional adjustments needed.
    } else if indent > m.pre_indent {
        s.penalty += if any_blanks {
            RELATIVE_INDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_INDENT_PENALTY
        };
    } else if m.post_indent != -1 && m.post_indent > indent {
        // Indented less than its predecessor but less than its successor: likely a block start.
        s.penalty += if any_blanks {
            RELATIVE_OUTDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_OUTDENT_PENALTY
        };
    } else {
        // That was probably the end of a block.
        s.penalty += if any_blanks {
            RELATIVE_DEDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_DEDENT_PENALTY
        };
    }
}

/// `score_cmp()`: negative if `s1` is the better split.
fn score_cmp(s1: &SplitScore, s2: &SplitScore) -> i32 {
    let cmp_indents = i32::from(s1.effective_indent > s2.effective_indent)
        - i32::from(s1.effective_indent < s2.effective_indent);
    INDENT_WEIGHT * cmp_indents + (s1.penalty - s2.penalty)
}

/// `struct xdlgroup`: a contiguous, possibly empty, group of changed lines.
#[derive(Clone, Copy, PartialEq)]
struct Group {
    /// The first changed line, or the line above which the empty group sits.
    start: usize,
    /// The first unchanged line after the group; equal to `start` for an empty group.
    end: usize,
}

/// `group_init()`.
fn group_init(changed: &[bool]) -> Group {
    let mut end = 0;
    while end < changed.len() && changed[end] {
        end += 1;
    }
    Group { start: 0, end }
}

/// `group_next()`: `false` if `g` is already at the end of the file.
fn group_next(changed: &[bool], g: &mut Group) -> bool {
    if g.end == changed.len() {
        return false;
    }
    g.start = g.end + 1;
    g.end = g.start;
    while g.end < changed.len() && changed[g.end] {
        g.end += 1;
    }
    true
}

/// `group_previous()`: `false` if `g` is already at the beginning of the file.
fn group_previous(changed: &[bool], g: &mut Group) -> bool {
    if g.start == 0 {
        return false;
    }
    g.end = g.start - 1;
    g.start = g.end;
    while g.start > 0 && changed[g.start - 1] {
        g.start -= 1;
    }
    true
}

/// `group_slide_down()`: slide `g` one line towards the end of the file, absorbing a group it bumps
/// into. `false` if it cannot be slid.
fn group_slide_down(changed: &mut [bool], recs: &[Token], g: &mut Group) -> bool {
    if g.end < recs.len() && recs[g.start] == recs[g.end] {
        changed[g.start] = false;
        g.start += 1;
        changed[g.end] = true;
        g.end += 1;
        while g.end < changed.len() && changed[g.end] {
            g.end += 1;
        }
        true
    } else {
        false
    }
}

/// `group_slide_up()`: slide `g` one line towards the beginning of the file, absorbing a group it
/// bumps into. `false` if it cannot be slid.
fn group_slide_up(changed: &mut [bool], recs: &[Token], g: &mut Group) -> bool {
    if g.start > 0 && recs[g.start - 1] == recs[g.end - 1] {
        g.start -= 1;
        changed[g.start] = true;
        g.end -= 1;
        changed[g.end] = false;
        while g.start > 0 && changed[g.start - 1] {
            g.start -= 1;
        }
        true
    } else {
        false
    }
}

/// The two sides of an edit script, as git's `xdfenv_t` holds them: one flag per line of each file.
pub struct Changed {
    /// One entry per line of the *before* file, `true` where the line was removed.
    pub removed: Vec<bool>,
    /// One entry per line of the *after* file, `true` where the line was added.
    pub added: Vec<bool>,
}

impl Changed {
    /// The hunks of the edit script as `(before, after)` line ranges, in `xdl_build_script()`
    /// order: the two flag arrays are walked in lockstep, and every run in which either side has a
    /// changed line forms one hunk.
    pub fn hunks(&self) -> Vec<(Range<u32>, Range<u32>)> {
        let mut out = Vec::new();
        let (mut before, mut after) = (0u32, 0u32);
        while (before as usize) < self.removed.len() || (after as usize) < self.added.len() {
            let (start_before, start_after) = (before, after);
            while (before as usize) < self.removed.len() && self.removed[before as usize] {
                before += 1;
            }
            while (after as usize) < self.added.len() && self.added[after as usize] {
                after += 1;
            }
            if before != start_before || after != start_after {
                out.push((start_before..before, start_after..after));
            }
            before += 1;
            after += 1;
        }
        out
    }
}

/// The inputs one `xdl_change_compact()` pass works on: the file it compacts, and the other one it
/// keeps in sync with.
struct Sides<'a> {
    /// `xe.xdf1.changed`.
    removed: &'a mut Vec<bool>,
    /// `xe.xdf2.changed`.
    added: &'a mut Vec<bool>,
    /// The interned lines of the *before* file.
    before: &'a [Token],
    /// The interned lines of the *after* file.
    after: &'a [Token],
}

/// Run git's `xdl_change_compact()` over the raw edit script in `diff`.
///
/// `before`/`after` are the interned lines the diff was computed from, `indent_before`/
/// `indent_after` the indentation of the corresponding original lines (see [`line_indents`]).
///
/// git compacts the *before* file first and the *after* file second (`xdiff/xdiffi.c:1098-1099`);
/// each pass reads and, on the histogram fall-back, writes the other file's flags, so the order is
/// part of the result.
pub fn change_compact(
    diff: &Diff,
    algorithm: Algorithm,
    before: &[Token],
    after: &[Token],
    indent_before: &[i32],
    indent_after: &[i32],
) -> Changed {
    let mut removed: Vec<bool> = (0..before.len() as u32).map(|i| diff.is_removed(i)).collect();
    let mut added: Vec<bool> = (0..after.len() as u32).map(|i| diff.is_added(i)).collect();

    compact_one(
        Sides {
            removed: &mut removed,
            added: &mut added,
            before,
            after,
        },
        Side::Before,
        indent_before,
        algorithm,
    );
    compact_one(
        Sides {
            removed: &mut removed,
            added: &mut added,
            before,
            after,
        },
        Side::After,
        indent_after,
        algorithm,
    );

    Changed { removed, added }
}

/// Which of the two files a `xdl_change_compact()` pass is compacting.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Before,
    After,
}

impl Sides<'_> {
    /// The flags of the file being compacted, and the lines it consists of.
    fn this(&mut self, side: Side) -> (&mut Vec<bool>, &[Token]) {
        match side {
            Side::Before => (self.removed, self.before),
            Side::After => (self.added, self.after),
        }
    }

    /// The flags of the other file.
    fn other(&self, side: Side) -> &[bool] {
        match side {
            Side::Before => self.added,
            Side::After => self.removed,
        }
    }
}

/// One `xdl_change_compact(xdf, xdfo, flags)` call: compact the groups of the file at `side` in
/// place, walking the other file alongside it.
fn compact_one(mut f: Sides<'_>, side: Side, indents: &[i32], algorithm: Algorithm) {
    let mut g = group_init(f.this(side).0);
    let mut go = group_init(f.other(side));

    loop {
        // If the group is empty in the to-be-compacted file, skip it.
        if g.end != g.start {
            let g_orig = g;
            let mut groupsize;
            let mut earliest_end;
            let mut end_matching_other;

            // Shift the change up and then down as far as possible in each direction, merging any
            // other change it bumps into, until doing so stops growing the group.
            loop {
                groupsize = g.end - g.start;
                end_matching_other = None;

                loop {
                    let (changed, recs) = f.this(side);
                    if !group_slide_up(changed, recs, &mut g) {
                        break;
                    }
                    let moved = group_previous(f.other(side), &mut go);
                    debug_assert!(moved, "BUG: group sync broken sliding up");
                }

                // This is the highest the group can be shifted; record its end index.
                earliest_end = g.end;

                if go.end > go.start {
                    end_matching_other = Some(g.end);
                }

                loop {
                    let (changed, recs) = f.this(side);
                    if !group_slide_down(changed, recs, &mut g) {
                        break;
                    }
                    let moved = group_next(f.other(side), &mut go);
                    debug_assert!(moved, "BUG: group sync broken sliding down");
                    if go.end > go.start {
                        end_matching_other = Some(g.end);
                    }
                }

                if groupsize == g.end - g.start {
                    break;
                }
            }

            // The group now sits as far down as it can go, so every heuristic below shifts it up.
            if g.end == earliest_end {
                // No shifting was possible.
            } else if end_matching_other.is_some() {
                // Line the group up with the last group of changes in the other file it can align
                // with, so that one change does not get split into an addition and a deletion.
                while go.end == go.start {
                    let (changed, recs) = f.this(side);
                    let ok = group_slide_up(changed, recs, &mut g);
                    debug_assert!(ok, "BUG: match disappeared");
                    let ok = group_previous(f.other(side), &mut go);
                    debug_assert!(ok, "BUG: group sync broken sliding to match");
                }
            } else {
                // A pure add/delete group implies two splits, one above it and one below it. Score
                // every position the group can be shifted to and take the least bad one.
                let mut best_shift = None;
                let mut best_score = SplitScore::default();

                let mut shift = earliest_end;
                if g.end > groupsize && g.end - groupsize - 1 > shift {
                    shift = g.end - groupsize - 1;
                }
                if g.end > INDENT_HEURISTIC_MAX_SLIDING && g.end - INDENT_HEURISTIC_MAX_SLIDING > shift {
                    shift = g.end - INDENT_HEURISTIC_MAX_SLIDING;
                }
                while shift <= g.end {
                    let mut score = SplitScore::default();
                    score_add_split(&measure_split(indents, shift as isize), &mut score);
                    score_add_split(
                        &measure_split(indents, shift as isize - groupsize as isize),
                        &mut score,
                    );
                    if best_shift.is_none() || score_cmp(&score, &best_score) <= 0 {
                        best_score = score;
                        best_shift = Some(shift);
                    }
                    shift += 1;
                }
                let best_shift = best_shift.expect("BUG: at least one shift is always scored");

                while g.end > best_shift {
                    let (changed, recs) = f.this(side);
                    let ok = group_slide_up(changed, recs, &mut g);
                    debug_assert!(ok, "BUG: best shift unreached");
                    let ok = group_previous(f.other(side), &mut go);
                    debug_assert!(ok, "BUG: group sync broken sliding to blank line");
                }
            }

            // Merging groups while shifting can leave matching lines inside the combined group that
            // the LCS never got to see. Only histogram diff can produce that: Myers already finds
            // minimal edits, so a shifted group cannot yield a smaller diff.
            if go.end != go.start && algorithm == Algorithm::Histogram && g != g_orig {
                let (this_range, other_range) = (g.start..g.end, go.start..go.end);
                let (range_before, range_after) = match side {
                    Side::Before => (this_range, other_range),
                    Side::After => (other_range, this_range),
                };
                fall_back_diff(&mut f, range_before, range_after);
            }
        }

        // Move past the just-processed group.
        if !group_next(f.this(side).0, &mut g) {
            break;
        }
        let moved = group_next(f.other(side), &mut go);
        debug_assert!(moved, "BUG: group sync broken moving to next group");
    }
}

/// `xdl_fall_back_diff()`: re-diff one group with the default algorithm so that lines the shifting
/// brought together are marked unchanged again. Both files' flags are overwritten in that range.
fn fall_back_diff(f: &mut Sides<'_>, range_before: Range<usize>, range_after: Range<usize>) {
    let (sub_before, sub_after) = (&f.before[range_before.clone()], &f.after[range_after.clone()]);
    // Only used by the histogram algorithm, which this never selects; git likewise clears the
    // algorithm bits before recursing (`xdiff/xdiffi.c:948`).
    let num_tokens = sub_before
        .iter()
        .chain(sub_after)
        .map(|t| u32::from(*t) + 1)
        .max()
        .unwrap_or(0);
    let mut sub = Diff::default();
    sub.compute_with(Algorithm::Myers, sub_before, sub_after, num_tokens);

    for (line, slot) in range_before.zip(0u32..) {
        f.removed[line] = sub.is_removed(slot);
    }
    for (line, slot) in range_after.zip(0u32..) {
        f.added[line] = sub.is_added(slot);
    }
}
