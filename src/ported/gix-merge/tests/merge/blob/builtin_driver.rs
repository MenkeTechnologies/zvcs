use gix_merge::blob::{
    Resolution, builtin_driver,
    builtin_driver::binary::{Pick, ResolveWith},
};

#[test]
fn binary() {
    assert_eq!(
        builtin_driver::binary(None),
        (Pick::Ours, Resolution::Conflict),
        "by default it picks ours and marks it as conflict"
    );
    assert_eq!(
        builtin_driver::binary(Some(ResolveWith::Ancestor)),
        (Pick::Ancestor, Resolution::CompleteWithAutoResolvedConflict),
        "Otherwise we can pick anything and it will mark it as complete"
    );
    assert_eq!(
        builtin_driver::binary(Some(ResolveWith::Ours)),
        (Pick::Ours, Resolution::CompleteWithAutoResolvedConflict)
    );
    assert_eq!(
        builtin_driver::binary(Some(ResolveWith::Theirs)),
        (Pick::Theirs, Resolution::CompleteWithAutoResolvedConflict)
    );
}

mod text {
    use arbitrary::Arbitrary;
    use bstr::ByteSlice;
    use gix_merge::blob::{
        Resolution, builtin_driver,
        builtin_driver::text::{self, Conflict, ConflictStyle},
    };
    use pretty_assertions::assert_str_eq;
    use std::num::NonZero;

    /// Cases where this build still differs from `git merge-file`, checked case by case:
    /// all four are the histogram variants of one fixture, so what is left is a
    /// difference between `imara-diff`'s histogram and git's, not between the merge
    /// drivers — the same fixture's Myers variants match.
    const DIVERGING: &[&str] = &[
        "complex/spurious-c-conflicts/diff3-histogram.merged",
        "complex/spurious-c-conflicts/zdiff3-histogram.merged",
    ];

    /// Should be a copy of `DIVERGING` once the reverse operation truly works like before
    const DIVERGING_REVERSED: &[&str] = &[
        "complex/spurious-c-conflicts/diff3-histogram.merged-reversed",
        "complex/spurious-c-conflicts/zdiff3-histogram.merged-reversed",
    ];

    // TODO: fix all of these eventually
    fn is_case_diverging(case: &baseline::Expectation, diverging: &[&str]) -> bool {
        diverging.iter().any(|name| case.name == *name)
    }

    #[test]
    fn fuzzed() {
        for (ours, base, theirs, opts) in [
            (
                &[255, 10, 10, 255][..],
                &[0, 10, 10, 13, 10, 193, 0, 51, 8, 33][..],
                &[10, 255, 10, 10, 10, 0, 10][..],
                builtin_driver::text::Options {
                    conflict: Conflict::ResolveWithUnion,
                    diff_algorithm: imara_diff::Algorithm::Myers,
                    ..Default::default()
                },
            ),
            (
                &[],
                &[10, 255, 255, 255],
                &[255, 10, 255, 10, 10, 255, 40],
                builtin_driver::text::Options::default(),
            ),
        ] {
            let mut out = Vec::new();
            let mut input = imara_diff::InternedInput::default();
            gix_merge::blob::builtin_driver::text(&mut out, &mut input, Default::default(), ours, base, theirs, opts);
        }
    }

