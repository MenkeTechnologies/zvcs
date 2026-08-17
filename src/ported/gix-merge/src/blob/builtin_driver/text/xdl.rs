//! A port of git's three-way merge core, `xdiff/xmerge.c` as of git 2.55.0.
//!
//! The names below follow the C: [`Change`] is `xdchange_t` reduced to the four
//! fields `xdl_do_merge()` reads, [`Region`] is `xdmerge_t`, and every function
//! carries the name of the one it was ported from.
//!
//! The one structural difference is that the singly-linked list of `xdmerge_t`
//! is a `Vec<Region>`, so "append to the tail" is `Vec::last_mut()` and the
//! splice in [`refine_conflicts()`] is `Vec::insert()`.

use bstr::BStr;
use imara_diff::{Algorithm, Diff, InternedInput, Token};

use crate::blob::builtin_driver::text::{Canonicalize, ConflictStyle, Labels, Level};

/// `xdchange_t`, reduced to what `xdl_do_merge()` reads: the range of ancestor
/// lines one side replaced (`i1`/`chg1`), and the range of that side's own
/// lines it replaced them with (`i2`/`chg2`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Change {
    pub i1: i64,
    pub chg1: i64,
    pub i2: i64,
    pub chg2: i64,
}

/// `xdmerge_t`: one region of the output, already assigned to a side or marked
/// as conflicting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    /// `0` = conflict, `1` = take ours, `2` = take theirs, `3` = take both,
    /// `4` = both sides made the same change (written by [`refine_conflicts()`]).
    pub mode: i32,
    /// The region in the ancestor; only the diff3 styles read it.
    pub i0: i64,
    pub chg0: i64,
    /// The region in our postimage.
    pub i1: i64,
    pub chg1: i64,
    /// The region in their postimage.
    pub i2: i64,
    pub chg2: i64,
}

/// One `xdfenv_t`'s record array: the interned tokens that decide which records
/// *match*, and the raw lines that get *written*.
///
/// git keeps both in one `xrecord_t` — `ptr`/`size` are the bytes, and
/// `xdl_recmatch()` compares them under `xpp->flags`. Interning cannot express
/// that in one array once a whitespace flag is on, because equal records then
/// have unequal bytes, so the two live side by side here: `tokens[i]` answers
/// "does this record match that one", `lines[i]` is what `xdl_recs_copy()`
/// emits. With no whitespace flag the two agree line for line.
#[derive(Copy, Clone)]
pub struct Side<'a> {
    pub tokens: &'a [Token],
    pub lines: &'a [&'a [u8]],
}

impl<'a> Side<'a> {
    /// The bytes of the `i`th line, terminator included — `recs[i]->ptr`.
    fn line(&self, i: i64) -> &'a [u8] {
        self.lines[i as usize]
    }

    /// `xdf->nrec`.
    fn len(&self) -> i64 {
        self.tokens.len() as i64
    }
}

/// The three record arrays a merge runs over.
///
/// This is the pair of `xdfenv_t` the C passes around: `ancestor` is the
/// `xdf1` both sides share, `ours` is `xe1->xdf2` and `theirs` is `xe2->xdf2`.
#[derive(Copy, Clone)]
pub struct Files<'a> {
    pub ancestor: Side<'a>,
    pub ours: Side<'a>,
    pub theirs: Side<'a>,
}

/// Split `data` into lines, keeping each line's terminator, the way `xdl_prepare()`
/// records them.
pub fn tokens(data: &[u8]) -> imara_diff::sources::ByteLines<'_> {
    imara_diff::sources::byte_lines(data)
}

/// How `XDF_IGNORE_*` reaches an interner that can only compare byte-for-byte.
///
/// Each equivalence class is represented by the first line that fell into it, and
/// that representative — never the canonical form — is what gets interned. Two
/// lines therefore share a token exactly when `xdl_recmatch()` calls them equal:
/// same class means same representative, and two classes cannot share a
/// representative because a representative is a member of its own class. The raw
/// lines stay untouched, so `xdl_recs_copy()` still writes what git writes.
pub struct Classes<'data> {
    representatives: std::collections::HashMap<Vec<u8>, &'data [u8]>,
    canonicalize: Option<Canonicalize>,
}

