//! One question — *are these two changes the same change?* — asked through the
//! four verbs that answer it, and the places where their answers must agree.
//!
//! `patch-id`, `cherry`, `range-diff` and `merge-tree` are four faces of one
//! notion of patch identity. `cherry` and `rev-list --cherry-*` classify a
//! commit by comparing its patch id against the upstream's; `range-diff` pairs
//! commits across two ranges by patch *similarity* and then prints `=` for the
//! pairs whose patches are byte-identical; `merge-tree` decides what a change
//! that is already present on the other side contributes to a merge; and
//! `patch-id` is the hash the first of those is built on, exposed directly.
//! A port can implement any one of them correctly and still hold four different
//! opinions about whether two commits are the same change — which is a defect no
//! single verb's stdout can show.
//!
//! # How this divides territory with the eight modules already here
//!
//! Every one of the four verbs already has cases. None of those cases measures a
//! *relation between two inputs*, which is what this module is for.
//!
//! * **`patch-id`**
//!   * [`super::mail_series`] owns the input *shapes* with no `@@` line — a mode
//!     change (`P_MODE`), a hunkless rename (`P_RENAME`), a combined `@@@` diff
//!     (`P_COMBINED`) — plus the two-message mailbox whose second column comes
//!     from each `From <oid>` line, a missing final newline, whitespace-only
//!     additions, and the empty input.
//!   * [`super::stdin_plumbing`] owns a single-commit diff, a rename, a binary
//!     block, the CRLF triple and a non-patch.
//!   * [`super::plumbing_objects`] owns the flag *parse*: the four spellings
//!     against a closed stdin, plus `--bogus` and a stray positional.
//!   * **Left over, and here:** every one of those measures *one* id. Nothing in
//!     the corpus measured two payloads against each other, so the properties
//!     that define the algorithm were all unreachable — the two file orders that
//!     `--stable` collapses and `--unstable` does not, the two hunk orders that
//!     *neither* collapses, the two context widths that change the id, the two
//!     whitespace renderings that only `--verbatim` separates, and a rename
//!     written as a rename against the same move written as a delete plus an
//!     add. See [`patch_id_equivalence_classes`] for the measured stock answers.
//! * **`cherry`**
//!   * [`super::fixture_gaps`] owns the plain forms on [`Shape::Cherry`] and
//!     [`Shape::Unrelated`]; [`super::history_query`] owns
//!     [`Shape::BehindRemote`], [`Shape::MergeableDirty`], [`Shape::Octopus`]
//!     and the empty-argument refusal; [`super::history_rewrite`] owns
//!     [`Shape::Branched`], [`Shape::Merged`] and the unknown ref;
//!     [`super::stdin_plumbing`] owns `--abbrev=4`/`--abbrev=40` on
//!     [`Shape::Branched`]; [`super::sequences`] owns `cherry` after a pick.
//!   * **Left over, and here:** the abbreviation width delivered by
//!     *configuration* rather than by `--abbrev` (no case sets `core.abbrev` for
//!     `cherry` anywhere), `--no-abbrev` and `--abbrev=0`, the upstream/head/limit
//!     positions moved off the branch tips, and [`Shape::CrissCross`] — the one
//!     shape where the two revisions have two merge bases, which `cherry`'s
//!     `<upstream>..<head>` walk has to handle without one.
//! * **`range-diff`**
//!   * [`super::diff_family`] owns the flag sweep on [`Shape::Branched`];
//!     [`super::history_query`] owns the only *matched* pair in the corpus
//!     (`div-cold`/`ff-cold` at `--creation-factor=100`) and the identical-pair
//!     `=` on [`Shape::Packed`]; [`super::misc_commands`] owns `--no-binary`;
//!     [`super::revision_syntax`] owns the bare `main...topic` on
//!     [`Shape::Cherry`] and `cc-left...cc-right` on [`Shape::CrissCross`].
//!   * **Left over, and here:** the output-format flags none of them names
//!     (`--raw`, `--name-only`, `--name-status`, `--check`, `--patch-with-raw` —
//!     all five refused by the port, see below), the three
//!     `--output-indicator-*` characters, `--diff-filter`, two ranges of
//!     *different lengths*, a range with a commit dropped off the end, a window
//!     slid by one so every pair matches at a shifted index, and `--notes`
//!     against a repository that actually has notes ([`Shape::NotesReplace`] —
//!     `diff_family`'s `--notes` case runs on [`Shape::Branched`], which has
//!     none, so it measures argument parsing).
//! * **`merge-tree`**
//!   * [`super::merge_family`] owns `--write-tree` on [`Shape::Conflicted`] and
//!     [`Shape::Branched`], the `merge.conflictStyle` sweep and the bad-ref
//!     refusals; [`super::history_query`] owns [`Shape::MergeableDirty`],
//!     `--merge-base=` on [`Shape::Octopus`] and the two-line `--stdin` payload;
//!     [`super::fixture_gaps`] owns [`Shape::Unrelated`] and `-X ours` on
//!     [`Shape::CrissCross`]; [`super::attributes_filters`] owns the merge-driver
//!     configuration.
//!   * **Left over, and here:** `--trivial-merge` *spelled out* — no curated case
//!     names it, and it selects an entirely different program (the old
//!     three-argument tree walker) from the one every existing case runs;
//!     `--quiet`, which is not merely `--write-tree` with stdout closed and which
//!     the port gets wrong (below); `-X theirs`; `--no-merge-base`; a
//!     `--merge-base=` that overrides a *real* ancestry to force a three-way
//!     merge of two commits that are linearly related, which is the only way this
//!     fixture set can put a rename on both sides of a merge; and the
//!     `<base> -- <b1> <b2>` form of `--stdin`.
//! * **`rebase --reapply-cherry-picks`** stays [`super::rebase_engine`]'s, and
//!   `cherry-pick` on [`Shape::Cherry`] stays [`super::fixture_gaps`]'s. The
//!   patch-id question underneath both is asked here through the four verbs
//!   directly, never by re-running either of those two.
//!
//! # The fixtures, and what each one can and cannot say
//!
//! * [`Shape::Cherry`] — the only fixture carrying one patch twice. `main` is
//!   `seed → shared patch → upstream only`; `topic` is `seed → topic base →
//!   shared patch (cherry-picked, new commit id) → topic only`. So the same
//!   change exists under two commit ids on two branches, which is what makes all
//!   four verbs answerable about one pair at once.
//! * [`Shape::CrissCross`] — two merge bases for one pair, so `cherry`'s walk,
//!   `range-diff`'s ranges and `merge-tree`'s base selection each face a
//!   question with no single answer.
//! * [`Shape::Renamed`] — four renames in *linear* history. With
//!   `--merge-base=main~4` forcing a base behind both sides, two commits that are
//!   ancestor and descendant become the two sides of a real three-way merge, and
//!   both sides then carry the same rename of `orig/alpha.txt`.
//! * [`Shape::Packed`] — seven revisions of one file, so two windows of the same
//!   history slid by one commit pair every commit at a shifted index.
//! * [`Shape::NotesReplace`] — the only shape with notes, which is what makes
//!   `range-diff --notes` measure anything.
//!
//! **What no fixture can express, and therefore is not measured here.** Both are
//! reported rather than approximated, because a case that cannot fail is worse
//! than no case:
//!
//! * **A reordered range.** `range-diff` renders a *crossing* — patches `A, B` on
//!   the left and `B, A` on the right — differently from a shifted match, and
//!   nothing in `fixture.rs` produces one: the file runs `cherry-pick` exactly
//!   once in the whole corpus (`fixture.rs:1783`), so no two branches carry the
//!   same two patches in opposite order. The nearest reachable thing is a range
//!   with a commit *inserted* ahead of the shared patch, which shifts a match
//!   without crossing it, and that is what the [`Shape::Cherry`] and
//!   [`Shape::Packed`] pairings below measure.
//! * **A split commit.** One commit on the left whose change is two commits on
//!   the right needs a branch built for it; no shape has a pair of commits whose
//!   union is another shape's single commit.
//! * **Renames to *different* paths on the two sides of a merge.** Every rename
//!   in the corpus is in linear history, so the forced-base construction below
//!   can put the *same* rename on both sides and never two competing ones.
//!
//! # What the state probe sees here, and the defect it found
//!
//! `merge-tree --write-tree` writes real objects into the object store, and the
//! probe's `cat-file --batch-check --batch-all-objects` census sees them. Checked
//! by hand rather than assumed — on [`Shape::CrissCross`], `merge-tree
//! --write-tree cc-a cc-b` leaves two new objects behind on both sides:
//!
//! ```text
//! fc1d612897852b4c8c2a5a6b74bb563cfa1acb8b tree 174
//! c5b90aef39de2dca86efcbfbc0b31ce98c9425dc blob 38
//! ```
//!
//! That sensitivity is what catches `--quiet`. Stock's `--quiet` is not
//! `--write-tree` with its stdout thrown away: it asks merge-ort for
//! *mergeability only*, which never merges content, so **stock writes no objects
//! at all**. The port runs the full merge and writes the merged blob, keeping
//! only the tree back — same stdout, same exit code, one extra object:
//!
//! ```text
//! $ git merge-tree --write-tree --quiet main topic     # Shape::Cherry
//! stock 2.55.0 → exit 0, no output, 0 new objects
//! zvcs         → exit 0, no output, 1 new object
//!                6f60e347bc68655fca9a74653c5e337a13765c2d blob 132
//! ```
//!
//! Three of the cases below are on that path ([`merge_tree_trivial_and_quiet`]),
//! one clean and two conflicting, so the defect is pinned in both outcomes.
//!
//! # A stock crash that is deliberately *not* a case
//!
//! `git merge-tree --write-tree --quiet cc-left cc-right` **segfaults** in stock
//! 2.55.0 *and* in 2.50.1 — reproduced by hand in a copy of
//! [`Shape::CrissCross`], exit status 139 both times — while this port exits 1
//! correctly. It needs all three of `--quiet`, a pair with *two* merge bases, and
//! a conflict; pinning the base with `--merge-base=main` gives a clean exit 1,
//! and `cc-a cc-b` (one base) does too.
//!
//! No case is written for it. The harness would classify the port's correct
//! answer as an exit difference against a signal-killed oracle, and the second
//! oracle would corroborate it as a defect, so the case would report the port
//! wrong for being right. `cc-a cc-b` is used for the conflicting `--quiet` case
//! instead, which reaches the same object-write defect without the crash.
//!
//! # Determinism
//!
//! Every case here was run twice against stock 2.55.0 in identical copies of its
//! fixture and compared on stdout, exit code, the object census and the ref list:
//! 97 repository cases and 32 `patch-id` stdin payload cases, zero disagreements.
//! The stdin payloads are literal diffs with literal blob ids, so nothing in them
//! is a function of a fixture; the repository cases name only branches, tags and
//! `~`/`^` offsets, never an object id, and the only abbreviation widths that
//! appear are ones a case sets itself.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    patch_id_equivalence_classes(out);
    cherry_upstream_matching(out);
    range_diff_output_modes(out);
    range_diff_pairings(out);
    merge_tree_trivial_and_quiet(out);
    merge_tree_forced_base(out);
    one_definition_four_verbs(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// Push one `patch-id` case per algorithm flag over one payload.
///
/// The payload is the subject and the flag is the axis, so the four spellings
/// are written once here rather than four times at each call site — and a call
/// site that only needs some of them passes only those.
fn pid(flags: &[&'static str], input: &'static [u8], out: &mut Vec<Case>) {
    for flag in flags {
        let args: Vec<&str> =
            if flag.is_empty() { vec!["patch-id"] } else { vec!["patch-id", flag] };
        out.push(Case::with_stdin("patch-id", &args, Shape::Linear, input));
    }
}

/// The four algorithm spellings, in the order they are measured below.
const ALGOS: &[&str] = &["", "--stable", "--unstable", "--verbatim"];

// ---------------------------------------------------------------------------
// patch-id: the equality relation
// ---------------------------------------------------------------------------

/// Pairs of payloads that describe the **same change** two ways, and one pair
/// that describes two different changes that look alike.
///
/// A single patch id proves nothing about the algorithm — any hash of the input
/// bytes reproduces it. What the algorithm *is* lives in which differences it
/// collapses, and every one of those is a statement about two payloads. Measured
/// against stock 2.55.0; ids truncated to twelve hex digits for reading, and each
/// row is one payload under one flag:
///
/// ```text
///                       default        --stable       --unstable     --verbatim
/// ORDER_AB              ace4f8fa01d5   5ecac8fbbc3f   ace4f8fa01d5   92b80d11b172
/// ORDER_BA              4638dbb2cc25   5ecac8fbbc3f   4638dbb2cc25   92b80d11b172
/// HUNKS_12              5ed28f2abfb0   5ed28f2abfb0   5ed28f2abfb0   73d60e5a0f89
/// HUNKS_21              5beb403fa2fc   5beb403fa2fc   5beb403fa2fc   d52f850c0e33
/// CTX_WIDE              3b48cbd024c8   3b48cbd024c8   3b48cbd024c8   c5939e8f9658
/// CTX_NARROW            37348a35afbf   37348a35afbf   37348a35afbf   d33e6ffa9a26
/// SPACED_TIGHT          8d5f8ecce00a   8d5f8ecce00a   8d5f8ecce00a   b008c166341b
/// SPACED_LOOSE          8d5f8ecce00a   8d5f8ecce00a   8d5f8ecce00a   10b3d279fd72
/// MOVE_AS_RENAME        65e71f81611b   65e71f81611b   65e71f81611b   07c1cacc5b97
/// MOVE_AS_ADD_DELETE    b34a2ac0207c   2bdaa77ea466   b34a2ac0207c   95c5845e6679
/// ```
///
/// Read down the pairs, that table is the specification:
///
/// * **File order is what `--stable` stabilizes, and the only thing.**
///   `ORDER_AB` and `ORDER_BA` are the same two file diffs in the two orders.
///   Under `--stable` they are one id; under the default `--unstable` they are
///   two. `--verbatim` also collapses them, which is not obvious from the name
///   and is exactly the kind of thing a reimplementation gets backwards.
/// * **Hunk order is not.** `HUNKS_12` and `HUNKS_21` are one file's two hunks in
///   the two orders, and *no* spelling collapses them — `--stable` sorts
///   per-file digests, not per-hunk ones. A port that "improved" `--stable` into
///   a full order-independent hash passes every existing case and fails these.
/// * **Context is hashed.** `CTX_WIDE` and `CTX_NARROW` are the identical
///   one-line edit at `-U3` and `-U1`. Every spelling gives a different id, so
///   `patch-id` is *not* a hash of the added and removed lines alone, and two
///   renderings of one commit's diff at two context widths are two different
///   patches to `cherry`.
/// * **Whitespace inside content is not**, until `--verbatim`. `SPACED_TIGHT`
///   and `SPACED_LOOSE` differ only in runs of spaces and one tab inside the
///   changed lines: one id under three spellings, two under `--verbatim`. This is
///   the pair that makes `--verbatim` mean something; `mail_series`'s single
///   whitespace payload can only show that *an* id comes out.
/// * **A move is not a move.** `MOVE_AS_RENAME` is a 60%-similar rename with its
///   hunk; `MOVE_AS_ADD_DELETE` is the same move written as a delete plus an add,
///   which is what the identical commit produces with rename detection off. The
///   ids differ under every spelling, so whether two commits are "the same
///   change" depends on the *renderer* that fed `patch-id` — and that is the
///   seam where `cherry` (which renders its own diffs) and a human piping
///   `git log -p` can legitimately disagree.
///
/// `TWICE` is the positive statement the other four make negatively: one stream,
/// two `commit` headers, two different blob ids in the two `index` lines, and one
/// patch id printed twice (`c6bcd6a366d3…`). That is `cherry`'s `-` marker
/// spelled out in the plumbing, and it is what [`one_definition_four_verbs`]
/// checks the other three verbs against.
///
/// `HEADLESS` and `TRUNCATED` are the two ways the stream can be malformed
/// without being prose: a hunk with no `diff --git` above it (stock prints
/// nothing and exits 0 — it is not an error) and a diff whose last line has no
/// terminator. Both are `strict`, because "no output, exit 0" is only a
/// meaningful answer if nothing was written to stderr either.
fn patch_id_equivalence_classes(out: &mut Vec<Case>) {
    pid(ALGOS, ORDER_AB, out);
    pid(ALGOS, ORDER_BA, out);

    pid(&["", "--stable", "--verbatim"], HUNKS_12, out);
    pid(&["", "--stable", "--verbatim"], HUNKS_21, out);

    pid(&["", "--verbatim"], CTX_WIDE, out);
    pid(&["", "--verbatim"], CTX_NARROW, out);

    pid(&["", "--verbatim"], SPACED_TIGHT, out);
    pid(&["", "--verbatim"], SPACED_LOOSE, out);

    pid(&["", "--stable"], MOVE_AS_RENAME, out);
    pid(&["", "--stable"], MOVE_AS_ADD_DELETE, out);

    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin("patch-id", &["patch-id"], Shape::Linear, HEADLESS)
    });
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin("patch-id", &["patch-id"], Shape::Linear, TRUNCATED)
    });
    pid(&["--verbatim"], TRUNCATED, out);
}

