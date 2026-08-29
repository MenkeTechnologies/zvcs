//! `git diff --no-index`: the whole rendering surface of the mode that has no
//! repository behind it.
//!
//! `--no-index` is the one diff invocation a user reaches for outside version
//! control at all — two files, two directories, a file against `/dev/null`,
//! `-` for stdin — and git supports very nearly the entire diff option set
//! there. The corpus reached exactly three of those options: [`shape_reach`]'s
//! block pins `--raw`, `--summary` and the `--abbrev` family because each was a
//! fixed defect. Everything a human actually looks at — the unified patch, the
//! stat family, the ignore-whitespace family, the binary line, the prefixes,
//! the exit code — was measured only by whatever the *in-repository* diff cases
//! happened to share with it, which is not the same code path: `diff_no_index()`
//! (diff-no-index.c) builds its queue from `stat`ed files with no index, no
//! attributes, no config from a repository, and hands it to the same
//! `diff_flush()`. Everything before that flush is separate, and everything
//! about it was unmeasured.
//!
//! Three properties make this worth curating rather than fuzzing:
//!
//!  * **The mode is defined by what it does *not* have.** No repository means
//!    no `core.abbrev`, no `.gitattributes`, no `diff.*` config — so a port that
//!    reads any of them from the ambient environment answers differently here
//!    and nowhere else. Every case below runs under [`OUTSIDE`], which puts the
//!    fixture's own repository out of reach.
//!  * **The exit code is part of the contract.** `--no-index` exits 1 when the
//!    inputs differ, like `diff(1)` and unlike every other git command in the
//!    corpus, and `--exit-code`/`--quiet` layer on top of that. The runner
//!    compares exit status, so these cases pin it.
//!  * **Two of the inputs are not files.** `/dev/null` and `-` are spelled like
//!    paths and are not; a queue built by `stat` alone gets them wrong.
//!
//! The pairs come from [`crate::fixture::Shape::NoIndexTrees`], which grew five
//! of them for this module: a whitespace-only pair, a binary pair, a
//! missing-final-newline pair, a renamed pair, and a pair of C files with
//! function bodies. Without those, `-w` had nothing to ignore, `--binary` had
//! nothing binary, `\ No newline at end of file` was unreachable, `-M` had no
//! rename to find, and `--function-context` had no function.

use crate::fixture::Shape;
use crate::runner::Case;

/// The fixture's repository, out of reach: the ceiling stops the upward search
/// at the fixture root, so a case running in `ni/` finds no repository and
/// takes the no-index path with no configuration behind it. Same value
/// [`super::shape_reach`] uses, for the same reason.
const OUTSIDE: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}")];

/// One no-index case, run from `ni/` with no repository in reach.
fn ni(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::new("diff", args, Shape::NoIndexTrees).in_dir("ni").with_env(OUTSIDE));
}

/// The same, compared on stderr as well: the cases whose whole answer is a
/// refusal.
fn ni_strict(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::strict("diff", args, Shape::NoIndexTrees).in_dir("ni").with_env(OUTSIDE));
}

pub fn cases(out: &mut Vec<Case>) {
    patch_body(out);
    stat_family(out);
    name_only_family(out);
    whitespace(out);
    binary_and_newline(out);
    prefixes(out);
    exit_code(out);
    rename_detection(out);
    algorithms(out);
    inputs_that_are_not_files(out);
    refusals(out);
}