impl<'data> Classes<'data> {
    pub fn new(canonicalize: Option<Canonicalize>) -> Self {
        Classes {
            representatives: std::collections::HashMap::new(),
            canonicalize,
        }
    }

    /// The line to intern in place of `line`.
    pub fn representative(&mut self, line: &'data [u8]) -> &'data [u8] {
        match self.canonicalize {
            // No whitespace flag: every line is its own class, as in the C's
            // leading `memcmp()` fast path.
            None => line,
            Some(canonicalize) => *self.representatives.entry(canonicalize(line)).or_insert(line),
        }
    }
}

/// Run `xdl_do_diff()` + `xdl_change_compact()` + `xdl_build_script()` over a
/// prepared `input`, yielding one [`Change`] per hunk.
///
/// `imara_diff`'s `postprocess_lines()` is its equivalent of the two
/// `xdl_change_compact()` passes git makes before building the script.
pub fn build_script(algorithm: Algorithm, input: &InternedInput<&[u8]>) -> Vec<Change> {
    let mut diff = Diff::compute(algorithm, input);
    diff.postprocess_lines(input);
    diff.hunks()
        .map(|hunk| Change {
            i1: i64::from(hunk.before.start),
            chg1: i64::from(hunk.before.end - hunk.before.start),
            i2: i64::from(hunk.after.start),
            chg2: i64::from(hunk.after.end - hunk.after.start),
        })
        .collect()
}

/// Port of `xdl_append_merge()`.
///
/// The fold in the `if` is what makes two changed regions with no unchanged
/// line between them one region rather than two: a new region that starts at or
/// before the end of the previous one *in either postimage* is absorbed into it,
/// and a mode mismatch downgrades the result to a conflict.
fn append_merge(regions: &mut Vec<Region>, mode: i32, i0: i64, chg0: i64, i1: i64, chg1: i64, i2: i64, chg2: i64) {
    if let Some(m) = regions.last_mut() {
        if i1 <= m.i1 + m.chg1 || i2 <= m.i2 + m.chg2 {
            if mode != m.mode {
                m.mode = 0;
            }
            m.chg0 = i0 + chg0 - m.i0;
            m.chg1 = i1 + chg1 - m.i1;
            m.chg2 = i2 + chg2 - m.i2;
            return;
        }
    }
    regions.push(Region {
        mode,
        i0,
        chg0,
        i1,
        chg1,
        i2,
        chg2,
    });
}

/// Port of `xdl_merge_cmp_lines()`: `true` when both sides' postimages agree on
/// all `line_count` lines.
fn merge_cmp_lines(files: &Files<'_>, i1: i64, i2: i64, line_count: i64) -> bool {
    (0..line_count).all(|k| {
        files.ours.tokens.get((i1 + k) as usize) == files.theirs.tokens.get((i2 + k) as usize)
    })
}

/// Port of `line_contains_alnum()` / `lines_contain_alnum()`.
fn lines_contain_alnum(files: &Files<'_>, i: i64, chg: i64) -> bool {
    (0..chg).any(|k| files.ours.line(i + k).iter().any(u8::is_ascii_alphanumeric))
}

/// Port of `xdl_refine_zdiff3_conflicts()`: drop lines common to both sides from
/// the front and the back of every conflict.
fn refine_zdiff3_conflicts(regions: &mut [Region], files: &Files<'_>) {
    for m in regions.iter_mut() {
        if m.mode != 0 {
            continue;
        }
        while m.chg1 != 0
            && m.chg2 != 0
            && files.ours.tokens[m.i1 as usize] == files.theirs.tokens[m.i2 as usize]
        {
            m.chg1 -= 1;
            m.chg2 -= 1;
            m.i1 += 1;
            m.i2 += 1;
        }
        while m.chg1 != 0
            && m.chg2 != 0
            && files.ours.tokens[(m.i1 + m.chg1 - 1) as usize]
                == files.theirs.tokens[(m.i2 + m.chg2 - 1) as usize]
        {
            m.chg1 -= 1;
            m.chg2 -= 1;
        }
    }
}

/// Port of `xdl_refine_conflicts()`: diff the two conflicting postimages against
/// each other and keep only the lines that really differ, splitting one region
/// into as many as that diff has hunks.
fn refine_conflicts(
    regions: &mut Vec<Region>,
    files: &Files<'_>,
    algorithm: Algorithm,
    canonicalize: Option<Canonicalize>,
) {
    let mut idx = 0;
    while idx < regions.len() {
        let m = regions[idx];
        // Only conflicts, and no sense refining one where a side is empty.
        if m.mode != 0 || m.chg1 == 0 || m.chg2 == 0 {
            idx += 1;
            continue;
        }

        // The C takes a sub-`mmfile_t` spanning the raw bytes of the conflicting
        // records; the same span is what concatenating those lines produces.
        let concat = |side: Side<'_>, start: i64, count: i64| -> Vec<u8> {
            let mut buf = Vec::new();
            for k in 0..count {
                buf.extend_from_slice(side.line(start + k));
            }
            buf
        };
        let t1 = concat(files.ours, m.i1, m.chg1);
        let t2 = concat(files.theirs, m.i2, m.chg2);

        let script = {
            // The C hands this sub-diff the same `xpp`, so the whitespace flags
            // apply here too.
            let mut classes = Classes::new(canonicalize);
            let mut sub = InternedInput::default();
            sub.update_before(tokens(&t1).map(|line| classes.representative(line)));
            sub.update_after(tokens(&t2).map(|line| classes.representative(line)));
            build_script(algorithm, &sub)
        };

        if script.is_empty() {
            // If this happens, the changes are identical.
            regions[idx].mode = 4;
            idx += 1;
            continue;
        }

        regions[idx].i1 = m.i1 + script[0].i1;
        regions[idx].chg1 = script[0].chg1;
        regions[idx].i2 = m.i2 + script[0].i2;
        regions[idx].chg2 = script[0].chg2;

        let mut at = idx + 1;
        for sub in &script[1..] {
            regions.insert(
                at,
                Region {
                    mode: 0,
                    // `xdl_refine_conflicts()` leaves `i0`/`chg0` of the split-off
                    // regions unset. They are only read by the diff3 styles, and
                    // those clamp the level below `Zealous`, so this never runs
                    // for them and the value is never observed.
                    i0: 0,
                    chg0: 0,
                    i1: m.i1 + sub.i1,
                    chg1: sub.chg1,
                    i2: m.i2 + sub.i2,
                    chg2: sub.chg2,
                },
            );
            at += 1;
        }
        idx = at;
    }
}