/// Two file diffs in one patch, `pair-a.txt` before `pair-b.txt`.
///
/// Deliberately two *different* edits rather than one repeated: two copies of one
/// change would hash the same in either order for the wrong reason.
const ORDER_AB: &[u8] = b"diff --git a/pair-a.txt b/pair-a.txt\n\
index 1111111..2222222 100644\n\
--- a/pair-a.txt\n\
+++ b/pair-a.txt\n\
@@ -1,3 +1,3 @@\n\
 alpha\n\
-beta\n\
+BETA\n\
 gamma\n\
diff --git a/pair-b.txt b/pair-b.txt\n\
index 3333333..4444444 100644\n\
--- a/pair-b.txt\n\
+++ b/pair-b.txt\n\
@@ -1,3 +1,3 @@\n\
 one\n\
-two\n\
+TWO\n\
 three\n";

/// [`ORDER_AB`]'s two file diffs swapped, byte for byte otherwise.
const ORDER_BA: &[u8] = b"diff --git a/pair-b.txt b/pair-b.txt\n\
index 3333333..4444444 100644\n\
--- a/pair-b.txt\n\
+++ b/pair-b.txt\n\
@@ -1,3 +1,3 @@\n\
 one\n\
-two\n\
+TWO\n\
 three\n\
diff --git a/pair-a.txt b/pair-a.txt\n\
index 1111111..2222222 100644\n\
--- a/pair-a.txt\n\
+++ b/pair-a.txt\n\
@@ -1,3 +1,3 @@\n\
 alpha\n\
-beta\n\
+BETA\n\
 gamma\n";

