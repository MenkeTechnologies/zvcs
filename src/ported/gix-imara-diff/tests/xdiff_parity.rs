//! Differential regressions against git 2.55.0's `xdiff/`, for the three places where the
//! algorithm the caller asked for changes what the surrounding machinery is allowed to do.
//!
//! Every expectation in here was measured from `/opt/homebrew/bin/git` 2.55.0 first — the
//! hunk bodies of `git diff -U3 --diff-algorithm=<algo>` over a one-file repository, with
//! the `diff --git`/`index`/`---`/`+++` header lines and the `@@`-trailing function context
//! dropped, which is exactly what [`Diff::unified_diff`] renders. The inputs are one
//! character per line, so `get_indent()` is `0` everywhere and the indent heuristic cannot
//! be what separates the algorithms.

use gix_imara_diff::{compact, Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};

/// One line per character, the shape all fixtures below are written in.
fn lines(chars: &str) -> String {
    chars.chars().map(|c| format!("{c}\n")).collect()
}

fn render(diff: &Diff, input: &InternedInput<&str>) -> String {
    diff.unified_diff(
        &BasicLineDiffPrinter(&input.interner),
        UnifiedDiffConfig::default(),
        input,
    )
    .to_string()
}

/// The diff the porcelain produces: the edit script, then git's `xdl_change_compact()`.
fn compacted(algorithm: Algorithm, input: &InternedInput<&str>) -> String {
    let mut diff = Diff::compute(algorithm, input);
    // `get_indent()` of a one-character line is 0 on both sides.
    let (before, after) = (vec![0i32; input.before.len()], vec![0i32; input.after.len()]);
    diff.compact_with(algorithm, &input.before, &input.after, Some((&before, &after)));
    render(&diff, input)
}

/// The same edit script through the slide-only postprocessing instead.
fn slid(algorithm: Algorithm, input: &InternedInput<&str>) -> String {
    let mut diff = Diff::compute(algorithm, input);
    diff.postprocess_lines(input);
    render(&diff, input)
}

/// `xdl_trim_ends()` is reached only through `xdl_optimize_ctxs()`, which
/// `xdl_prepare_env()` skips for patience and histogram (`xdiff/xprepare.c:460-462`).
///
/// Both files here share the prefix `cbb` and the suffix `bbc`, so there is something to
/// trim, and both algorithms decide what to anchor on by counting occurrences inside the
/// range they are handed — patience the lines unique in both files, histogram the least
/// frequent line. Trimming removes occurrences from those counts, so a trimmed histogram
/// anchors somewhere git never does, which is what the differing expectation below pins.
#[test]
fn patience_and_histogram_see_the_untrimmed_range() {
    let before = lines("cbbaaccbccccbbbc");
    let after = lines("cbbabcccccabaaabbbc");
    let input = InternedInput::new(before.as_str(), after.as_str());

    let myers = "\
@@ -2,14 +2,17 @@
 b
 b
 a
-a
-c
-c
 b
 c
 c
 c
 c
+c
+a
+b
+a
+a
+a
 b
 b
 b
";
    let histogram = "\
@@ -2,14 +2,17 @@
 b
 b
 a
+b
+c
+c
+c
+c
+c
 a
-c
-c
 b
-c
-c
-c
-c
+a
+a
+a
 b
 b
 b
";

    assert_eq!(slid(Algorithm::Myers, &input), myers);
    assert_eq!(slid(Algorithm::MyersMinimal, &input), myers);
    assert_eq!(slid(Algorithm::Patience, &input), myers);
    assert_eq!(compacted(Algorithm::Histogram, &input), histogram);
    // The fixture is only evidence because stock separates the two.
    assert_ne!(myers, histogram);
}

/// `xdl_change_compact()` re-diffs a group that grew while being shifted, for histogram
/// only (`xdiff/xdiffi.c:940-958`). The slide-only postprocessing has no such step, so
/// every input here is one where the two disagree and only the re-diffing one is git.
#[test]
fn histogram_re_diffs_groups_that_merged_while_shifting() {
    // (before, after, git's hunk body)
    let cases = [
        (
            "aaaaa",
            "bbabbaa",
            "\
@@ -1,5 +1,7 @@
+b
+b
 a
-a
-a
+b
+b
 a
 a
",
        ),
        (
            "aaaababbb",
            "bbbbbb",
            "\
@@ -1,9 +1,6 @@
-a
-a
-a
-a
 b
-a
+b
+b
 b
 b
 b
",
        ),
        (
            "caacbb",
            "bbbbcbabbbb",
            "\
@@ -1,6 +1,11 @@
+b
+b
+b
+b
 c
+b
 a
-a
-c
+b
+b
 b
 b
",
        ),
    ];

    for (before, after, expected) in cases {
        let (before, after) = (lines(before), lines(after));
        let input = InternedInput::new(before.as_str(), after.as_str());
        assert_eq!(compacted(Algorithm::Histogram, &input), expected);
        // Without the re-diff the same edit script lands somewhere else, which is what
        // makes this input evidence rather than decoration.
        assert_ne!(slid(Algorithm::Histogram, &input), expected);
    }
}