/// Port of `xdl_simplify_non_conflicts()`.
///
/// Fewer than four unchanged lines between two conflicts take up no fewer lines
/// once folded into them, so fold them, which also merges the two conflicts.
fn simplify_non_conflicts(regions: &mut Vec<Region>, files: &Files<'_>, simplify_if_no_alnum: bool) {
    let mut i = 0;
    while i + 1 < regions.len() {
        let begin = regions[i].i1 + regions[i].chg1;
        let end = regions[i + 1].i1;
        let keep_separate = regions[i].mode != 0
            || regions[i + 1].mode != 0
            || (end - begin > 3 && (!simplify_if_no_alnum || lines_contain_alnum(files, begin, end - begin)));
        if keep_separate {
            i += 1;
        } else {
            // `xdl_merge_two_conflicts()`: everything between the two hunks
            // becomes conflicting as well.
            let next = regions.remove(i + 1);
            regions[i].chg1 = next.i1 + next.chg1 - regions[i].i1;
            regions[i].chg2 = next.i2 + next.chg2 - regions[i].i2;
        }
    }
}

/// Port of `is_eol_crlf()`: `Some(true)` for a CRLF line ending, `Some(false)`
/// for LF-only, `None` when it cannot be determined (the C returns `-1`).
fn is_eol_crlf(side: Side<'_>, i: i64) -> Option<bool> {
    let nrec = side.len();
    let ends_in_crlf = |n: i64| {
        let line = side.line(n);
        line.len() > 1 && line[line.len() - 2] == b'\r'
    };
    if i < nrec - 1 {
        // All lines before the last *must* end in LF.
        return Some(ends_in_crlf(i));
    }
    if nrec == 0 || i >= nrec {
        // Cannot determine eol style from an empty file.
        return None;
    }
    let line = side.line(i);
    if line.last() == Some(&b'\n') {
        // Last line; ends in LF; is it CR/LF?
        return Some(ends_in_crlf(i));
    }
    if i == 0 {
        // The only line has no eol.
        return None;
    }
    // Determine eol from the second-to-last line.
    Some(ends_in_crlf(i - 1))
}