/// One file, two hunks, the earlier one first — as any diff generator writes it.
const HUNKS_12: &[u8] = b"diff --git a/hunks.txt b/hunks.txt\n\
index 1111111..2222222 100644\n\
--- a/hunks.txt\n\
+++ b/hunks.txt\n\
@@ -2,3 +2,3 @@ h line 1\n\
 h line 2\n\
-h line 3\n\
+h line 3 edited\n\
 h line 4\n\
@@ -7,3 +7,3 @@ h line 6\n\
 h line 7\n\
-h line 8\n\
+h line 8 edited\n\
 h line 9\n";

/// [`HUNKS_12`]'s two hunks swapped. Not a diff any generator emits, and that is
/// the point: `patch-id` accepts it, and what it does with it is the definition
/// of how far `--stable` reaches.
const HUNKS_21: &[u8] = b"diff --git a/hunks.txt b/hunks.txt\n\
index 1111111..2222222 100644\n\
--- a/hunks.txt\n\
+++ b/hunks.txt\n\
@@ -7,3 +7,3 @@ h line 6\n\
 h line 7\n\
-h line 8\n\
+h line 8 edited\n\
 h line 9\n\
@@ -2,3 +2,3 @@ h line 1\n\
 h line 2\n\
-h line 3\n\
+h line 3 edited\n\
 h line 4\n";