/// `try_lcs()` advances to the next occurrence the region it just measured does not cover
/// with `while (np <= ae)` (`xdiff/xhistogram.c:207-217`). `np` and `ae` are both indices
/// into the *before* file, so the bound is the before-side end of the region and never the
/// after-side one; the two drift apart by however far the region's halves sit from each
/// other, which is exactly what this fixture makes visible.
#[test]
fn histogram_skips_covered_occurrences_by_the_before_side_index() {
    let before = lines("ccbbbbaacabcbaab");
    let after = lines("cbcacbcabbabaab");
    let input = InternedInput::new(before.as_str(), after.as_str());

    let myers = "\
@@ -1,15 +1,14 @@
 c
-c
-b
-b
 b
-b
-a
-a
 c
 a
+c
 b
 c
+a
+b
+b
+a
 b
 a
 a
";
    let histogram = "\
@@ -1,15 +1,14 @@
 c
-c
-b
-b
-b
 b
+c
 a
-a
+c
+b
 c
 a
 b
-c
+b
+a
 b
 a
 a
";

    assert_eq!(slid(Algorithm::Myers, &input), myers);
    assert_eq!(compacted(Algorithm::Histogram, &input), histogram);
    assert_ne!(myers, histogram);
}

/// `index.max_chain_length` is 64 and `index.cnt` starts at 65 (`xdiff/xhistogram.c:284`
/// and `:289`), so a line occurring 64 times can still anchor an LCS and one occurring 65
/// times cannot — at 65 `index.cnt` never drops below `max_chain_length`, `find_lcs()`
/// reports failure and the region goes to Myers.
///
/// The fixture makes that boundary visible: `r` occurs `n` times and `c` occurs `2n`, so
/// `r` is the only candidate, and anchoring on it produces a different (equally sized)
/// edit script than Myers does.
#[test]
fn the_histogram_anchor_limit_is_gits_64() {
    let fixture = |n: usize| {
        let before = "c\n".repeat(n) + &"r\n".repeat(n) + &"c\n".repeat(n);
        let after = "r\n".repeat(n) + &"c\n".repeat(2 * n);
        (before, after)
    };
    let anchors = |n: usize| {
        let (before, after) = fixture(n);
        let input = InternedInput::new(before.as_str(), after.as_str());
        render(&Diff::compute(Algorithm::Histogram, &input), &input)
            != render(&Diff::compute(Algorithm::Myers, &input), &input)
    };

    assert!(anchors(64), "a line occurring 64 times must still anchor");
    assert!(!anchors(65), "a line occurring 65 times must fall back to Myers");
}

/// A line far past the limit never anchors either, however far past it is.
///
/// This does *not* discriminate between counting occurrences exactly, the way `rec->cnt`
/// does (`xdiff/xhistogram.c:47` caps it at `MAX_CNT`, `UINT_MAX`), and reporting the
/// length of an occurrence list that stops growing at the limit: with the list capped at
/// `MAX_CHAIN_LEN + 1` the two are indistinguishable, because every threshold they are
/// compared against is at most that same value. No fixture found in this session separates
/// them; the exact count is kept because it is what the C does, not because it is
/// observable here.
#[test]
fn a_line_far_past_the_limit_never_anchors() {
    let fixture = |n: usize| {
        let before = "c\n".repeat(n) + &"r\n".repeat(n) + &"c\n".repeat(n);
        let after = "r\n".repeat(n) + &"c\n".repeat(2 * n);
        (before, after)
    };
    for n in [65, 100, 400, 1000] {
        let (before, after) = fixture(n);
        let input = InternedInput::new(before.as_str(), after.as_str());
        let histogram = render(&Diff::compute(Algorithm::Histogram, &input), &input);
        let myers = render(&Diff::compute(Algorithm::Myers, &input), &input);
        assert_eq!(histogram, myers, "a line occurring {n} times must not anchor");
    }
}

/// `get_indent()` (`xdiff/xdiffi.c`), which `xdl_change_compact()` scores splits with.
#[test]
fn get_indent_measures_tabs_to_the_next_multiple_of_eight() {
    assert_eq!(compact::get_indent(b"x"), 0);
    assert_eq!(compact::get_indent(b"   x"), 3);
    assert_eq!(compact::get_indent(b"\tx"), 8);
    assert_eq!(compact::get_indent(b" \tx"), 8);
    assert_eq!(compact::get_indent(b"\t x"), 9);
    // An empty or all-whitespace line is blank.
    assert_eq!(compact::get_indent(b""), -1);
    assert_eq!(compact::get_indent(b"  \t"), -1);
}