/// Port of `is_cr_needed()`.
///
/// The C treats the undetermined `-1` as true, so an undetermined side falls
/// through to the next check and only a definite LF-only stops the chain.
fn is_cr_needed(files: &Files<'_>, m: &Region) -> bool {
    // Match post-images' preceding, or first, lines' end-of-line style.
    let mut needs_cr = is_eol_crlf(files.ours, if m.i1 != 0 { m.i1 - 1 } else { 0 });
    if needs_cr != Some(false) {
        needs_cr = is_eol_crlf(files.theirs, if m.i2 != 0 { m.i2 - 1 } else { 0 });
    }
    // Look at pre-image's first line, unless we already settled on LF.
    if needs_cr != Some(false) {
        needs_cr = is_eol_crlf(files.ancestor, 0);
    }
    // If still undecided, use LF-only.
    needs_cr == Some(true)
}

/// Port of `xdl_recs_copy_0()`.
fn recs_copy(side: Side<'_>, i: i64, count: i64, needs_cr: bool, add_nl: bool, out: &mut Vec<u8>) {
    if count < 1 {
        return;
    }
    for k in 0..count {
        out.extend_from_slice(side.line(i + k));
    }
    if add_nl && side.line(i + count - 1).last() != Some(&b'\n') {
        if needs_cr {
            out.push(b'\r');
        }
        out.push(b'\n');
    }
}

/// Write one `<<<<<<<` / `|||||||` / `=======` / `>>>>>>>` line.
fn write_marker(out: &mut Vec<u8>, marker: u8, label: Option<&BStr>, marker_size: usize, needs_cr: bool) {
    out.extend(std::iter::repeat_n(marker, marker_size));
    if let Some(label) = label.filter(|label| !label.is_empty()) {
        out.push(b' ');
        out.extend_from_slice(label);
    }
    if needs_cr {
        out.push(b'\r');
    }
    out.push(b'\n');
}

/// Port of `fill_conflict_hunk()`.
fn fill_conflict_hunk(
    files: &Files<'_>,
    labels: Labels<'_>,
    i: i64,
    style: ConflictStyle,
    m: &Region,
    marker_size: usize,
    out: &mut Vec<u8>,
) {
    let needs_cr = is_cr_needed(files, m);

    // Before conflicting part.
    recs_copy(files.ours, i, m.i1 - i, false, false, out);
    write_marker(out, b'<', labels.current, marker_size, needs_cr);
    // Postimage from side #1.
    recs_copy(files.ours, m.i1, m.chg1, needs_cr, true, out);

    if matches!(style, ConflictStyle::Diff3 | ConflictStyle::ZealousDiff3) {
        // Shared preimage.
        write_marker(out, b'|', labels.ancestor, marker_size, needs_cr);
        recs_copy(files.ancestor, m.i0, m.chg0, needs_cr, true, out);
    }

    write_marker(out, b'=', None, marker_size, needs_cr);
    // Postimage from side #2.
    recs_copy(files.theirs, m.i2, m.chg2, needs_cr, true, out);
    write_marker(out, b'>', labels.other, marker_size, needs_cr);
}