/// One line changed, printed with three lines of context on each side.
const CTX_WIDE: &[u8] = b"diff --git a/ctx.txt b/ctx.txt\n\
index 1111111..2222222 100644\n\
--- a/ctx.txt\n\
+++ b/ctx.txt\n\
@@ -2,5 +2,5 @@ c line 1\n\
 c line 2\n\
 c line 3\n\
-c line 4\n\
+c line 4 edited\n\
 c line 5\n\
 c line 6\n";

/// [`CTX_WIDE`]'s change with one line of context. Same commit, `-U1`.
const CTX_NARROW: &[u8] = b"diff --git a/ctx.txt b/ctx.txt\n\
index 1111111..2222222 100644\n\
--- a/ctx.txt\n\
+++ b/ctx.txt\n\
@@ -3,3 +3,3 @@ c line 2\n\
 c line 3\n\
-c line 4\n\
+c line 4 edited\n\
 c line 5\n";

/// `old value` becomes `new value`, single-spaced.
const SPACED_TIGHT: &[u8] = b"diff --git a/spaced.txt b/spaced.txt\n\
index 1111111..2222222 100644\n\
--- a/spaced.txt\n\
+++ b/spaced.txt\n\
@@ -1,3 +1,3 @@\n\
 keep\n\
-old value\n\
+new value\n\
 tail\n";

/// [`SPACED_TIGHT`] with three spaces in the removed line and a tab in the added
/// one. Nothing else differs, so the pair isolates the whitespace stripping that
/// `--verbatim` turns off.
const SPACED_LOOSE: &[u8] = b"diff --git a/spaced.txt b/spaced.txt\n\
index 1111111..2222222 100644\n\
--- a/spaced.txt\n\
+++ b/spaced.txt\n\
@@ -1,3 +1,3 @@\n\
 keep\n\
-old   value\n\
+new\tvalue\n\
 tail\n";

/// A move plus an edit, rendered as a rename: `similarity index 60%`, one hunk,
/// two different path names in the one `diff --git` header.
const MOVE_AS_RENAME: &[u8] = b"diff --git a/moved-from.txt b/moved-to.txt\n\
similarity index 60%\n\
rename from moved-from.txt\n\
rename to moved-to.txt\n\
index 1111111..2222222 100644\n\
--- a/moved-from.txt\n\
+++ b/moved-to.txt\n\
@@ -1,3 +1,3 @@\n\
 keep\n\
-old\n\
+new\n\
 tail\n";

/// [`MOVE_AS_RENAME`]'s change with rename detection off: the whole old file
/// deleted and the whole new one added. Two file diffs, so `--stable` and
/// `--unstable` differ from each other here as well.
const MOVE_AS_ADD_DELETE: &[u8] = b"diff --git a/moved-from.txt b/moved-from.txt\n\
deleted file mode 100644\n\
index 1111111..0000000\n\
--- a/moved-from.txt\n\
+++ /dev/null\n\
@@ -1,3 +0,0 @@\n\
-keep\n\
-old\n\
-tail\n\
diff --git a/moved-to.txt b/moved-to.txt\n\
new file mode 100644\n\
index 0000000..2222222\n\
--- /dev/null\n\
+++ b/moved-to.txt\n\
@@ -0,0 +1,3 @@\n\
+keep\n\
+new\n\
+tail\n";

/// `git log -p` over two commits carrying the same patch — the shape a human
/// actually pipes into `patch-id`, and the shape [`Shape::Cherry`] holds.
///
/// The two `index` lines name different blobs and the two `commit` lines
/// different commits, so a hash that reached the object ids would print two
/// different patch ids. Stock prints `c6bcd6a366d3…` twice, once per commit id in
/// the second column.
const TWICE: &[u8] = b"commit 6fca7005ce9a71b30b1f5b7e0d5e5f2c9b7f3a11\n\
Author: zvcs parity <parity@example.invalid>\n\
Date:   Tue Nov 14 22:13:20 2023 +0000\n\
\n\
    cherry: shared patch\n\
\n\
diff --git a/app.txt b/app.txt\n\
index 1111111..2222222 100644\n\
--- a/app.txt\n\
+++ b/app.txt\n\
@@ -1,5 +1,5 @@\n\
 app line 1\n\
 app line 2\n\
-app line 3\n\
+app line 3 edited\n\
 app line 4\n\
 app line 5\n\
\n\
commit 7a4b88a6917cd6570662128a5a27584666dd092e\n\
Author: zvcs parity <parity@example.invalid>\n\
Date:   Tue Nov 14 22:13:20 2023 +0000\n\
\n\
    cherry: shared patch\n\
\n\
diff --git a/app.txt b/app.txt\n\
index 3333333..4444444 100644\n\
--- a/app.txt\n\
+++ b/app.txt\n\
@@ -1,5 +1,5 @@\n\
 app line 1\n\
 app line 2\n\
-app line 3\n\
+app line 3 edited\n\
 app line 4\n\
 app line 5\n";

