use crate::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};
use expect_test::expect;

#[test]
fn myers_is_even() {
    let before = "a\nb\nx\nx\ny\n";
    let after = "b\na\nx\ny\nx\n";

    cov_mark::check!(EVEN_SPLIT);
    // if the check for is_odd incorrectly always true then we take a fastpath
    // when we shouldn't, which always leads to infinite iterations/recursion
    // still we check the number of iterations here in case the search
    // is buggy in more subtle ways
    cov_mark::check_count!(SPLIT_SEARCH_ITER, 15);
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Myers, &input);
    expect![[r#"
        @@ -1,5 +1,5 @@
        -a
         b
        -x
        +a
         x
         y
        +x
    "#]]
    .assert_eq(
        &diff
            .unified_diff(
                &BasicLineDiffPrinter(&input.interner),
                UnifiedDiffConfig::default(),
                &input,
            )
            .to_string(),
    );
}

#[test]
fn myers_is_odd() {
    let before = "a\nb\nx\ny\nx\n";
    let after = "b\na\nx\ny\n";

    cov_mark::check!(ODD_SPLIT);
    // if the check for odd doesn't work then
    // we still find the correct result but the number of search
    // iterations increases
    cov_mark::check_count!(SPLIT_SEARCH_ITER, 9);
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Myers, &input);
    expect![[r#"
        @@ -1,5 +1,4 @@
        -a
         b
        +a
         x
         y
        -x
    "#]]
    .assert_eq(
        &diff
            .unified_diff(
                &BasicLineDiffPrinter(&input.interner),
                UnifiedDiffConfig::default(),
                &input,
            )
            .to_string(),
    );
}

/// `Algorithm::Patience` must anchor on the lines that are unique in *both* sides, which
/// is the one thing that distinguishes it from `Histogram`'s approximation of the same
/// idea. This input is a case where the two genuinely disagree, so a `Patience` that
/// silently delegated to `Histogram` (or to `Myers`) would fail here rather than pass
/// quietly.
///
/// The expected body is stock git 2.55.0's, captured with
/// `git diff --no-index --patience -U3`; the `Histogram` assertion below is the same
/// command with `--histogram`. Both sides are full of repeated braces and blank lines,
/// so most of the file has no unique pairs at all and the recursion has to fall back to
/// Myers for those stretches — the fallback path is exercised, not just the anchor walk.
#[test]
fn patience_anchors_on_unique_lines() {
    let before = "if (c)\n\n\ny = 2;\nx = 1;\n}\n}\ny = 2;\ny = 2;\n}\n{\nelse\nreturn;\n{\nelse\n{\nx = 1;\nelse\n";
    let after = "if (c)\nz = 3;\nif (c)\nx = 1;\n\n}\nz = 3;\nx = 1;\nz = 3;\nwhile (d)\n{\n{\nx = 1;\nif (c)\nif (c)\n}\nx = 1;\nif (c)\n";

    let input = InternedInput::new(before, after);
    let mut patience = Diff::compute(Algorithm::Patience, &input);
    patience.postprocess_lines(&input);
    let patience_text = patience
        .unified_diff(
            &BasicLineDiffPrinter(&input.interner),
            UnifiedDiffConfig::default(),
            &input,
        )
        .to_string();

    expect![[r#"
        @@ -1,18 +1,18 @@
         if (c)
        -
        -
        -y = 2;
        +z = 3;
        +if (c)
         x = 1;
        +
         }
        -}
        -y = 2;
        -y = 2;
        -}
        -{
        -else
        -return;
        +z = 3;
        +x = 1;
        +z = 3;
        +while (d)
         {
        -else
         {
         x = 1;
        -else
        +if (c)
        +if (c)
        +}
        +x = 1;
        +if (c)
    "#]]
    .assert_eq(&patience_text);

    // The guard against a silent fallback: on this input git's two algorithms disagree,
    // so an implementation that produced the histogram edit script would be wrong.
    let mut histogram = Diff::compute(Algorithm::Histogram, &input);
    histogram.postprocess_lines(&input);
    let histogram_text = histogram
        .unified_diff(
            &BasicLineDiffPrinter(&input.interner),
            UnifiedDiffConfig::default(),
            &input,
        )
        .to_string();
    assert_ne!(
        patience_text, histogram_text,
        "patience must not collapse into the histogram edit script on this input"
    );
}

/// The recursion's two degenerate exits, which upstream handles before it ever builds a
/// hashmap: one empty side, and two sides with no line in common at all.
#[test]
fn patience_handles_empty_and_disjoint_sides() {
    for (before, after) in [("", "a\nb\n"), ("a\nb\n", ""), ("a\nb\n", "c\nd\n")] {
        let input = InternedInput::new(before, after);
        let patience = Diff::compute(Algorithm::Patience, &input);
        let myers = Diff::compute(Algorithm::Myers, &input);
        assert_eq!(
            patience.count_removals(),
            myers.count_removals(),
            "removals disagree for {before:?} -> {after:?}"
        );
        assert_eq!(
            patience.count_additions(),
            myers.count_additions(),
            "additions disagree for {before:?} -> {after:?}"
        );
    }
}
