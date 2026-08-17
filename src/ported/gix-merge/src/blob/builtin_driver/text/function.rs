use crate::blob::{
    Resolution,
    builtin_driver::text::{
        Canonicalize, Conflict, Labels, Merge, Options, Rendering,
        xdl::{self, Classes, Files, Params, Side},
    },
};

impl<'input, 'data> Merge<'input, 'data> {
    /// Prepare merge state for `current`, `ancestor`, and `other` using `diff_algorithm`.
    ///
    /// This computes the two change scripts once so they can be rendered multiple times with
    /// [`Merge::run()`], which is useful when experimenting with multiple conflict styles
    /// for the same input triplet.
    ///
    /// The returned [`Merge`] keeps a reference to the provided `input`, which guarantees
    /// that subsequent calls to [`Merge::run()`] use the exact same interner state.
    pub fn new(
        input: &'input mut imara_diff::InternedInput<&'data [u8]>,
        current: &'data [u8],
        ancestor: &'data [u8],
        other: &'data [u8],
        diff_algorithm: imara_diff::Algorithm,
    ) -> Merge<'input, 'data> {
        Merge::new_canonicalized(input, current, ancestor, other, diff_algorithm, None)
    }

    /// [`Merge::new()`] with `xpp.flags`' whitespace rule in force — see
    /// [`Canonicalize`].
    pub fn new_canonicalized(
        input: &'input mut imara_diff::InternedInput<&'data [u8]>,
        current: &'data [u8],
        ancestor: &'data [u8],
        other: &'data [u8],
        diff_algorithm: imara_diff::Algorithm,
        canonicalize: Option<Canonicalize>,
    ) -> Merge<'input, 'data> {
        let ancestor_lines: Vec<&'data [u8]> = xdl::tokens(ancestor).collect();
        let current_lines: Vec<&'data [u8]> = xdl::tokens(current).collect();
        let other_lines: Vec<&'data [u8]> = xdl::tokens(other).collect();

        // One class table for all three sides: `xdl_recmatch()` is asked about
        // pairs drawn from any two of them, so they have to agree on which lines
        // are the same line.
        let mut classes = Classes::new(canonicalize);

        // `xdl_merge()`'s pair of `xdl_do_diff()` calls: both sides against the
        // shared ancestor.
        input.update_before(ancestor_lines.iter().map(|&line| classes.representative(line)));
        input.update_after(current_lines.iter().map(|&line| classes.representative(line)));
        let ours = xdl::build_script(diff_algorithm, input);

        // Interning is shared, so the current-side tokens stay valid once `after`
        // is re-filled with the other side.
        let current_tokens = std::mem::take(&mut input.after);
        input.update_after(other_lines.iter().map(|&line| classes.representative(line)));
        let theirs = xdl::build_script(diff_algorithm, input);

        Merge {
            input,
            current,
            other,
            current_tokens,
            ancestor_lines,
            current_lines,
            other_lines,
            ours,
            theirs,
            diff_algorithm,
            canonicalize,
        }
    }

    /// Merge `current` and `other` with `ancestor` as base using `conflict` as strategy.
    ///
    /// Use `labels` to annotate conflict sections.
    ///
    /// Place the merged result in `out` (cleared before use) and return the resolution.
    pub fn run(&self, out: &mut Vec<u8>, labels: Labels<'_>, conflict: Conflict) -> Resolution {
        self.run_with(
            out,
            labels,
            Rendering {
                conflict,
                ..Default::default()
            },
        )
        .0
    }

    /// Like [`Merge::run()`], but with every `xmparam_t` knob spelled out, and returning
    /// how many conflicting regions the merge produced alongside the resolution.
    ///
    /// The count is what `git merge-file` reports as its exit code, so it is zero
    /// whenever `rendering` resolved the conflicts automatically — git counts them
    /// only after that rewrite. Use the [`Resolution`] to tell such a merge from a
    /// clean one.
    pub fn run_with(&self, out: &mut Vec<u8>, labels: Labels<'_>, rendering: Rendering) -> (Resolution, usize) {
        out.clear();

        // `xdl_merge()` short-circuits when one side is unchanged: the other
        // side's postimage *is* the merge result, byte for byte.
        if self.ours.is_empty() {
            out.extend_from_slice(self.other);
            return (Resolution::Complete, 0);
        }
        if self.theirs.is_empty() {
            out.extend_from_slice(self.current);
            return (Resolution::Complete, 0);
        }

        let favor = match rendering.conflict {
            Conflict::Keep { .. } => 0,
            Conflict::ResolveWithOurs => 1,
            Conflict::ResolveWithTheirs => 2,
            Conflict::ResolveWithUnion => 3,
        };

        let files = Files {
            ancestor: Side {
                tokens: &self.input.before,
                lines: &self.ancestor_lines,
            },
            ours: Side {
                tokens: &self.current_tokens,
                lines: &self.current_lines,
            },
            theirs: Side {
                tokens: &self.input.after,
                lines: &self.other_lines,
            },
        };
        let conflicts = xdl::merge_scripts(
            &files,
            &self.ours,
            &self.theirs,
            Params {
                labels,
                favor,
                style: rendering.style(),
                marker_size: rendering.marker_size(),
                level: rendering.level,
                algorithm: self.diff_algorithm,
                canonicalize: self.canonicalize,
            },
            out,
        );

        let resolution = match (conflicts.before_favor, favor) {
            (0, _) => Resolution::Complete,
            (_, 0) => Resolution::Conflict,
            (_, _) => Resolution::CompleteWithAutoResolvedConflict,
        };
        (resolution, conflicts.reported)
    }
}

/// Merge `current` and `other` with `ancestor` as base according to `opts`.
///
/// Use `labels` to annotate conflict sections.
///
/// `input` is for reusing memory for lists of tokens, but note that it grows indefinitely
/// while tokens for `current`, `ancestor` and `other` are added.
/// Place the merged result in `out` (cleared before use) and return the resolution.
///
/// # Important
///
/// *The caller* is responsible for clearing `input`, otherwise tokens will accumulate.
/// This idea is to save time if the input is known to be very similar.
pub fn merge<'a>(
    out: &mut Vec<u8>,
    input: &mut imara_diff::InternedInput<&'a [u8]>,
    labels: Labels<'_>,
    current: &'a [u8],
    ancestor: &'a [u8],
    other: &'a [u8],
    options: Options,
) -> Resolution {
    merge_counted(out, input, labels, current, ancestor, other, options).0
}

/// Like [`merge()`], but also returns the number of conflicting regions, which is
/// what `git merge-file` reports as its exit code.
pub fn merge_counted<'a>(
    out: &mut Vec<u8>,
    input: &mut imara_diff::InternedInput<&'a [u8]>,
    labels: Labels<'_>,
    current: &'a [u8],
    ancestor: &'a [u8],
    other: &'a [u8],
    Options {
        diff_algorithm,
        conflict,
        style,
        level,
        canonicalize,
    }: Options,
) -> (Resolution, usize) {
    out.clear();
    let merge = Merge::new_canonicalized(input, current, ancestor, other, diff_algorithm, canonicalize);
    merge.run_with(
        out,
        labels,
        Rendering {
            conflict,
            style,
            level,
            marker_size: None,
        },
    )
}