/// A hunk with no `diff --git` header above it. Stock reads it, finds no file to
/// attribute it to, prints nothing and exits 0.
const HEADLESS: &[u8] = b"@@ -1,3 +1,3 @@\n keep\n-old\n+new\n tail\n";

/// A diff whose final line has no newline terminator — what a truncated pipe
/// delivers. The last added line still counts.
const TRUNCATED: &[u8] = b"diff --git a/cut.txt b/cut.txt\n\
index 1111111..2222222 100644\n\
--- a/cut.txt\n\
+++ b/cut.txt\n\
@@ -1,2 +1,2 @@\n\
 keep\n\
-old\n\
+new";

// ---------------------------------------------------------------------------
// cherry
// ---------------------------------------------------------------------------

/// `cherry`'s three positional arguments and its abbreviation width, on the two
/// shapes where the answer is not a foregone conclusion.
///
/// The upstream/head/limit triple is one walk with three inputs, and every
/// existing case fixes two of them at branch tips. Moving them individually is
/// what separates "computes patch ids over `<upstream>..<head>`" from "diffs two
/// branch tips":
///
/// * `main~1 topic` — the upstream is *behind* the shared patch, so the commit
///   marked `-` under `main topic` becomes `+` here. One `~` changes the answer.
/// * `main topic~1` — the head stops before `topic only`; the shared patch is
///   still `-`.
/// * `topic topic` — upstream and head are the same ref: an empty walk, exit 0,
///   no output. The degenerate case a port is most likely to special-case wrong.
/// * `main topic main` — the *limit* is `main`, which is also the upstream.
/// * `HEAD~2 topic` — `HEAD` is `topic` in this shape, so the upstream is named
///   relative to the head rather than as a branch.
///
/// The abbreviation width is measured through `core.abbrev` rather than
/// `--abbrev` because no case anywhere sets that key for `cherry`, and the two
/// are different code paths: `--abbrev` is parsed by `builtin/log.c`'s option
/// table, `core.abbrev` by `git_default_config`. `--no-abbrev` and `--abbrev=0`
/// are the two ways to ask for the full id, and neither appears in the corpus.
///
/// [`Shape::CrissCross`] is here because a pair with two merge bases is a
/// question `cherry` has to answer without one — `cc-a cc-left` walks into a
/// merge commit, and `main cc-left cc-a` limits that walk with one of the two
/// bases. [`Shape::Merged`], [`Shape::Unrelated`] and [`Shape::Renamed`] round
/// out the shapes where a walk crosses a merge, crosses nothing, or crosses a
/// rename.
fn cherry_upstream_matching(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "cherry",
        &[
            &["cherry", "--no-abbrev", "-v", "main", "topic"],
            &["cherry", "--abbrev=0", "-v", "main", "topic"],
            &["cherry", "-v", "main~1", "topic"],
            &["cherry", "-v", "main", "topic~1"],
            &["cherry", "-v", "topic", "topic"],
            &["cherry", "-v", "main", "topic", "main"],
            &["cherry", "-v", "HEAD~2", "topic"],
        ],
        out,
    );

    for width in ["4", "12", "40"] {
        out.push(
            Case::new("cherry", &["cherry", "-v", "main", "topic"], Shape::Cherry)
                .with_config(&[("core.abbrev", width)]),
        );
    }

    each(
        Shape::CrissCross,
        "cherry",
        &[
            &["cherry", "-v", "cc-a", "cc-left"],
            &["cherry", "-v", "cc-b", "cc-left"],
            &["cherry", "-v", "cc-left", "cc-right"],
            &["cherry", "-v", "main", "cc-left", "cc-a"],
        ],
        out,
    );

    // A walk that crosses a merge commit, in both directions.
    each(
        Shape::Merged,
        "cherry",
        &[&["cherry", "-v", "main", "side"], &["cherry", "-v", "side", "main"]],
        out,
    );
    // Two roots with no ancestor at all: every commit on the head is `+`.
    out.push(Case::new("cherry", &["cherry", "-v", "alien", "alien-clash"], Shape::Unrelated));
    // A walk over commits whose diffs are renames, which `cherry` renders itself
    // and therefore decides the rename-detection setting of.
    out.push(Case::new("cherry", &["cherry", "-v", "main~2", "main"], Shape::Renamed));
}

// ---------------------------------------------------------------------------
// range-diff: how the comparison is printed
// ---------------------------------------------------------------------------