/// The unified patch itself, and the knobs that decide how much of it prints.
///
/// The default rendering had one case in the whole corpus (`diff --no-index da
/// db`, kept in [`super::shape_reach`] for the `core.abbrev` question), so the
/// hunk header, the context width and the `diff --git` line were pinned only as
/// a side effect of asking about something else.
fn patch_body(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "a.txt", "b.txt"][..],
        &["diff", "--no-index", "-p", "a.txt", "b.txt"],
        &["diff", "--no-index", "--patch", "a.txt", "b.txt"],
        // Context width, at the edges: none, one, and wider than the file.
        &["diff", "--no-index", "-U0", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "-U1", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--unified=7", "fn_a.c", "fn_b.c"],
        // `--function-context` extends the hunk to the enclosing function,
        // which is a different computation from any `-U<n>`.
        &["diff", "--no-index", "--function-context", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "-W", "-U0", "fn_a.c", "fn_b.c"],
        // Two hunks in one file, joined or kept apart by the gap rule.
        &["diff", "--no-index", "--inter-hunk-context=0", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--inter-hunk-context=8", "fn_a.c", "fn_b.c"],
        // A whole directory queue rendered as patches rather than as `--raw`.
        &["diff", "--no-index", "da", "db"],
        &["diff", "--no-index", "-p", "--stat", "da", "db"],
        // Tabs in the payload, expanded and not.
        &["diff", "--no-index", "--expand-tabs", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--no-expand-tabs", "fn_a.c", "fn_b.c"],
        // Colour, forced: `NO_COLOR` and a dumb terminal turn `auto` off on
        // both sides, so `always` is the only way to compare the escapes.
        &["diff", "--no-index", "--color=always", "a.txt", "b.txt"],
        &["diff", "--no-index", "--color=never", "a.txt", "b.txt"],
        &["diff", "--no-index", "--color-words", "a.txt", "b.txt"],
        &["diff", "--no-index", "--word-diff", "a.txt", "b.txt"],
        &["diff", "--no-index", "--word-diff=porcelain", "a.txt", "b.txt"],
        &["diff", "--no-index", "--word-diff=plain", "ws_a.txt", "ws_b.txt"],
    ] {
        ni(out, args);
    }
}

/// The stat family: five renderings of the same queue, each with its own
/// column arithmetic.
fn stat_family(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "--stat", "da", "db"][..],
        &["diff", "--no-index", "--stat", "a.txt", "b.txt"],
        // Widths, which is where the arithmetic is: git divides the terminal
        // width between the name column and the graph.
        &["diff", "--no-index", "--stat=40", "da", "db"],
        &["diff", "--no-index", "--stat-width=40", "--stat-name-width=8", "da", "db"],
        &["diff", "--no-index", "--stat-graph-width=6", "da", "db"],
        &["diff", "--no-index", "--numstat", "da", "db"],
        &["diff", "--no-index", "--shortstat", "da", "db"],
        &["diff", "--no-index", "--compact-summary", "da", "db"],
        &["diff", "--no-index", "--dirstat", "da", "db"],
        &["diff", "--no-index", "--dirstat=files,0", "da", "db"],
        &["diff", "--no-index", "--dirstat-by-file", "da", "db"],
        // The combinations that print two renderings of one queue in order.
        &["diff", "--no-index", "--patch-with-stat", "da", "db"],
        &["diff", "--no-index", "--patch-with-raw", "da", "db"],
        &["diff", "--no-index", "--stat", "--summary", "da", "db"],
        // `--numstat` on the binary pair prints `-\t-`, not a line count.
        &["diff", "--no-index", "--numstat", "bin_a.bin", "bin_b.bin"],
    ] {
        ni(out, args);
    }
}

/// The name-listing family, including the `-z` spelling every script uses.
fn name_only_family(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "--name-only", "da", "db"][..],
        &["diff", "--no-index", "--name-status", "da", "db"],
        &["diff", "--no-index", "-z", "--name-only", "da", "db"],
        &["diff", "--no-index", "-z", "--name-status", "da", "db"],
        &["diff", "--no-index", "-z", "--raw", "da", "db"],
        // The filters, on a queue that holds one of each kind.
        &["diff", "--no-index", "--diff-filter=A", "--name-status", "da", "db"],
        &["diff", "--no-index", "--diff-filter=D", "--name-status", "da", "db"],
        &["diff", "--no-index", "--diff-filter=M", "--name-status", "da", "db"],
        &["diff", "--no-index", "--diff-filter=ad", "--name-status", "da", "db"],
    ] {
        ni(out, args);
    }
}

/// The ignore-whitespace family, against the pair whose only difference is
/// whitespace — so each flag either empties the patch or does not, and the two
/// answers are distinguishable.
fn whitespace(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "ws_a.txt", "ws_b.txt"][..],
        &["diff", "--no-index", "-w", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "-b", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--ignore-space-at-eol", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--ignore-blank-lines", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--ignore-cr-at-eol", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "-w", "--ignore-blank-lines", "ws_a.txt", "ws_b.txt"],
        // With every difference ignored, the queue empties — and an empty queue
        // is also an exit code question.
        &["diff", "--no-index", "--exit-code", "-w", "--ignore-blank-lines", "ws_a.txt", "ws_b.txt"],
        // `--check` reports the whitespace errors instead of the patch, and
        // exits 2 when it finds any.
        &["diff", "--no-index", "--check", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--ws-error-highlight=all", "--color=always", "ws_a.txt", "ws_b.txt"],
    ] {
        ni(out, args);
    }
}