    #[derive(Debug, Arbitrary)]
    struct FuzzCtx<'a> {
        base: &'a [u8],
        ours: &'a [u8],
        theirs: &'a [u8],
        marker_size: NonZero<u8>,
    }

    fn run_fuzz_case(ours: &[u8], base: &[u8], theirs: &[u8], marker_size: NonZero<u8>) {
        let mut out = Vec::new();
        let mut input = imara_diff::InternedInput::default();
        // Keep this in sync with the fuzz target. Histogram remains enabled here because it is the
        // diff algorithm we fuzz through gix-merge itself. Myers-family algorithms have
        // pathological cases that are expensive enough under fuzz instrumentation to turn the
        // target into a timeout reproducer for the diff backend instead of a useful gix-merge
        // fuzz harness.
        for (left, right) in [(ours, theirs), (theirs, ours)] {
            input.clear();
            let merge = text::Merge::new(&mut input, left, base, right, imara_diff::Algorithm::Histogram);
            let resolution = merge.run(&mut out, Default::default(), Conflict::default());
            if resolution == Resolution::Conflict {
                for conflict in [
                    Conflict::ResolveWithOurs,
                    Conflict::ResolveWithTheirs,
                    Conflict::ResolveWithUnion,
                    Conflict::Keep {
                        style: ConflictStyle::Diff3,
                        marker_size,
                    },
                    Conflict::Keep {
                        style: ConflictStyle::ZealousDiff3,
                        marker_size,
                    },
                ] {
                    merge.run(&mut out, Default::default(), conflict);
                }
            }
        }
    }

    #[test]
    fn clusterfuzz_timeout_regression() {
        for (name, data) in [
            (
                "clusterfuzz-testcase-minimized-gix-merge-blob-6377298803884032",
                include_bytes!("../../fixtures/clusterfuzz-testcase-minimized-gix-merge-blob-6377298803884032")
                    .as_slice(),
            ),
            (
                "clusterfuzz-testcase-minimized-gix-merge-blob-5577413097750528",
                include_bytes!("../../fixtures/clusterfuzz-testcase-minimized-gix-merge-blob-5577413097750528")
                    .as_slice(),
            ),
        ] {
            let ctx = FuzzCtx::arbitrary(&mut arbitrary::Unstructured::new(data))
                .unwrap_or_else(|_| panic!("{name}: testcase matches the historical fuzz target input layout"));
            run_fuzz_case(ctx.ours, ctx.base, ctx.theirs, ctx.marker_size);
        }
    }

    #[test]
    fn run_baseline() -> crate::Result {
        let root = gix_testtools::scripted_fixture_read_only("text-baseline.sh")?;
        for (baseline, diverging, expected_percentage) in [
            // Down from 10% before `xdiff/xmerge.c` was ported.
            ("baseline.cases", DIVERGING, 0),
            ("baseline-reversed.cases", DIVERGING_REVERSED, 0),
        ] {
            let cases = std::fs::read_to_string(root.join(baseline))?;
            let mut out = Vec::new();
            let mut num_diverging = 0;
            let mut num_cases = 0;
            for case in baseline::Expectations::new(&root, &cases) {
                num_cases += 1;
                let mut input = imara_diff::InternedInput::default();
                let actual = gix_merge::blob::builtin_driver::text(
                    &mut out,
                    &mut input,
                    case.labels(),
                    &case.ours,
                    &case.base,
                    &case.theirs,
                    case.options,
                );
                if is_case_diverging(&case, diverging) {
                    num_diverging += 1;
                } else {
                    if case.expected.contains_str("<<<<<<<") {
                        assert_eq!(actual, Resolution::Conflict, "{}: resolution mismatch", case.name);
                    } else {
                        assert!(
                            matches!(
                                actual,
                                Resolution::Complete | Resolution::CompleteWithAutoResolvedConflict
                            ),
                            "{}: resolution mismatch",
                            case.name
                        );
                    }
                    assert_str_eq!(
                        out.as_bstr().to_str_lossy(),
                        case.expected.to_str_lossy(),
                        "{}: output mismatch\n{}",
                        case.name,
                        out.as_bstr()
                    );
                    assert_eq!(out.as_bstr(), case.expected);
                }
            }

            assert_eq!(
                num_diverging,
                diverging.len(),
                "Number of expected diverging cases must match the actual one - probably the implementation improved"
            );
            assert_eq!(
                ((num_diverging as f32 / num_cases as f32) * 100.0) as usize,
                expected_percentage,
                "Just to show the percentage of skipped tests - this should get better"
            );
        }
        Ok(())
    }

    /// Every expectation here is the verbatim output of
    /// `git merge-file -p -L ours -L base -L theirs ours base theirs` from git 2.55.0.
    mod xdl_regions {
        use super::{Resolution, builtin_driver};
        use bstr::ByteSlice;
        use builtin_driver::text::{Conflict, ConflictStyle, Labels, Level, Options};

        fn run(ours: &[u8], base: &[u8], theirs: &[u8], conflict: Conflict, level: Level) -> (Vec<u8>, Resolution) {
            run_styled(ours, base, theirs, conflict, level, None)
        }

        fn run_styled(
            ours: &[u8],
            base: &[u8],
            theirs: &[u8],
            conflict: Conflict,
            level: Level,
            style: Option<ConflictStyle>,
        ) -> (Vec<u8>, Resolution) {
            let mut input = imara_diff::InternedInput::default();
            let mut out = Vec::new();
            let labels = Labels {
                ancestor: Some("base".into()),
                current: Some("ours".into()),
                other: Some("theirs".into()),
            };
            let resolution = builtin_driver::text(
                &mut out,
                &mut input,
                labels,
                ours,
                base,
                theirs,
                Options {
                    conflict,
                    level,
                    style,
                    ..Default::default()
                },
            );
            (out, resolution)
        }

        fn keep(style: ConflictStyle) -> Conflict {
            Conflict::Keep {
                style,
                marker_size: 7.try_into().unwrap(),
            }
        }

        /// Two changed regions with no unchanged ancestor line between them are one
        /// region to `xdl_do_merge()`, whose independence test is the strict
        /// `xscr1->i1 + xscr1->chg1 < xscr2->i1`.
        ///
        /// Resolving them independently instead yields `1\nTWO\nTHREE\n4\n` — a file
        /// neither side ever wrote, reported as a clean merge.
        #[test]
        fn touching_changes_are_one_conflict() {
            let (base, ours, theirs) = (&b"1\n2\n3\n4\n"[..], &b"1\n2\nTHREE\n4\n"[..], &b"1\nTWO\n3\n4\n"[..]);
            for (style, expected) in [
                (
                    ConflictStyle::Merge,
                    &b"1\n<<<<<<< ours\n2\nTHREE\n=======\nTWO\n3\n>>>>>>> theirs\n4\n"[..],
                ),
                (
                    ConflictStyle::Diff3,
                    &b"1\n<<<<<<< ours\n2\nTHREE\n||||||| base\n2\n3\n=======\nTWO\n3\n>>>>>>> theirs\n4\n"[..],
                ),
                (
                    ConflictStyle::ZealousDiff3,
                    &b"1\n<<<<<<< ours\n2\nTHREE\n||||||| base\n2\n3\n=======\nTWO\n3\n>>>>>>> theirs\n4\n"[..],
                ),
            ] {
                let (out, resolution) = run(ours, base, theirs, keep(style), Level::Zealous);
                assert_eq!(out.as_bstr(), expected.as_bstr(), "{style:?}");
                assert_eq!(resolution, Resolution::Conflict, "{style:?}");
            }

            let (out, resolution) = run(ours, base, theirs, Conflict::ResolveWithOurs, Level::Zealous);
            assert_eq!(out.as_bstr(), b"1\n2\nTHREE\n4\n".as_bstr());
            assert_eq!(resolution, Resolution::CompleteWithAutoResolvedConflict);

            let (out, _) = run(ours, base, theirs, Conflict::ResolveWithUnion, Level::Zealous);
            assert_eq!(out.as_bstr(), b"1\n2\nTHREE\nTWO\n3\n4\n".as_bstr());
        }

        /// One unchanged line is all it takes for the two changes to stay independent,
        /// which is the other half of the strict inequality above.
        #[test]
        fn a_single_unchanged_line_keeps_changes_independent() {
            let (out, resolution) = run(
                b"1\n2\n3\nFOUR\n5\n",
                b"1\n2\n3\n4\n5\n",
                b"1\nTWO\n3\n4\n5\n",
                keep(ConflictStyle::Merge),
                Level::Zealous,
            );
            assert_eq!(out.as_bstr(), b"1\nTWO\n3\nFOUR\n5\n".as_bstr());
            assert_eq!(resolution, Resolution::Complete);
        }

        /// `xdl_simplify_non_conflicts()` folds at most three unchanged lines between
        /// two conflicts into them, because doing so takes up no more lines.
        #[test]
        fn three_unchanged_lines_between_conflicts_are_folded_in() {
            let (out, _) = run(
                b"a\nO1\nc\nd\ne\nO2\ng\n",
                b"a\nb\nc\nd\ne\nf\ng\n",
                b"a\nT1\nc\nd\ne\nT2\ng\n",
                keep(ConflictStyle::Merge),
                Level::Zealous,
            );
            assert_eq!(
                out.as_bstr(),
                b"a\n<<<<<<< ours\nO1\nc\nd\ne\nO2\n=======\nT1\nc\nd\ne\nT2\n>>>>>>> theirs\ng\n".as_bstr()
            );
        }

        #[test]
        fn four_unchanged_lines_between_conflicts_stay_out() {
            let (out, _) = run(
                b"a\nO1\nc\nd\ne\nf\nO2\nh\n",
                b"a\nb\nc\nd\ne\nf\ng\nh\n",
                b"a\nT1\nc\nd\ne\nf\nT2\nh\n",
                keep(ConflictStyle::Merge),
                Level::Zealous,
            );
            assert_eq!(
                out.as_bstr(),
                b"a\n<<<<<<< ours\nO1\n=======\nT1\n>>>>>>> theirs\nc\nd\ne\nf\n<<<<<<< ours\nO2\n=======\nT2\n>>>>>>> theirs\nh\n".as_bstr()
            );
        }

        /// The only difference between the two levels git uses: `ZealousAlnum`
        /// (`builtin/merge-file.c`) folds a gap holding no letter or digit no matter how
        /// long, `Zealous` (`merge-ll.c`, so `git merge`) does not. Both expectations
        /// come from the corresponding stock command.
        #[test]
        fn alnum_free_gaps_are_folded_only_at_the_higher_level() {
            let (ours, base, theirs) = (
                &b"a\nO1\n{\n}\n(\n)\nO2\nh\n"[..],
                &b"a\nb\n{\n}\n(\n)\ng\nh\n"[..],
                &b"a\nT1\n{\n}\n(\n)\nT2\nh\n"[..],
            );
            let (out, _) = run(ours, base, theirs, keep(ConflictStyle::Merge), Level::ZealousAlnum);
            assert_eq!(
                out.as_bstr(),
                b"a\n<<<<<<< ours\nO1\n{\n}\n(\n)\nO2\n=======\nT1\n{\n}\n(\n)\nT2\n>>>>>>> theirs\nh\n".as_bstr(),
                "git merge-file folds it"
            );
            let (out, _) = run(ours, base, theirs, keep(ConflictStyle::Merge), Level::Zealous);
            assert_eq!(
                out.as_bstr(),
                b"a\n<<<<<<< ours\nO1\n=======\nT1\n>>>>>>> theirs\n{\n}\n(\n)\n<<<<<<< ours\nO2\n=======\nT2\n>>>>>>> theirs\nh\n".as_bstr(),
                "git merge does not"
            );
        }

        /// `Minimal` conflicts over every overlap, even one where both sides made the
        /// very same change, which `Eager` and up resolve silently.
        #[test]
        fn minimal_level_conflicts_over_identical_changes() {
            let (ours, base, theirs) = (&b"1\nSAME\n3\n"[..], &b"1\n2\n3\n"[..], &b"1\nSAME\n3\n"[..]);
            let (out, resolution) = run(ours, base, theirs, keep(ConflictStyle::Merge), Level::Minimal);
            assert_eq!(
                out.as_bstr(),
                b"1\n<<<<<<< ours\nSAME\n=======\nSAME\n>>>>>>> theirs\n3\n".as_bstr()
            );
            assert_eq!(resolution, Resolution::Conflict);

            let (out, resolution) = run(ours, base, theirs, keep(ConflictStyle::Merge), Level::Eager);
            assert_eq!(out.as_bstr(), b"1\nSAME\n3\n".as_bstr());
            assert_eq!(resolution, Resolution::Complete);
        }

        /// git keeps `xmp.style` independent of `xmp.favor`, so a union merge still gets
        /// the region shapes the style implies: the diff3 styles clamp the level to
        /// `Eager`, which leaves the two conflicts of `three_unchanged_lines_…` apart and
        /// unions each one separately. Both expectations are
        /// `git merge-file -p [--zdiff3] --union`.
        #[test]
        fn the_style_shapes_regions_even_when_conflicts_are_resolved() {
            let (ours, base, theirs) = (
                &b"a\nO1\nc\nd\ne\nO2\ng\n"[..],
                &b"a\nb\nc\nd\ne\nf\ng\n"[..],
                &b"a\nT1\nc\nd\ne\nT2\ng\n"[..],
            );
            let (out, _) = run_styled(
                ours,
                base,
                theirs,
                Conflict::ResolveWithUnion,
                Level::ZealousAlnum,
                Some(ConflictStyle::ZealousDiff3),
            );
            assert_eq!(out.as_bstr(), b"a\nO1\nT1\nc\nd\ne\nO2\nT2\ng\n".as_bstr());

            let (out, _) = run_styled(
                ours,
                base,
                theirs,
                Conflict::ResolveWithUnion,
                Level::ZealousAlnum,
                Some(ConflictStyle::Merge),
            );
            assert_eq!(out.as_bstr(), b"a\nO1\nc\nd\ne\nO2\nT1\nc\nd\ne\nT2\ng\n".as_bstr());
        }

        /// `xdl_merge()` returns the other side's buffer verbatim when a side did not
        /// change anything, so a file with no trailing newline keeps not having one.
        #[test]
        fn an_unchanged_side_is_copied_verbatim() {
            let (out, resolution) = run(
                b"1\n2\n3",
                b"1\n2\n3",
                b"1\nTWO\n3",
                keep(ConflictStyle::Merge),
                Level::Zealous,
            );
            assert_eq!(out.as_bstr(), b"1\nTWO\n3".as_bstr());
            assert_eq!(resolution, Resolution::Complete);
        }
    }

    #[test]
    fn both_sides_same_changes_are_conflict_free() {
        for conflict in [
            builtin_driver::text::Conflict::Keep {
                style: ConflictStyle::Merge,
                marker_size: 7.try_into().unwrap(),
            },
            builtin_driver::text::Conflict::Keep {
                style: ConflictStyle::Diff3,
                marker_size: 7.try_into().unwrap(),
            },
            builtin_driver::text::Conflict::Keep {
                style: ConflictStyle::ZealousDiff3,
                marker_size: 7.try_into().unwrap(),
            },
            builtin_driver::text::Conflict::ResolveWithOurs,
            builtin_driver::text::Conflict::ResolveWithTheirs,
            builtin_driver::text::Conflict::ResolveWithUnion,
        ] {
            let options = builtin_driver::text::Options {
                conflict,
                ..Default::default()
            };
            let mut input = imara_diff::InternedInput::default();
            let mut out = Vec::new();
            let actual = builtin_driver::text(
                &mut out,
                &mut input,
                Default::default(),
                b"1\n3\nother",
                b"1\n2\n3",
                b"1\n3\nother",
                options,
            );
            assert_eq!(actual, Resolution::Complete, "{conflict:?}");
        }
    }

    #[test]
    fn both_differ_partially_resolution_is_conflicting() {
        for (conflict, expected) in [
            (
                builtin_driver::text::Conflict::Keep {
                    style: ConflictStyle::Merge,
                    marker_size: 7.try_into().unwrap(),
                },
                Resolution::Conflict,
            ),
            (
                builtin_driver::text::Conflict::Keep {
                    style: ConflictStyle::Diff3,
                    marker_size: 7.try_into().unwrap(),
                },
                Resolution::Conflict,
            ),
            (
                builtin_driver::text::Conflict::Keep {
                    style: ConflictStyle::ZealousDiff3,
                    marker_size: 7.try_into().unwrap(),
                },
                Resolution::Conflict,
            ),
            (
                builtin_driver::text::Conflict::ResolveWithOurs,
                Resolution::CompleteWithAutoResolvedConflict,
            ),
            (
                builtin_driver::text::Conflict::ResolveWithTheirs,
                Resolution::CompleteWithAutoResolvedConflict,
            ),
            (
                builtin_driver::text::Conflict::ResolveWithUnion,
                Resolution::CompleteWithAutoResolvedConflict,
            ),
        ] {
            let options = builtin_driver::text::Options {
                conflict,
                ..Default::default()
            };
            let mut input = imara_diff::InternedInput::default();
            let mut out = Vec::new();
            let actual = builtin_driver::text(
                &mut out,
                &mut input,
                Default::default(),
                b"1\n3\nours",
                b"1\n2\n3",
                b"1\n3\ntheirs",
                options,
            );
            assert_eq!(actual, expected, "{conflict:?}");
        }
    }

    mod false_conflict {
        use gix_merge::blob::{Resolution, builtin_driver, builtin_driver::text::Conflict};
        use imara_diff::InternedInput;

        /// Minimal reproduction: Myers produces a false conflict where git merge-file resolves cleanly.
        ///
        /// base:   alpha_x / (blank) / bravo_x / charlie_x / (blank)
        /// ours:   (blank) / (blank) / bravo_x / charlie_x
        /// theirs: alpha_x / (blank) / charlie_x / (blank)
        ///
        /// base→ours:  alpha_x deleted (replaced by blank), trailing blank removed
        /// base→theirs: bravo_x deleted
        ///
        /// These are non-overlapping changes that git merges cleanly.
        /// See https://github.com/GitoxideLabs/gitoxide/issues/2475
        #[test]
        fn myers_false_conflict_with_blank_line_ambiguity() {
            let base = b"alpha_x\n\nbravo_x\ncharlie_x\n\n";
            let ours = b"\n\nbravo_x\ncharlie_x\n";
            let theirs = b"alpha_x\n\ncharlie_x\n\n";

            let labels = builtin_driver::text::Labels {
                ancestor: Some("base".into()),
                current: Some("ours".into()),
                other: Some("theirs".into()),
            };

            // Histogram resolves cleanly.
            {
                let options = builtin_driver::text::Options {
                    diff_algorithm: imara_diff::Algorithm::Histogram,
                    conflict: Conflict::Keep {
                        style: builtin_driver::text::ConflictStyle::Merge,
                        marker_size: 7.try_into().unwrap(),
                    },
                    ..Default::default()
                };
                let mut out = Vec::new();
                let mut input = InternedInput::default();
                let res = builtin_driver::text(&mut out, &mut input, labels, ours, base, theirs, options);
                assert_eq!(res, Resolution::Complete, "Histogram should resolve cleanly");
            }

            // Myers should also resolve cleanly (it used to produce a false conflict because
            // imara-diff's Myers splits the ours change into two hunks — a deletion at base[0]
            // and an empty insertion at base[2] — and the insertion collided with theirs'
            // deletion at base[2]).
            {
                let options = builtin_driver::text::Options {
                    diff_algorithm: imara_diff::Algorithm::Myers,
                    conflict: Conflict::Keep {
                        style: builtin_driver::text::ConflictStyle::Merge,
                        marker_size: 7.try_into().unwrap(),
                    },
                    ..Default::default()
                };
                let mut out = Vec::new();
                let mut input = InternedInput::default();
                let res = builtin_driver::text(&mut out, &mut input, labels, ours, base, theirs, options);
                assert_eq!(
                    res,
                    Resolution::Complete,
                    "Myers should resolve cleanly (git merge-file does). Output:\n{}",
                    String::from_utf8_lossy(&out)
                );
            }
        }
    }

    mod baseline {
        use std::path::Path;

        use bstr::BString;
        use gix_merge::blob::builtin_driver::text::{Conflict, ConflictStyle};

        #[derive(Debug)]
        pub struct Expectation {
            pub ours: BString,
            pub ours_marker: String,
            pub theirs: BString,
            pub theirs_marker: String,
            pub base: BString,
            pub base_marker: String,
            pub name: BString,
            pub expected: BString,
            pub options: gix_merge::blob::builtin_driver::text::Options,
        }

        impl Expectation {
            pub fn labels(&self) -> gix_merge::blob::builtin_driver::text::Labels<'_> {
                gix_merge::blob::builtin_driver::text::Labels {
                    ancestor: Some(self.base_marker.as_str().as_ref()),
                    current: Some(self.ours_marker.as_str().as_ref()),
                    other: Some(self.theirs_marker.as_str().as_ref()),
                }
            }
        }

        pub struct Expectations<'a> {
            root: &'a Path,
            lines: std::str::Lines<'a>,
        }

        impl<'a> Expectations<'a> {
            pub fn new(root: &'a Path, cases: &'a str) -> Self {
                Expectations {
                    root,
                    lines: cases.lines(),
                }
            }
        }

        impl Iterator for Expectations<'_> {
            type Item = Expectation;

            fn next(&mut self) -> Option<Self::Item> {
                let line = self.lines.next()?;
                let mut words = line.split(' ');
                let (Some(ours), Some(base), Some(theirs), Some(output)) =
                    (words.next(), words.next(), words.next(), words.next())
                else {
                    panic!("need at least the input and output")
                };

                let read = |rela_path: &str| read_blob(self.root, rela_path);

                let mut options = gix_merge::blob::builtin_driver::text::Options {
                    // `text-baseline.sh` records the output of `git merge-file`, and
                    // `builtin/merge-file.c` runs at `XDL_MERGE_ZEALOUS_ALNUM` — a level
                    // above the `merge-ll.c` default these options otherwise carry.
                    level: gix_merge::blob::builtin_driver::text::Level::ZealousAlnum,
                    ..Default::default()
                };
                let marker_size = 7.try_into().unwrap();
                for arg in words {
                    let (conflict, style) = match arg {
                        "--diff3" => (
                            Conflict::Keep {
                                style: ConflictStyle::Diff3,
                                marker_size,
                            },
                            ConflictStyle::Diff3,
                        ),
                        "--zdiff3" => (
                            Conflict::Keep {
                                style: ConflictStyle::ZealousDiff3,
                                marker_size,
                            },
                            ConflictStyle::ZealousDiff3,
                        ),
                        "--ours" => (Conflict::ResolveWithOurs, ConflictStyle::Merge),
                        "--theirs" => (Conflict::ResolveWithTheirs, ConflictStyle::Merge),
                        "--union" => (Conflict::ResolveWithUnion, ConflictStyle::Merge),
                        _ => panic!("Unknown argument to parse into options: '{arg}'"),
                    };
                    options.conflict = conflict;
                    // The style flags and the favor flags are separate arguments on the
                    // command line, so a later favor must not drop an earlier style.
                    if !matches!(style, ConflictStyle::Merge) {
                        options.style = Some(style);
                    }
                }
                if output.contains("histogram") {
                    options.diff_algorithm = imara_diff::Algorithm::Histogram;
                }

                Some(Expectation {
                    ours: read(ours),
                    ours_marker: ours.into(),
                    theirs: read(theirs),
                    theirs_marker: theirs.into(),
                    base: read(base),
                    base_marker: base.into(),
                    expected: read(output),
                    name: output.into(),
                    options,
                })
            }
        }

        fn read_blob(root: &Path, rela_path: &str) -> BString {
            std::fs::read(root.join(rela_path))
                .unwrap_or_else(|err| panic!("Failed to read '{rela_path}' in '{}': {err}", root.display()))
                .into()
        }
    }
}