/// The output-format half of `range-diff`, on the one shape with a real match to
/// print.
///
/// `range-diff` embeds a whole `diff` inside each commit's block, so the diff
/// family's output selectors apply to it — and five of them are **refused by the
/// port**. Reproduced by hand in a copy of [`Shape::Cherry`]:
///
/// ```text
/// $ git range-diff --raw main...topic
/// -:  ------- > 1:  dabff09 cherry: topic base
/// 1:  6fca700 = 2:  7a4b88a cherry: shared patch
///     :100644 100644 0000000 0000000 M	a
/// 2:  b0db3a7 < -:  ------- cherry: upstream only
/// -:  ------- > 3:  d74c8d4 cherry: topic only
/// (exit 0)
///
/// $ zvcs range-diff --raw main...topic
/// fatal: unsupported flag "--raw"
/// (exit 128)
/// ```
///
/// `--name-only`, `--name-status`, `--check` and `--patch-with-raw` fail the same
/// way, with the flag's own name in the message. All five are in the fuzzer's
/// `range-diff` flag list (`grammars_generated.rs:549`) and none was in the
/// curated corpus, so they were only ever findable by a sampled run.
///
/// The rest of the group is measured and matching, and pins behaviour the corpus
/// had no case for:
///
/// * `--output-indicator-{new,old,context}` — the three characters that replace
///   `+`, `-` and the leading space. Asked once each and once together, because
///   `range-diff` has *two* layers of markers (its own `>`/`<`/`=`/`!` column and
///   the inner diff's) and a port that wires the option to the wrong layer
///   changes only one of them.
/// * `--diff-filter` — `M` keeps the modification, `A` keeps nothing, so the two
///   bracket whether the filter reaches the inner diff at all.
/// * `--creation-factor=0` and `--creation-factor=bogus` — the boundary value and
///   a non-integer. The second is `strict`: `error: option `creation-factor'
///   expects an integer value with an optional k/m/g suffix` names the option, and
///   both sides print it byte for byte.
/// * `--left-only --right-only` together — `strict`, and refused with
///   `error: options '--left-only' and '--right-only' cannot be used together`.
/// * `--right-only`, `--dual-color` and `--no-dual-color` on this shape.
///   `diff_family` has all three on [`Shape::Branched`], where nothing pairs, so
///   the colour-suppression flags had no dual-coloured output to suppress.
fn range_diff_output_modes(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "range-diff",
        &[
            // The five the port does not implement.
            &["range-diff", "--raw", "main...topic"],
            &["range-diff", "--name-only", "main...topic"],
            &["range-diff", "--name-status", "main...topic"],
            &["range-diff", "--check", "main...topic"],
            &["range-diff", "--patch-with-raw", "main...topic"],
            // The marker characters, singly and together.
            &["range-diff", "--output-indicator-new=Y", "main...topic"],
            &["range-diff", "--output-indicator-old=X", "main...topic"],
            &["range-diff", "--output-indicator-context=.", "main...topic"],
            &[
                "range-diff",
                "--output-indicator-new=Y",
                "--output-indicator-old=X",
                "--output-indicator-context=.",
                "main...topic",
            ],
            &["range-diff", "--diff-filter=M", "main...topic"],
            &["range-diff", "--diff-filter=A", "main...topic"],
            &["range-diff", "--creation-factor=0", "main...topic"],
            &["range-diff", "--right-only", "main...topic"],
            &["range-diff", "--dual-color", "main...topic"],
            &["range-diff", "--no-dual-color", "main...topic"],
        ],
        out,
    );

    // Two refusals whose message names the offending option, so stderr is the
    // behaviour rather than prose around it.
    out.push(Case::strict(
        "range-diff",
        &["range-diff", "--creation-factor=bogus", "main...topic"],
        Shape::Cherry,
    ));
    out.push(Case::strict(
        "range-diff",
        &["range-diff", "--left-only", "--right-only", "main...topic"],
        Shape::Cherry,
    ));
}