/// The binary pair and the missing final newline: two renderings that exist
/// nowhere else in the no-index queue.
fn binary_and_newline(out: &mut Vec<Case>) {
    for args in [
        // The default is the one-line "Binary files ... differ".
        &["diff", "--no-index", "bin_a.bin", "bin_b.bin"][..],
        // `--binary` emits the literal delta git can apply; `--text` forces the
        // bytes through the text path instead.
        &["diff", "--no-index", "--binary", "bin_a.bin", "bin_b.bin"],
        &["diff", "--no-index", "-a", "bin_a.bin", "bin_b.bin"],
        &["diff", "--no-index", "--text", "--stat", "bin_a.bin", "bin_b.bin"],
        &["diff", "--no-index", "--stat", "bin_a.bin", "bin_b.bin"],
        &["diff", "--no-index", "--summary", "bin_a.bin", "bin_b.bin"],
        // `\ No newline at end of file`, on the side that lacks it — and again
        // reversed, where it moves to the other side of the hunk.
        &["diff", "--no-index", "eol_a.txt", "eol_b.txt"],
        &["diff", "--no-index", "eol_b.txt", "eol_a.txt"],
        &["diff", "--no-index", "-R", "eol_a.txt", "eol_b.txt"],
        &["diff", "--no-index", "--numstat", "eol_a.txt", "eol_b.txt"],
    ] {
        ni(out, args);
    }
}

/// The `a/`…`b/` prefixes, which are pure rendering and which scripts parse.
fn prefixes(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "--no-prefix", "a.txt", "b.txt"][..],
        &["diff", "--no-index", "--src-prefix=old/", "a.txt", "b.txt"],
        &["diff", "--no-index", "--dst-prefix=new/", "a.txt", "b.txt"],
        &["diff", "--no-index", "--src-prefix=old/", "--dst-prefix=new/", "da", "db"],
        &["diff", "--no-index", "--default-prefix", "--no-prefix", "a.txt", "b.txt"],
        &["diff", "--no-index", "--line-prefix=| ", "a.txt", "b.txt"],
        &["diff", "--no-index", "--full-index", "da", "db"],
        &["diff", "--no-index", "--no-prefix", "--stat", "da", "db"],
    ] {
        ni(out, args);
    }
}

/// The exit code, which in this mode is the answer rather than a detail.
///
/// `--no-index` exits 1 on any difference — the `diff(1)` convention, not
/// git's — and 0 when the inputs match. `--quiet` additionally silences the
/// output, and `--exit-code` asks for the status without the silence. A port
/// that returns 0 for a difference is wrong in a way no stdout comparison
/// catches, because with `--quiet` there is no stdout at all.
fn exit_code(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "--exit-code", "a.txt", "b.txt"][..],
        &["diff", "--no-index", "--quiet", "a.txt", "b.txt"],
        &["diff", "--no-index", "--exit-code", "da", "db"],
        &["diff", "--no-index", "--quiet", "da", "db"],
        // Identical inputs: the same file named twice, and a copy of one
        // directory against itself. Exit 0, no output.
        &["diff", "--no-index", "a.txt", "a.txt"],
        &["diff", "--no-index", "--exit-code", "a.txt", "a.txt"],
        &["diff", "--no-index", "--quiet", "da", "da"],
        // A difference that only the ignore rules erase: the status has to
        // follow the emptied queue, not the raw comparison.
        &["diff", "--no-index", "--quiet", "-w", "ws_a.txt", "ws_b.txt"],
        // Binary difference through `--quiet`, where there is no text to print
        // and the status is the whole answer.
        &["diff", "--no-index", "--quiet", "bin_a.bin", "bin_b.bin"],
    ] {
        ni(out, args);
    }
}