/// Port of `xdl_fill_merge_buffer()`.
///
/// `favor` is `xmp.favor`: `0` keeps conflicts, `1`/`2`/`3` are
/// `XDL_MERGE_FAVOR_OURS`/`_THEIRS`/`_UNION`. As in the C it rewrites the mode
/// of every conflicting region in place, which is why the conflict count the
/// caller reports is taken before this runs.
#[allow(clippy::too_many_arguments)]
fn fill_merge_buffer(
    files: &Files<'_>,
    labels: Labels<'_>,
    favor: i32,
    regions: &mut [Region],
    style: ConflictStyle,
    marker_size: usize,
    out: &mut Vec<u8>,
) {
    let mut i: i64 = 0;
    for m in regions.iter_mut() {
        if favor != 0 && m.mode == 0 {
            m.mode = favor;
        }

        if m.mode == 0 {
            fill_conflict_hunk(files, labels, i, style, m, marker_size, out);
        } else if m.mode & 3 != 0 {
            // Before conflicting part.
            recs_copy(files.ours, i, m.i1 - i, false, false, out);
            // Postimage from side #1.
            if m.mode & 1 != 0 {
                let needs_cr = is_cr_needed(files, m);
                recs_copy(files.ours, m.i1, m.chg1, needs_cr, m.mode & 2 != 0, out);
            }
            // Postimage from side #2.
            if m.mode & 2 != 0 {
                recs_copy(files.theirs, m.i2, m.chg2, false, false, out);
            }
        } else {
            // Mode 4: both sides made the same change, so our postimage already
            // carries it and gets copied through by the next region's preamble.
            continue;
        }
        i = m.i1 + m.chg1;
    }
    recs_copy(files.ours, i, files.ours.len() - i, false, false, out);
}

/// Port of `xdl_do_merge()`, minus the output step.
///
/// Returns the regions with their final modes, before `favor` is applied.
fn do_merge(
    files: &Files<'_>,
    xscr1: &[Change],
    xscr2: &[Change],
    level: Level,
    style: ConflictStyle,
    algorithm: Algorithm,
    canonicalize: Option<Canonicalize>,
) -> Vec<Region> {
    // "diff3 -m" output does not make sense for anything more aggressive than
    // `Eager`, because the base is shown and does not match, so conflicts
    // cannot be refined against it. `ZealousDiff3` gets its own, weaker
    // refinement below instead.
    let mut level = level;
    if matches!(style, ConflictStyle::Diff3 | ConflictStyle::ZealousDiff3) && level > Level::Eager {
        level = Level::Eager;
    }

    let mut regions: Vec<Region> = Vec::new();
    let (mut a, mut b) = (0usize, 0usize);

    while a < xscr1.len() && b < xscr2.len() {
        let s1 = xscr1[a];
        let s2 = xscr2[b];

        // Note the strict `<`: two changes are independent only when at least
        // one unchanged ancestor line separates them.
        if s1.i1 + s1.chg1 < s2.i1 {
            append_merge(
                &mut regions,
                1,
                s1.i1,
                s1.chg1,
                s1.i2,
                s1.chg2,
                s2.i2 - s2.i1 + s1.i1,
                s1.chg1,
            );
            a += 1;
            continue;
        }
        if s2.i1 + s2.chg1 < s1.i1 {
            append_merge(
                &mut regions,
                2,
                s2.i1,
                s2.chg1,
                s1.i2 - s1.i1 + s2.i1,
                s2.chg1,
                s2.i2,
                s2.chg2,
            );
            b += 1;
            continue;
        }

        if level == Level::Minimal
            || s1.i1 != s2.i1
            || s1.chg1 != s2.chg1
            || s1.chg2 != s2.chg2
            || !merge_cmp_lines(files, s1.i2, s2.i2, s1.chg2)
        {
            // Conflict: widen both postimages so they cover the same ancestor span.
            let off = s1.i1 - s2.i1;
            let ffo = off + s1.chg1 - s2.chg1;

            let mut i0 = s1.i1;
            let mut i1 = s1.i2;
            let mut i2 = s2.i2;
            if off > 0 {
                i0 -= off;
                i1 -= off;
            } else {
                i2 += off;
            }
            let mut chg0 = s1.i1 + s1.chg1 - i0;
            let mut chg1 = s1.i2 + s1.chg2 - i1;
            let mut chg2 = s2.i2 + s2.chg2 - i2;
            if ffo < 0 {
                chg0 -= ffo;
                chg1 -= ffo;
            } else {
                chg2 += ffo;
            }
            append_merge(&mut regions, 0, i0, chg0, i1, chg1, i2, chg2);
        }

        let e1 = s1.i1 + s1.chg1;
        let e2 = s2.i1 + s2.chg1;
        if e1 >= e2 {
            b += 1;
        }
        if e2 >= e1 {
            a += 1;
        }
    }

    let ancestor_nrec = files.ancestor.len();
    while a < xscr1.len() {
        let s1 = xscr1[a];
        append_merge(
            &mut regions,
            1,
            s1.i1,
            s1.chg1,
            s1.i2,
            s1.chg2,
            s1.i1 + files.theirs.len() - ancestor_nrec,
            s1.chg1,
        );
        a += 1;
    }
    while b < xscr2.len() {
        let s2 = xscr2[b];
        append_merge(
            &mut regions,
            2,
            s2.i1,
            s2.chg1,
            s2.i1 + files.ours.len() - ancestor_nrec,
            s2.chg1,
            s2.i2,
            s2.chg2,
        );
        b += 1;
    }

    // Refine conflicts.
    if style == ConflictStyle::ZealousDiff3 {
        refine_zdiff3_conflicts(&mut regions, files);
    } else if level >= Level::Zealous {
        refine_conflicts(&mut regions, files, algorithm, canonicalize);
        simplify_non_conflicts(&mut regions, files, level > Level::Zealous);
    }

    regions
}