/// Which commit `range-diff` decides is which, when the two ranges are not the
/// same length and the matches do not line up.
///
/// The pairing is the algorithm; the rendering above is downstream of it. Every
/// pairing case in the corpus compares two ranges of *equal* length whose
/// commits either all match or none do, which is the one input where a
/// cost-matrix solver and a zip of two lists agree.
///
/// * [`Shape::Cherry`], `main~2..main` (two commits) against `topic~3..topic`
///   (three). The shared patch is the left range's first commit and the right
///   range's second, because `topic base` was inserted ahead of it — a match at
///   a shifted index, which is as close to a reordering as this fixture set gets
///   (see the module header for why a true crossing is not expressible).
/// * The same pair at equal length, `main~2..main` against `topic~2..topic`, so
///   the shift is removed and only the substitution is left.
/// * The three-argument form `main~2 main topic`, which expands to
///   `main~2..main` and `main~2..topic` — a two-commit range against a
///   four-commit one sharing a base, the widest length mismatch here.
/// * [`Shape::Renamed`], `main~4..main` against `main~4..main~1`: the identical
///   range with its last commit **dropped**. Both directions, because
///   `range-diff` is not symmetric — the dropped commit is `<` one way and `>`
///   the other — and `--left-only` on top, which must show only the side that
///   has it.
/// * [`Shape::Packed`], `main~3..main` against `main~4..main~1`: the same
///   three-commit window slid by one, so every pair matches at a shifted index
///   and one commit falls off each end.
/// * [`Shape::NotesReplace`], where `--notes` has notes to print. `diff_family`'s
///   `--notes` case runs on a shape with none. All three spellings, because
///   `--notes=<ref>` selects a *different* notes ref than the default and
///   `--no-notes` has to suppress what the default would have added.
/// * [`Shape::CrissCross`] `cc-a...cc-b` and [`Shape::Octopus`]
///   `main..oct-a main..oct-b`: two ranges whose commits share a base but no
///   patch, so the solver has to decline every pairing rather than force one.
fn range_diff_pairings(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "range-diff",
        &[
            &["range-diff", "main~2..main", "topic~3..topic"],
            &["range-diff", "main~2..main", "topic~2..topic"],
            &["range-diff", "main~2", "main", "topic"],
            &["range-diff", "--creation-factor=1", "main...topic"],
            &["range-diff", "-s", "main...topic"],
            &["range-diff", "--stat", "main...topic"],
            &["range-diff", "--abbrev=12", "main...topic"],
        ],
        out,
    );

    each(
        Shape::Renamed,
        "range-diff",
        &[
            &["range-diff", "main~4..main", "main~4..main~1"],
            &["range-diff", "main~4..main~1", "main~4..main"],
            &["range-diff", "--left-only", "main~4..main", "main~4..main~1"],
        ],
        out,
    );

    each(
        Shape::Packed,
        "range-diff",
        &[
            &["range-diff", "main~3..main", "main~4..main~1"],
            &["range-diff", "--right-only", "main~3..main", "main~4..main~1"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "range-diff",
        &[
            &["range-diff", "--notes", "main~2..main", "main~2..main"],
            &["range-diff", "--notes=refs/notes/review", "main~2..main", "main~2..main"],
            &["range-diff", "--no-notes", "main~2..main", "main~2..main"],
        ],
        out,
    );

    out.push(Case::new("range-diff", &["range-diff", "cc-a...cc-b"], Shape::CrissCross));
    out.push(Case::new(
        "range-diff",
        &["range-diff", "main..oct-a", "main..oct-b"],
        Shape::Octopus,
    ));
}

// ---------------------------------------------------------------------------
// merge-tree: the two programs behind one name
// ---------------------------------------------------------------------------

/// `--trivial-merge` and `--quiet`: the mode nothing names, and the flag that is
/// not what it looks like.
///
/// `git merge-tree` is two programs. `--write-tree` runs merge-ort over two
/// commits; `--trivial-merge` runs the original three-argument tree walker, which
/// merges *trees*, does no rename detection, and prints its own line-oriented
/// report instead of a tree id. Every existing case either passes `--write-tree`
/// or passes three arguments and lets the mode be inferred — **no case in the
/// corpus spells `--trivial-merge`**, so the option itself was never parsed and
/// the two ways of selecting the old mode were never checked against each other.
/// Seven shapes are asked here, including [`Shape::Symlinks`] (mode 120000 through
/// the walker) and [`Shape::Unrelated`] (a base that is an ancestor of neither
/// side).
///
/// The three refusals are `strict`, and all three messages match byte for byte
/// today:
///
/// ```text
/// merge-tree --trivial-merge --write-tree main~1 main feature
///   error: options '--write-tree' and '--trivial-merge' cannot be used together
/// merge-tree --trivial-merge main feature          # two arguments, old mode wants three
/// merge-tree --write-tree main feature extra       # three arguments, new mode wants two
///   usage: git merge-tree [--write-tree] [<options>] <branch1> <branch2>
///      or: git merge-tree [--trivial-merge] <base-tree> <branch1> <branch2>
/// ```
///
/// The arity pair is the point of the last two: each mode rejects exactly the
/// argument count the *other* one requires, so a port that dispatches on argument
/// count instead of on the flag accepts both and is caught here.
///
/// `--quiet` is documented as "suppress all output; only exit status wanted", and
/// a port that reads it that way — do the merge, drop stdout — passes on stdout
/// and exit code and still gets it wrong, because stock's `--quiet` asks merge-ort
/// for mergeability only and therefore **merges no content and writes no
/// objects**. Three cases are on that path and all three fail on the object
/// census alone: a clean merge on [`Shape::Cherry`], a conflicting one on
/// [`Shape::CrissCross`], and a conflicting merge of unrelated roots. Two more
/// (`main feature` on [`Shape::Branched`], `main alien` on [`Shape::Unrelated`])
/// match, and are kept as the controls: the first because its result tree already
/// exists in the repository so no write is observable either way, the second
/// because it refuses before merging anything.
///
/// The three `--quiet` incompatibilities are `strict` for the same reason as the
/// arity pair — `fatal: options '--quiet' and '--name-only' cannot be used
/// together` names both halves — and they are also what proves the matching
/// `--quiet` combinations above are matching for the right reason: a port that
/// accepted `--quiet --name-only` would have been agreeing on a refusal it never
/// made.
///
/// `merge-tree --write-tree --quiet cc-left cc-right` is **not** here; see the
/// module header for the stock segfault that makes it unmeasurable.
fn merge_tree_trivial_and_quiet(out: &mut Vec<Case>) {
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main~1", "main", "feature"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main^", "main", "theirs"],
        Shape::Conflicted,
    ));
    each(
        Shape::CrissCross,
        "merge-tree",
        &[
            &["merge-tree", "--trivial-merge", "cc-a", "cc-left", "cc-right"],
            &["merge-tree", "--trivial-merge", "main", "cc-a", "cc-b"],
        ],
        out,
    );
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main~2", "main", "topic"],
        Shape::Cherry,
    ));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main", "main", "sym-pending"],
        Shape::Symlinks,
    ));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main", "main", "alien"],
        Shape::Unrelated,
    ));

    // Mode selection and arity, all three refusals.
    out.push(Case::strict(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "--write-tree", "main~1", "main", "feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "merge-tree",
        &["merge-tree", "--trivial-merge", "main", "feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "merge-tree",
        &["merge-tree", "--write-tree", "main", "feature", "extra"],
        Shape::Branched,
    ));

    // `--quiet`: three that write an object the oracle does not, two controls.
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--quiet", "main", "topic"],
        Shape::Cherry,
    ));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--quiet", "cc-a", "cc-b"],
        Shape::CrissCross,
    ));
    each(
        Shape::Unrelated,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--quiet", "--allow-unrelated-histories", "main", "alien-clash"],
            &["merge-tree", "--write-tree", "--quiet", "main", "alien"],
        ],
        out,
    );
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--quiet", "main", "feature"],
        Shape::Branched,
    ));

    // What `--quiet` may not be combined with.
    for other in [&["--name-only"][..], &["--messages"][..], &["-z"][..]] {
        let mut args = vec!["merge-tree", "--write-tree", "--quiet"];
        args.extend_from_slice(other);
        args.extend_from_slice(&["main", "topic"]);
        out.push(Case::strict("merge-tree", &args, Shape::Cherry));
    }
}