/// Rename and copy detection on a queue built from two directories.
///
/// `diffcore_rename()` runs here exactly as it does in a repository, but on a
/// queue whose ids were computed by hashing files off disk rather than read
/// from an index — and [`super::shape_reach`] pins that it *skips* the hashing
/// pass on a degenerate queue. `ra`/`rb` is the input where the pass runs and
/// finds something.
fn rename_detection(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "-M", "--raw", "ra", "rb"][..],
        &["diff", "--no-index", "-M", "--name-status", "ra", "rb"],
        &["diff", "--no-index", "-M", "--summary", "ra", "rb"],
        &["diff", "--no-index", "-M", "--stat", "ra", "rb"],
        &["diff", "--no-index", "--find-renames=40%", "--name-status", "ra", "rb"],
        &["diff", "--no-index", "--no-renames", "--name-status", "ra", "rb"],
        &["diff", "--no-index", "-C", "--find-copies-harder", "--name-status", "da", "db"],
        &["diff", "--no-index", "-B", "--name-status", "a.txt", "b.txt"],
        &["diff", "--no-index", "-M", "-z", "--name-status", "ra", "rb"],
        // The rename pair as patches: with detection off it is a delete and an
        // add, with detection on it is a `similarity index` header.
        &["diff", "--no-index", "-M", "ra", "rb"],
        &["diff", "--no-index", "--no-renames", "ra", "rb"],
    ] {
        ni(out, args);
    }
}

/// The four diff algorithms, plus `--find-object`, on inputs where they can
/// disagree.
fn algorithms(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "--diff-algorithm=myers", "fn_a.c", "fn_b.c"][..],
        &["diff", "--no-index", "--diff-algorithm=minimal", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--diff-algorithm=patience", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--diff-algorithm=histogram", "fn_a.c", "fn_b.c"],
        &["diff", "--no-index", "--patience", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--histogram", "ws_a.txt", "ws_b.txt"],
        &["diff", "--no-index", "--minimal", "da", "db"],
        &["diff", "--no-index", "--anchored=four", "ws_a.txt", "ws_b.txt"],
        // An algorithm name git does not know: a refusal, and its wording is
        // the whole answer.
    ] {
        ni(out, args);
    }
    ni_strict(out, &["diff", "--no-index", "--diff-algorithm=nosuch", "a.txt", "b.txt"]);
}

/// The inputs that are spelled like paths and are not: `/dev/null`, `-`, and a
/// directory paired with a file.
///
/// `diff_no_index()` special-cases each before it stats anything
/// (diff-no-index.c:`get_mode`), so a queue built by stat alone gets all three
/// wrong — and `-` is the only input in this corpus that makes `diff` read
/// stdin at all.
fn inputs_that_are_not_files(out: &mut Vec<Case>) {
    for args in [
        &["diff", "--no-index", "/dev/null", "a.txt"][..],
        &["diff", "--no-index", "a.txt", "/dev/null"],
        &["diff", "--no-index", "--stat", "/dev/null", "a.txt"],
        &["diff", "--no-index", "/dev/null", "/dev/null"],
        // A directory against a file, both ways round.
        &["diff", "--no-index", "--name-status", "da", "a.txt"],
        &["diff", "--no-index", "--name-status", "a.txt", "da"],
    ] {
        ni(out, args);
    }

    // `-` reads stdin. Both sides are fed the same bytes, so the only variable
    // is what each does with the name.
    for args in [
        &["diff", "--no-index", "-", "a.txt"][..],
        &["diff", "--no-index", "a.txt", "-"],
        &["diff", "--no-index", "--stat", "-", "a.txt"],
    ] {
        out.push(
            Case::with_stdin("diff", args, Shape::NoIndexTrees, b"alpha\nbeta\ngamma\n")
                .in_dir("ni")
                .with_env(OUTSIDE),
        );
    }
    // The same, with content that differs from the file on disk.
    out.push(
        Case::with_stdin(
            "diff",
            &["diff", "--no-index", "-", "a.txt"],
            Shape::NoIndexTrees,
            b"alpha\nchanged\ngamma\n",
        )
        .in_dir("ni")
        .with_env(OUTSIDE),
    );
}

/// The refusals. Each is compared on stderr, because the message is the whole
/// behaviour: there is no stdout to be right about.
fn refusals(out: &mut Vec<Case>) {
    for args in [
        // One path, three paths: `--no-index` takes exactly two.
        &["diff", "--no-index", "a.txt"][..],
        &["diff", "--no-index", "a.txt", "b.txt", "da"],
        // A path that is not there, on each side.
        &["diff", "--no-index", "nosuch.txt", "a.txt"],
        &["diff", "--no-index", "a.txt", "nosuch.txt"],
        &["diff", "--no-index", "nosuch_a", "nosuch_b"],
        // `--cached` asks for an index that the mode does not have.
        &["diff", "--no-index", "--cached", "a.txt", "b.txt"],
        // A revision is not a path, and outside a repository it cannot become
        // one.
        &["diff", "--no-index", "HEAD", "a.txt"],
    ] {
        ni_strict(out, args);
    }
}