/// Everything the caller has to decide before `xdl_do_merge()` can run.
#[derive(Copy, Clone, Debug)]
pub struct Params<'a> {
    pub labels: Labels<'a>,
    /// `xmp.favor`: `0` keeps conflicts, `1`/`2`/`3` resolve them with
    /// ours/theirs/both.
    pub favor: i32,
    pub style: ConflictStyle,
    pub marker_size: usize,
    pub level: Level,
    pub algorithm: Algorithm,
    /// `xpp.flags & XDF_WHITESPACE_FLAGS`, as the canonicalization
    /// [`Classes`] groups records by. `None` is git's flagless `memcmp()`.
    pub canonicalize: Option<Canonicalize>,
}

/// How many regions conflicted, before and after an automatic resolution was
/// applied to them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Conflicts {
    /// What `xdl_merge()` returns, and what `git merge-file` reports as its exit
    /// code: `xdl_cleanup_merge()` counts the regions still marked as conflicts
    /// *after* `xdl_fill_merge_buffer()` has rewritten them with `favor`, so this
    /// is zero whenever a favor was given.
    pub reported: usize,
    /// The count before `favor` was applied, which is the only way to tell a
    /// clean merge from one that was resolved for us.
    pub before_favor: usize,
}

/// Port of `xdl_merge()`'s tail: merge the two change scripts and render the
/// result into `out`.
pub fn merge_scripts(
    files: &Files<'_>,
    xscr1: &[Change],
    xscr2: &[Change],
    params: Params<'_>,
    out: &mut Vec<u8>,
) -> Conflicts {
    let mut regions = do_merge(
        files,
        xscr1,
        xscr2,
        params.level,
        params.style,
        params.algorithm,
        params.canonicalize,
    );
    let before_favor = regions.iter().filter(|m| m.mode == 0).count();
    fill_merge_buffer(
        files,
        params.labels,
        params.favor,
        &mut regions,
        params.style,
        params.marker_size,
        out,
    );
    // `xdl_cleanup_merge()`, which runs after the buffer has been filled.
    Conflicts {
        reported: regions.iter().filter(|m| m.mode == 0).count(),
        before_favor,
    }
}