/// A merge base chosen by hand, which is the only way this fixture set can put a
/// rename on both sides of a merge — plus the strategy options and `--stdin`
/// forms the corpus does not reach.
///
/// [`Shape::Renamed`]'s four renames are all in linear history, so no two
/// branches disagree about a path and `merge-tree` over it is always a
/// fast-forward. `--merge-base=` breaks that: it overrides the computed base, so
/// `--merge-base=main~4 main~3 main~2` merges two commits that *are* ancestor and
/// descendant as if they were siblings, over a base behind both. The result is a
/// genuine three-way merge in which both sides renamed `orig/alpha.txt` to
/// `moved/alpha.txt` and one side also renamed and edited `orig/beta.txt`, which
/// is merge-ort's rename-detection path and not reachable any other way here.
/// The base is varied (`main~3 main~2`, `main~3 main~1`, `main~1 main`,
/// `main~2 main`) so the number of renames each side carries changes, and
/// `-X no-renames` / `-X find-renames=90` / `merge.renames=false` /
/// `merge.renameLimit=1` turn the detection down from four directions — the last
/// two by configuration, which no `merge-tree` case delivers.
///
/// On [`Shape::CrissCross`] the base is *ambiguous* rather than absent, and the
/// three ways of resolving that are measured against each other: let merge-ort
/// build the virtual base (`--no-merge-base`), or pin either of the two real ones
/// (`--merge-base=cc-a`, `--merge-base=cc-b`). `-X theirs` is here because
/// `fixture_gaps` has `-X ours` on the same pair and the two are not mirror
/// images — the file whose stage-1 blob exists in no commit resolves to a
/// different side. `merge.conflictStyle` is measured *with* `--messages`;
/// `merge_family`'s sweep runs without it, so the conflict report that the style
/// changes was never printed beside the tree id.
///
/// `--merge-base=v0.2.0` on [`Shape::Branched`] passes an *annotated tag*, which
/// has to be peeled before it can be a tree; `--merge-base=nope` is the refusal,
/// `strict`, and `fatal: could not parse as tree 'nope'` matches byte for byte.
///
/// The `--stdin` cases add the `<base> -- <branch1> <branch2>` form —
/// `history_query`'s payload uses only the bare pair form — plus `-z`,
/// `--name-only` and `--quiet` layered on top of it, each of which changes the
/// per-line record rather than the whole stream.
fn merge_tree_forced_base(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--merge-base=main~4", "main~3", "main~2"],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=main~4", "main~3", "main~2"],
            &["merge-tree", "--write-tree", "--name-only", "--merge-base=main~4", "main~3", "main~1"],
            &["merge-tree", "--write-tree", "--merge-base=main~4", "main~1", "main"],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=main~4", "main~2", "main"],
            &["merge-tree", "--write-tree", "-X", "no-renames", "--merge-base=main~4", "main~3", "main~2"],
            &["merge-tree", "--write-tree", "-X", "find-renames=90", "--merge-base=main~4", "main~3", "main~1"],
        ],
        out,
    );
    for (key, value) in [("merge.renames", "false"), ("merge.renameLimit", "1")] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "--merge-base=main~4", "main~3", "main~1"],
                Shape::Renamed,
            )
            .with_config(&[(key, value)]),
        );
    }

    each(
        Shape::CrissCross,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "-X", "theirs", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "--no-merge-base", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "--merge-base=cc-a", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "--merge-base=cc-b", "cc-left", "cc-right"],
        ],
        out,
    );
    for style in ["diff3", "zdiff3"] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "cc-left", "cc-right"],
                Shape::CrissCross,
            )
            .with_config(&[("merge.conflictStyle", style)]),
        );
    }

    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--merge-base=v0.2.0", "main", "feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "merge-tree",
        &["merge-tree", "--write-tree", "--merge-base=nope", "main", "feature"],
        Shape::Branched,
    ));

    // `--stdin`: the base-bearing form, then the three record modifiers over the
    // bare-pair form.
    out.push(Case::with_stdin(
        "merge-tree",
        &["merge-tree", "--stdin"],
        Shape::CrissCross,
        b"cc-a -- cc-left cc-right\nmain -- cc-a cc-b\n",
    ));
    for extra in [&["-z"][..], &["--name-only"][..], &["--quiet"][..]] {
        let mut args = vec!["merge-tree", "--stdin"];
        args.extend_from_slice(extra);
        out.push(Case::with_stdin(
            "merge-tree",
            &args,
            Shape::CrissCross,
            b"cc-left cc-right\ncc-a cc-b\nmain cc-left\n",
        ));
    }
}

// ---------------------------------------------------------------------------
// The consistency question
// ---------------------------------------------------------------------------

/// One pair of branches, four verbs, one answer.
///
/// On [`Shape::Cherry`] the commits `main~1` and `topic~1` are the same change
/// under two commit ids. Every verb here has to say so, in its own vocabulary,
/// and the value of grouping them is that a *disagreement between two of these
/// lines* is a stronger finding than any one line's stdout — it says the port
/// holds two definitions of patch identity, which no amount of per-verb
/// correctness can be. Stock's four answers, measured:
///
/// ```text
/// $ git patch-id < TWICE                      # the two commits' diffs, piped
/// c6bcd6a366d3f44700ea1bc5ff75dede5af6908a 6fca7005ce9a71b30b1f5b7e0d5e5f2c9b7f3a11
/// c6bcd6a366d3f44700ea1bc5ff75dede5af6908a 7a4b88a6917cd6570662128a5a27584666dd092e
///                                             ^ one id, two commits
///
/// $ git cherry -v main topic
/// + dabff09104470c435bb10a5c58e1d78248982323 cherry: topic base
/// - 7a4b88a6917cd6570662128a5a27584666dd092e cherry: shared patch
/// + d74c8d447e7b33535f0a5750670168e3b259ea79 cherry: topic only
///   ^ `-` is "upstream already has this patch"
///
/// $ git range-diff --creation-factor=100 main...topic
/// -:  ------- > 1:  dabff09 cherry: topic base
/// 1:  6fca700 = 2:  7a4b88a cherry: shared patch
/// 2:  b0db3a7 < -:  ------- cherry: upstream only
/// -:  ------- > 3:  d74c8d4 cherry: topic only
///   ^ `=` is "these two commits' patches are identical"
/// ```
///
/// The `merge-tree` lines are the fourth vocabulary: the merge of two branches
/// that share a patch must contribute that patch once, so `main topic` and
/// `topic main` produce the same tree with no conflict report, and
/// `--merge-base=main~2` — a base *behind* the shared patch, so both sides
/// "added" it — must still merge it cleanly rather than reporting an add/add
/// collision. A port whose merge treats the duplicated patch as two independent
/// additions produces a conflict here while `cherry` still prints `-`, and that
/// contradiction is what this group exists to surface.
///
/// The argvs are chosen not to repeat the ones `fixture_gaps` (`cherry main
/// topic`, the `rev-list --cherry-*` family) and `revision_syntax` (`range-diff
/// main...topic`) already own on this shape; `--abbrev=40` and
/// `--creation-factor=100` are the spellings that make the two ids directly
/// comparable with the `patch-id` output above.
fn one_definition_four_verbs(out: &mut Vec<Case>) {
    pid(&["", "--stable", "--verbatim"], TWICE, out);

    each(
        Shape::Cherry,
        "cherry",
        &[&["cherry", "-v", "--abbrev=40", "main", "topic"]],
        out,
    );
    each(
        Shape::Cherry,
        "range-diff",
        &[&["range-diff", "--creation-factor=100", "main...topic"]],
        out,
    );
    each(
        Shape::Cherry,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--messages", "main", "topic"],
            &["merge-tree", "--write-tree", "--messages", "topic", "main"],
            &["merge-tree", "--write-tree", "--name-only", "main", "topic"],
            &["merge-tree", "--write-tree", "--merge-base=main~2", "main", "topic"],
        ],
        out,
    );
}
