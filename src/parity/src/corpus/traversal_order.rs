//! The revision walk itself: **which commits come back, and in what order**.
//!
//! `log`, `rev-list`, `shortlog`, `show-branch` and `format-patch` are five
//! front ends over one file. `revision.c` parses the walk options, builds the
//! pending set, runs the traversal and hands each front end a commit list; the
//! front end only decides how to print it. So a defect in the walk shows up
//! five times and reads as five unrelated findings, and — more usefully — a
//! defect in **one front end's option table** shows up as two front ends
//! disagreeing with each other about the same walk while stock agrees with
//! itself. That second shape is what this module is built to catch, and it
//! found it seven times over — see "What the port gets wrong" below, where six
//! of the seven items are one front end contradicting another.
//!
//! # How this divides territory with the six adjacent modules
//!
//! Every one of them was read before a case was written here.
//!
//! * **`history_query.rs`** owns `rev-list`'s selection flags on
//!   [`Shape::Octopus`], [`Shape::BehindRemote`] and [`Shape::MergeableDirty`]
//!   — `--boundary`, `--first-parent --boundary`, `--min-parents=3`,
//!   `--merges`, `--ancestry-path=oct-a`, `--topo-order`,
//!   `--author-date-order`, `--reverse --topo-order`, `--no-walk`, the
//!   `--left-right`/`--cherry-mark`/`--cherry-pick` set against `origin/div`,
//!   and the `--exclude=`/`--glob=`/`--branches=`/`--tags=` scope group. It
//!   also owns `merge-base`, `name-rev`, `range-diff`, `cherry`, `shortlog`'s
//!   grouping and `merge-tree`. Its rev-list block never runs on
//!   [`Shape::CrissCross`], [`Shape::CommitGraph`] or [`Shape::Cherry`], never
//!   crosses two ordering flags with a selection flag, and never asks a second
//!   front end the same question. No argv below is repeated from it.
//! * **`history_simplification.rs`** owns the six simplification modes —
//!   `--full-history`, `--dense`, `--sparse`, `--simplify-merges`,
//!   `--ancestry-path` *with a pathspec*, `--show-pulls` — plus `--follow` and
//!   the `--graph` rows a *simplified* walk produces. Simplification is
//!   switched on by a pathspec; **no case in this file passes a pathspec**, so
//!   nothing here enters `simplify_commit()` at all. The two files meet only at
//!   `--first-parent`, which that module uses as an input to simplification and
//!   this one uses as an ordering and selection question with no path involved.
//! * **`log_format.rs`** owns rendering: the pretty-format atoms, `--date=`,
//!   decoration, the diff presentation layer. Every case here that names a
//!   format uses `--oneline` (or `--format=%h%d` once, where the *decoration*
//!   is the reflog walk's evidence), because the answer under test is the list
//!   and never the layout.
//! * **`revision_syntax.rs`** owns the rev-spec *grammar* — `^`, `~`, `@{n}`,
//!   `:/text`, `^{}`, the ambiguity rules. This file spells every revision in
//!   the plainest form the walk will accept (`cc-left..cc-right`,
//!   `main...topic`, `^main`) and never asks what a spelling resolves to.
//! * **`naming_ancestry.rs`** owns `describe`/`name-rev`/`show-branch`/
//!   `merge-base`, and it states that **every one of its `show-branch` cases
//!   runs on [`Shape::CrissCross`]**. So `show-branch` appears nowhere below,
//!   in any spelling, even though it is a fifth front end on this walk.
//! * **`bisect_replay.rs`** owns `--bisect`, `--bisect-vars` and `--bisect-all`
//!   as the bisection *algorithm*, on [`Shape::Packed`], [`Shape::AmbiguousRef`]
//!   and [`Shape::TagChain`], including `--bisect-all --reverse`. What is left
//!   is how the walk options change the interval a bisection is taken over, and
//!   that needs a DAG rather than a nine-commit line: [`bisect_over_a_dag`]
//!   crosses `--bisect` with `--first-parent`, `--topo-order` and `--no-merges`
//!   on [`Shape::CrissCross`] and [`Shape::Octopus`], which that module never
//!   touches. `--bisect-vars` and `--bisect-all` are deliberately **not**
//!   re-spelled here: both exit 129 on the port everywhere, and that is already
//!   its finding.
//! * **`fixture_gaps.rs`** owns the `--cherry-mark`/`--cherry-pick`/`--cherry`
//!   set on [`Shape::Cherry`] — eight `rev-list` forms, four `log` forms, three
//!   `format-patch` forms. Every one of them is plain. [`revision_marks`] adds
//!   the one axis it has none of: the same flags **under `--graph`**, where the
//!   mark replaces the `*` glyph instead of being a column of its own.
//!
//! # Whether the ordering axis is measurable at all
//!
//! It is, but only half of it, and the reason is worth stating because it is a
//! property of every fixture rather than of any case.
//!
//! `fixture.rs`'s `git()` helper runs every construction command under
//! `env::harden`, which pins `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE` to
//! `env::FIXED_DATE` = `1700000000 +0000`. Nothing in `fixture.rs` overrides
//! either variable or passes `--date=` to a commit. So **every commit in every
//! shape carries one timestamp, in both the author and the committer field**,
//! verified across `Merged`, `CrissCross`, `Octopus`, `Branched`, `Cherry`,
//! `CommitGraph` and `Unrelated` with `log --all --format='%ad|%cd' --date=raw`.
//!
//! Two consequences, in opposite directions:
//!
//! * **`--date-order` and `--author-date-order` are indistinguishable from each
//!   other, in every shape, and no case can separate them.** They sort on two
//!   fields that hold the same value. Both are still measured below, against
//!   the *other* orders, because a port that implements one of them and aliases
//!   the other to the default is caught by that; a port that swaps the two is
//!   not, and cannot be until a fixture commits with a skewed `--date=`. That
//!   is a shape change, not a case, and it is recorded here rather than papered
//!   over. `history_query.rs`'s header already notes the degeneracy for
//!   `show-branch`; this is the same fact stated for the whole walk.
//! * **The other three orders are *not* degenerate.** With every date equal the
//!   default order falls back to `prio_queue`'s insertion-order tiebreak while
//!   `--topo-order` runs the in-degree pass and `--date-order` runs the
//!   date-sorted queue *with* the topological constraint, and the three
//!   algorithms disagree. Stock 2.55.0 on [`Shape::CrissCross`], `--all`:
//!
//!   | # | default | `--topo-order` | `--date-order` |
//!   |---|---------|----------------|----------------|
//!   | 1 | `a`             | `cc-left tip`   | `cc-left tip`   |
//!   | 2 | `b`             | `cc-left merge` | `cc-right tip`  |
//!   | 3 | `cc-left tip`   | `cc-right tip`  | `cc-left merge` |
//!   | 4 | `cc-right tip`  | `cc-right merge`| `cc-right merge`|
//!   | 5 | `base`          | `a`             | `b`             |
//!   | 6 | `cc-left merge` | `b`             | `a`             |
//!   | 7 | `cc-right merge`| `base`          | `base`          |
//!   | 8 | `initial`       | `initial`       | `initial`       |
//!
//!   [`Shape::Octopus`] separates all three as well. A port that ignores the
//!   ordering flags entirely, or that implements one of them as another, is
//!   caught on both shapes without any date ever moving.
//!
//! # Which shape supplies which topology
//!
//! * [`Shape::CrissCross`] — two branches that each merged the other. The only
//!   shape where a merge's two parents were committed in an order that is not
//!   the order the walk emits them, which is what makes the three orderings
//!   above differ; the only one where `^<merge>` on the excluded side gives
//!   `--exclude-first-parent-only` something to keep
//!   (`cc-left..cc-right` prints two commits without it and three with);
//!   and the only one whose reflog holds thirteen entries including two
//!   `commit (merge)` and three `checkout: moving from`.
//! * [`Shape::Octopus`] — a four-parent merge with a fork left unmerged beside
//!   it. Second and later parents exist, so `--first-parent` drops three
//!   commits instead of one and the `--graph` rows have lanes to expand.
//! * [`Shape::CommitGraph`] — the shape that broke the port. A merge whose
//!   *second* parent (`cg-side`) is a branch tip, so it is in `--all`'s pending
//!   set **and** unreachable by first-parent from the tip that names it. That
//!   is the exact input `--first-parent --topo-order` disagrees on (see
//!   [`first_parent_ordering`]), and no other shape has it: on `Merged` and
//!   `Octopus` no second parent carries its own ref.
//! * [`Shape::Cherry`] — one patch on both sides of a fork, needed for the
//!   `=` mark under `--graph`.
//! * [`Shape::Packed`] — nine commits and an **expired** reflog
//!   (`fixture.rs` runs `reflog expire --expire=all --all`, leaving
//!   `.git/logs/HEAD` and `.git/logs/refs/heads/main` at zero bytes). The only
//!   shape where `--reflog` contributes nothing to the pending set, which is
//!   the input that separates `log`'s HEAD fallback from `rev-list`'s refusal.
//!
//! # Determinism
//!
//! Every one of the 124 argvs below was run twice against stock git 2.55.0 in
//! two independent copies of its shape and byte-compared; all 124 agreed. Three specific
//! hazards were checked rather than assumed:
//!
//! * **No wall clock.** `--min-age`/`--max-age` take a raw epoch and are
//!   compared against the pinned `1700000000`, so `--max-age=1699999999`
//!   selects all eight commits and `--max-age=1700000001` selects none —
//!   forever, on any machine. `--since`/`--until` are approxidate and are used
//!   **only** in the `@<epoch>` spelling, which `parse_date` resolves without
//!   consulting `time(NULL)`. No relative spelling (`2 weeks ago`, `yesterday`)
//!   appears anywhere in this file.
//! * **`-g`/`--walk-reflogs` renders no relative date here.** The `HEAD@{n}`
//!   selector git prints under `-g` is a *count*, not a date; the date form
//!   (`HEAD@{2 days ago}`) appears only when the caller asked for one. The
//!   medium format's `Date:` line is the pinned stamp. Checked by reading the
//!   output: `log -g --oneline HEAD` prints
//!   `9efbd8f HEAD@{0}: checkout: moving from cc-right to cc-left` and twelve
//!   more like it.
//! * **No absolute path and no environment.** No case here sets a variable, so
//!   nothing can collide with `env::harden`'s pins.
//!
//! # What the port gets wrong (stock 2.55.0 and 2.50.1 agree on every one)
//!
//! 1. **`--first-parent` corrupts the topological order.** On
//!    [`Shape::CommitGraph`], `rev-list --first-parent --topo-order --all`
//!    emits `cg-side` fourth where both gits emit it second. Same wrong list
//!    from `log`, `shortlog` and `--graph`, so the defect is in the walk and
//!    not in a front end.
//! 2. **`rev-list --skip=<n>` exits 129** — the flag is absent from its option
//!    table. `log --skip=` and `shortlog --skip=` both work, so the same walk
//!    answers two ways depending on which binary name asked.
//! 3. **`--exclude-first-parent-only` is unimplemented in three different
//!    ways.** `rev-list` exits 129 (unknown option), `log` exits 1
//!    (`unsupported flag`), and `shortlog` exits 1 with a *specific* refusal:
//!    ``--exclude-first-parent-only` across a merge in the excluded history is
//!    not ported`.
//! 4. **`rev-list -g`/`--walk-reflogs` exits 129; `log -g` works.**
//! 5. **`--reflog` with an empty reflog goes the wrong way on both front
//!    ends.** On [`Shape::Packed`] both gits make `rev-list --reflog` exit 129
//!    (nothing pending, and `rev-list` demands a commit) while `log --reflog`
//!    falls back to `HEAD` and prints nine commits. The port does the reverse of
//!    each: `rev-list --reflog` exits 0 printing nothing, `log --reflog` exits 0
//!    printing nothing.
//! 6. **`--graph` drops the revision mark.** `log --graph --left-right`,
//!    `--graph --cherry-mark` and `--graph --cherry` print `<`, `>` and `=` in
//!    the glyph column on both gits; the port prints `*`. `--graph --boundary`'s
//!    `o` is correct, so it is the mark lookup and not the glyph column.
//! 7. **Six walk options are missing from one front end's table each.**
//!    `log --single-worktree`, `log --alternate-refs`, `log --count`,
//!    `log --bisect`, `log --objects`, `log --unpacked` exit 1 on the port and 0
//!    on both gits; `rev-list --graph` exits 129 on the port and renders a graph
//!    on both gits.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ordering_axis(out);
    first_parent_ordering(out);
    shape_selection(out);
    count_and_skip(out);
    reflog_walk(out);
    age_window(out);
    walk_scope(out);
    graph_orders(out);
    revision_marks(out);
    front_end_split(out);
    bisect_over_a_dag(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// The four orders and `--reverse`, on the two shapes that separate them.
///
/// The table in the module header is what this group pins. `--date-order` and
/// `--author-date-order` are both present and are expected to be byte-identical
/// to each other forever, for the reason given there; they are kept because
/// each still has to differ from the default and from `--topo-order`, and a
/// port that aliased either to the default is caught here.
///
/// `--reverse` is crossed with the orders rather than measured alone, because
/// reversing is applied *after* the sort and a port that reverses the emission
/// instead of the sorted list produces the right answer on the default order
/// and the wrong one on `--topo-order`.
fn ordering_axis(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--all"],
            // `rev-list --topo-order --all` on this shape is `fixture_gaps.rs`'s
            // (its `criss_cross` block). The topological leg is taken here
            // through `--reverse` and through `log`, which it does not have.
            &["rev-list", "--date-order", "--all"],
            &["rev-list", "--author-date-order", "--all"],
            &["rev-list", "--reverse", "--all"],
            &["rev-list", "--topo-order", "--reverse", "--all"],
            &["rev-list", "--date-order", "--reverse", "--all"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--all"],
            &["log", "--oneline", "--topo-order", "--all"],
            &["log", "--oneline", "--date-order", "--all"],
            &["log", "--oneline", "--author-date-order", "--all"],
            &["log", "--oneline", "--reverse", "--all"],
        ],
        out,
    );

    // The same four orders where the merge has four parents. `--topo-order`
    // emits the parents right-to-left (`oct-c`, `oct-b`, `oct-a`) and
    // `--date-order` left-to-right, which is the clearest single row in the
    // corpus separating the two.
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--all"],
            &["rev-list", "--date-order", "--all"],
            &["rev-list", "--reverse", "--all"],
            // `history_query.rs` owns `--topo-order --all` and
            // `--author-date-order --all` on this shape. Both legs are still
            // taken, crossed with `--reverse`, which it does not have.
            &["rev-list", "--topo-order", "--reverse", "--all"],
            &["rev-list", "--author-date-order", "--reverse", "--all"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--all"],
            &["log", "--oneline", "--date-order", "--all"],
        ],
        out,
    );

    each(
        Shape::Cherry,
        "rev-list",
        &[
            &["rev-list", "--topo-order", "--all"],
            &["rev-list", "--date-order", "--all"],
            &["rev-list", "--reverse", "--all"],
        ],
        out,
    );
}

/// `--first-parent` crossed with each order — the group that found the defect.
///
/// [`Shape::CommitGraph`] is the only shape where a merge's **second** parent
/// carries its own branch ref (`cg-side` at `commit-graph: side`). `--all` puts
/// that tip in the pending set, and `--first-parent` says the walk may not
/// reach it *through* the merge — so the sort has to place a commit that is in
/// the set and has no first-parent path from the head that names the merge.
///
/// Both gits agree on where it goes. Stock 2.55.0 and 2.50.1,
/// `rev-list --first-parent --topo-order --all`, abbreviated:
///
/// ```text
/// 8df13ef  commit-graph: loose fork
/// 0198521  commit-graph: side          <- second
/// 0687895  commit-graph: after the write
/// 011cb96  commit-graph: merge side
/// 333b6d9 … 60ad1e7 … 8215c26 … 0d223e8 … 2ca90a6 … edfab1b
/// ```
///
/// The port emits `0198521` **fourth**, after the merge that it is a parent of:
///
/// ```text
/// 8df13ef  commit-graph: loose fork
/// 0687895  commit-graph: after the write
/// 011cb96  commit-graph: merge side
/// 0198521  commit-graph: side          <- fourth
/// …
/// ```
///
/// Without `--first-parent` the port's `--topo-order` is correct, and without
/// `--topo-order` its `--first-parent` is correct; only the pair is wrong.
/// `log`, `shortlog`, `format-patch` and `--graph` are asked the same walk so
/// the finding is attributable to `revision.c` rather than to `builtin/log.c`.
fn first_parent_ordering(out: &mut Vec<Case>) {
    each(
        Shape::CommitGraph,
        "rev-list",
        &[
            // The two halves that are individually right, so the report shows
            // the pair failing between two passes rather than in isolation.
            &["rev-list", "--all"],
            &["rev-list", "--topo-order", "--all"],
            &["rev-list", "--date-order", "--all"],
            &["rev-list", "--reverse", "--topo-order", "--all"],
            &["rev-list", "--first-parent", "--all"],
            // The pair.
            &["rev-list", "--first-parent", "--topo-order", "--all"],
            &["rev-list", "--first-parent", "--date-order", "--all"],
            &["rev-list", "--first-parent", "--author-date-order", "--all"],
            &["rev-list", "--first-parent", "--reverse", "--topo-order", "--all"],
        ],
        out,
    );

    // The same walk through three more front ends. All four must move together;
    // if one ever passes while the others fail, the defect moved out of the walk.
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--first-parent", "--topo-order", "--all"],
        Shape::CommitGraph,
    ));
    out.push(Case::new(
        "log",
        &["log", "--graph", "--oneline", "--first-parent", "--all"],
        Shape::CommitGraph,
    ));
    out.push(Case::new(
        "shortlog",
        &["shortlog", "--first-parent", "--topo-order", "--all"],
        Shape::CommitGraph,
    ));
    out.push(Case::new(
        "format-patch",
        &["format-patch", "--stdout", "--no-signature", "--first-parent", "cg-loose..main"],
        Shape::CommitGraph,
    ));
}

/// Selection by the *shape* of the commit: how many parents it has, and which
/// of them the walk is allowed to follow.
///
/// `--merges` and `--no-merges` are aliases for `--min-parents=2` and
/// `--max-parents=1`, so the same question is asked in both spellings on a
/// shape with two merges: a port that implements the aliases separately from
/// the bounds gets one pair right and the other wrong. `--no-min-parents
/// --no-max-parents` before a real `--min-parents=` is the reset path: a port
/// that treats the negations as flags rather than as assignments to the same
/// two fields answers the un-reset question.
///
/// `--exclude-first-parent-only` needs a **merge on the excluded side** to do
/// anything at all — it changes how `^<rev>` is expanded, not how the positive
/// side is walked. Checked by hand: on `^main`, `main..HEAD`, `^cc-a cc-left`
/// and `^oct-a HEAD` the flag changes nothing, because none of those excluded
/// tips is a merge. `cc-left..cc-right` is the discriminator — stock prints
/// `cc-right tip` and `cc-right merge` without it and adds `criss-cross: b`
/// with it, because `^cc-left` stops following `cc-left`'s second parent.
fn shape_selection(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            // `--min-parents=2 --all` alone is `fixture_gaps.rs`'s. The pair
            // that pins both bounds at once is not.
            &["rev-list", "--min-parents=2", "--max-parents=2", "--all"],
            &["rev-list", "--max-parents=1", "--all"],
            &["rev-list", "--max-parents=0", "--all"],
            &["rev-list", "--min-parents=1", "--max-parents=1", "--all"],
            &["rev-list", "--merges", "--topo-order", "--all"],
            &["rev-list", "--no-merges", "--topo-order", "--all"],
            &["rev-list", "--no-min-parents", "--no-max-parents", "--min-parents=2", "--all"],
            &["rev-list", "--first-parent", "--topo-order", "--all"],
            &["rev-list", "--first-parent", "cc-left"],
            // `--boundary` prints the excluded commits the walk stopped at,
            // prefixed `-`. Crossed with an order and with `--first-parent`,
            // because the boundary set is emitted after the walk and a port that
            // appends it unsorted passes the plain form.
            &["rev-list", "--boundary", "--topo-order", "cc-right..cc-left"],
            &["rev-list", "--boundary", "--first-parent", "cc-right..cc-left"],
            &["rev-list", "--exclude-first-parent-only", "cc-left..cc-right"],
            &["rev-list", "--exclude-first-parent-only", "--topo-order", "cc-right..cc-left"],
        ],
        out,
    );

    // The same flag through the other two front ends: three different refusals
    // on the port for one option (129 / 1 / 1-with-a-named-message).
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--exclude-first-parent-only", "cc-left..cc-right"],
        Shape::CrissCross,
    ));
    out.push(Case::new(
        "shortlog",
        &["shortlog", "--exclude-first-parent-only", "cc-left..cc-right"],
        Shape::CrissCross,
    ));
    // With no `^` in the argument list the flag is inert on stock. Kept so the
    // port cannot pass the group by rejecting the flag only where it matters.
    out.push(Case::new(
        "rev-list",
        &["rev-list", "--exclude-first-parent-only", "--all"],
        Shape::Octopus,
    ));
}

/// `--max-count`, `-n`, `--skip=` and `--count`, and the order they compose in.
///
/// `--skip=<n>` drops the first `n` commits *after* the sort and *before*
/// `--max-count`, so `--skip=2 --max-count=2 --topo-order` is the only form
/// that can tell a port that applies them in the other order from one that
/// does not. `--skip=0` is the identity case: a port that treats any `--skip`
/// as "drop something" is caught by it.
///
/// `--count --skip=1` is the interaction the counter has to survive — the count
/// is of what survived the skip, not of the whole walk.
fn count_and_skip(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--max-count=3", "--topo-order", "--all"],
            &["rev-list", "-n2", "--reverse", "--all"],
            &["rev-list", "--skip=2", "--all"],
            &["rev-list", "--skip=0", "--all"],
            &["rev-list", "--skip=2", "--max-count=2", "--topo-order", "--all"],
            &["rev-list", "--count", "--skip=1", "--all"],
        ],
        out,
    );
    // `--skip=` through `log` and `shortlog`, which the port accepts. Same
    // walk, same expected list, and on the port two different exit codes.
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--skip=2", "--all"],
            &["log", "--oneline", "--skip=2", "--max-count=2", "--topo-order", "--all"],
        ],
        out,
    );
    out.push(Case::new("shortlog", &["shortlog", "--skip=2", "--all"], Shape::CrissCross));
}

/// Walking the reflog instead of the commit graph: `-g`/`--walk-reflogs`, and
/// `--reflog` as a source of pending tips.
///
/// The two are different questions and the corpus had neither. `-g` walks the
/// **entries** of one ref's log in order, so a commit reachable twice appears
/// twice (`9efbd8f` is `HEAD@{0}` and `HEAD@{4}` on [`Shape::CrissCross`]);
/// `--reflog` adds every reflogged id to the pending set and then walks the
/// graph normally, so it never repeats.
///
/// The output is deterministic: the selector `-g` prints is `HEAD@{n}`, a
/// count, and the only date in any of these formats is the pinned
/// `1700000000`. `--format=%h%d` is used once because ref decoration is where
/// a reflog walk's extra tips become visible without printing a date at all.
///
/// [`Shape::Packed`] is the negative half, and it is where both front ends go
/// wrong in opposite directions. Its reflog was expired at build time, so
/// `.git/logs/HEAD` and `.git/logs/refs/heads/main` are zero bytes and
/// `--reflog` contributes nothing. Stock 2.55.0 and 2.50.1 then split by front
/// end — `rev-list` has nothing to walk and exits 129 with its usage text,
/// `log` falls back to `HEAD` and prints all nine commits — and the port does
/// the opposite of each.
fn reflog_walk(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "-g", "HEAD"],
            &["log", "-g", "--format=%h%d", "HEAD"],
            &["log", "--oneline", "--walk-reflogs", "--all"],
            &["log", "--oneline", "--reflog"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "-g", "HEAD"],
            &["rev-list", "--walk-reflogs", "HEAD"],
            &["rev-list", "--walk-reflogs", "--count", "HEAD"],
            &["rev-list", "--reflog", "--count"],
        ],
        out,
    );

    // The expired-reflog half.
    out.push(Case::new("rev-list", &["rev-list", "--reflog"], Shape::Packed));
    out.push(Case::new("rev-list", &["rev-list", "--reflog", "--count"], Shape::Packed));
    out.push(Case::new("log", &["log", "--oneline", "--reflog"], Shape::Packed));
}

/// `--min-age`/`--max-age` and the two approxidate spellings that need no clock.
///
/// These are the only options in this file that read a date, and the whole
/// group exists to be measurable rather than to be interesting: every fixture
/// commit is stamped `1700000000`, so a raw epoch one second below it selects
/// everything and one second above it selects nothing, on any machine, at any
/// future date. The four cases are the two flags crossed with the two sides of
/// that boundary, which is the smallest set that distinguishes a port that
/// implements the comparison from one that inverts it and from one that ignores
/// the flag.
///
/// `--since`/`--until` are approxidate and would be a clock read in almost
/// every spelling. The `@<epoch>` form is the exception — `parse_date` resolves
/// it arithmetically and never calls `time(NULL)` — so those two cases are the
/// only ones here, and no relative spelling appears anywhere in this file.
fn age_window(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--all", "--max-age=1699999999"],
            &["rev-list", "--all", "--max-age=1700000001"],
            &["rev-list", "--all", "--min-age=1699999999"],
            &["rev-list", "--all", "--min-age=1700000001"],
            &["rev-list", "--all", "--max-age=1699999999", "--topo-order"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--all", "--until=@1700000001"],
            &["log", "--oneline", "--all", "--since=@1699999999"],
        ],
        out,
    );
}

/// Which refs the walk starts from, and the three options that decide whether
/// a walk happens at all.
///
/// `--exclude-hidden=<section>` is the half of the scope surface the corpus
/// never reached, and its interesting behaviour is a *refusal*: git rejects it
/// beside `--branches`, `--tags` or `--remotes` with
/// `error: options '--exclude-hidden' and '--branches' cannot be used
/// together`. All three section names are asked, because the option validates
/// the name (`fetch`, `receive`, `uploadpack`) before it validates the
/// combination.
///
/// `--no-walk` and `--do-walk` are last-one-wins over the same field, so
/// `--no-walk --do-walk` must walk. `--no-walk=unsorted` with three tips named
/// out of graph order is the only form that shows the difference from
/// `=sorted`: it emits them in argv order.
///
/// `--single-worktree` and `--alternate-refs` are asked on a shape with neither
/// a linked worktree nor an alternate, so both are no-ops on stock and the
/// answer is the ordinary walk. That is exactly what makes them worth asking:
/// the port rejects both from `log` while accepting `--single-worktree` from
/// `rev-list`, which is a difference no repository state can excuse.
fn walk_scope(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--exclude-hidden=fetch", "--all"],
            &["rev-list", "--exclude-hidden=receive", "--all"],
            &["rev-list", "--exclude-hidden=uploadpack", "--all"],
            &["rev-list", "--exclude-hidden=fetch", "--branches"],
            &["rev-list", "--single-worktree", "--all"],
            &["rev-list", "--alternate-refs"],
            &["rev-list", "--do-walk", "--all"],
            &["rev-list", "--no-walk", "--do-walk", "--all"],
            &["rev-list", "--no-walk=sorted", "--all"],
            &["rev-list", "--no-walk=unsorted", "--all"],
            &["rev-list", "--no-walk=unsorted", "cc-right", "cc-left", "main"],
            &["rev-list", "--exclude=refs/heads/cc-*", "--all"],
            &["rev-list", "--glob=refs/heads/cc-?", "--topo-order"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--exclude-hidden=fetch", "--branches"],
            &["log", "--oneline", "--single-worktree", "--all"],
            &["log", "--oneline", "--alternate-refs"],
            &["log", "--oneline", "--no-walk=unsorted", "cc-right", "cc-left", "main"],
        ],
        out,
    );
}

/// `--graph` crossed with every ordering and with the selection flags.
///
/// `graph.c` does not sort; it renders whatever order the walk produced, and it
/// *forces* `--topo-order` on when the caller asked for none. So the rows are a
/// second, independent readout of the same traversal — a port whose ordering is
/// wrong and whose glyph column is right still fails here, and the two failures
/// name the same bug rather than two.
///
/// It also forces `--topo-order` on when the caller named no order — verified:
/// `log --graph --oneline --all` and `log --graph --oneline --all --topo-order`
/// are byte-identical on [`Shape::CrissCross`] — so the `--date-order` and
/// `--author-date-order` rows below are the only ones whose order the caller
/// chose.
///
/// [`Shape::CrissCross`]'s rows are the ones a two-parent merge cannot produce:
/// stock draws `| |/| ` followed by `| |/  ` and `|/|   ` where the two merges
/// cross. [`Shape::Octopus`] supplies the `*---.` reach and the `|\ \ \` row.
/// `--reverse` under `--graph` is included because git draws the graph from the
/// reversed list rather than reversing the drawn rows, which is a different
/// picture and not a flipped one.
///
/// `rev-list --graph` is asked on both shapes because stock renders a graph for
/// it — `--graph` is a `revision.c` option, not a `builtin/log.c` one — and the
/// port's `rev-list` rejects it.
fn graph_orders(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--graph", "--oneline", "--all", "--topo-order"],
            &["log", "--graph", "--oneline", "--all", "--date-order"],
            &["log", "--graph", "--oneline", "--all", "--reverse"],
            &["log", "--graph", "--oneline", "--all", "--first-parent"],
            &["log", "--graph", "--oneline", "--merges", "--all"],
        ],
        out,
    );
    // `corpus.rs`'s own graph block owns `--graph --oneline --all --topo-order`
    // and `--all --date-order` on this shape, plus `--graph --oneline
    // --first-parent` with no `--all`. The three below are the spellings it does
    // not have: the third order, the scope flag in place of `--all` (same commit
    // set on this shape — checked, byte-identical — different pending-set code
    // path), and `--first-parent`
    // *with* `--all`, which is the only form where a second parent that carries
    // its own ref stays in the set.
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--graph", "--oneline", "--all", "--author-date-order"],
            &["log", "--graph", "--oneline", "--branches", "--topo-order"],
            &["log", "--graph", "--oneline", "--all", "--first-parent"],
            &["log", "--graph", "--oneline", "--all", "--reverse"],
            &["log", "--graph", "--oneline", "--no-merges", "--all"],
        ],
        out,
    );
    out.push(Case::new("rev-list", &["rev-list", "--graph", "--all"], Shape::Octopus));
    out.push(Case::new(
        "rev-list",
        &["rev-list", "--graph", "--topo-order", "--all"],
        Shape::CrissCross,
    ));
}

/// The mark a commit carries under `--graph`: `<`, `>`, `=` and `o`.
///
/// Outside `--graph` the left/right and cherry marks are a separate column and
/// the glyph column does not exist. Under `--graph` they are the *same*
/// column: `graph.c` asks `get_revision_mark()` for the character it would
/// otherwise draw as `*`. `fixture_gaps.rs` owns every plain spelling of these
/// flags on [`Shape::Cherry`]; none of them is under `--graph`, and the port
/// passes all of them.
///
/// It fails all four graph forms the same way. Stock 2.55.0 and 2.50.1,
/// `log --graph --oneline --cherry-mark --left-right main...topic`:
///
/// ```text
/// < b0db3a7 cherry: upstream only
/// = 6fca700 cherry: shared patch
/// > d74c8d4 cherry: topic only
/// = 7a4b88a cherry: shared patch
/// > dabff09 cherry: topic base
/// ```
///
/// The port prints `*` on all five rows. `--graph --boundary` is included as
/// the control: its `o` is correct on the port, which places the defect in the
/// mark lookup rather than in the glyph column or the row layout. The ordering
/// is also a finding in its own right — `--graph` forces a topological walk, so
/// the `=` rows move relative to the non-graph output, and the port reproduces
/// that movement correctly while losing the characters.
fn revision_marks(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "log",
        &[
            &["log", "--graph", "--oneline", "--left-right", "main...topic"],
            &["log", "--graph", "--oneline", "--cherry-mark", "--left-right", "main...topic"],
            &["log", "--graph", "--oneline", "--cherry-pick", "--left-right", "main...topic"],
            &["log", "--graph", "--oneline", "--cherry", "main...topic"],
            &["log", "--graph", "--oneline", "--boundary", "main...topic"],
        ],
        out,
    );
    out.push(Case::new(
        "log",
        &["log", "--graph", "--oneline", "--left-right", "cc-left...cc-right"],
        Shape::CrissCross,
    ));
    each(
        Shape::Cherry,
        "rev-list",
        &[
            &["rev-list", "--cherry-mark", "--left-right", "--topo-order", "main...topic"],
            &["rev-list", "--cherry-pick", "--reverse", "main...topic"],
            &["rev-list", "--cherry-mark", "--boundary", "main...topic"],
        ],
        out,
    );
}

/// Walk options asked of the front end that does not advertise them.
///
/// `revision.c` parses one option table for all five front ends, and each
/// front end then ignores what does not apply to it. `git log --count` is not
/// an error on stock: `--count` sets `revs->count`, `builtin/log.c` never reads
/// it, and the log prints normally. The same is true of `--bisect`,
/// `--objects` and `--unpacked` through `log`, and of `--graph` through
/// `rev-list` (which renders it). A port that builds a *per-verb* allow-list
/// instead of one shared table gets every one of these wrong in the same
/// direction, and that is what the group measures — six options, two front
/// ends, one root cause.
///
/// Every case is the same walk (`--all` on [`Shape::CrissCross`]) with one
/// option added, so the expected stdout is a constant and the only variable is
/// whether the option was tolerated.
fn front_end_split(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--count", "--all"],
            &["log", "--oneline", "--bisect", "--all"],
            &["log", "--oneline", "--objects", "--all"],
            &["log", "--oneline", "--unpacked", "--all"],
        ],
        out,
    );
}

/// `--bisect` over a DAG, crossed with the options that change the interval.
///
/// `bisect_replay.rs` owns the bisection algorithm on [`Shape::Packed`]'s
/// nine-commit line, where the midpoint is arithmetic and no walk option can
/// move it. What it cannot reach is the part that belongs here: `--bisect`
/// picks the commit that best halves the *set the walk produced*, so
/// `--first-parent` and `--no-merges` change the answer by changing the set,
/// and `--topo-order` must **not** change it because the choice is made from
/// the set rather than from its order.
///
/// [`Shape::CrissCross`] gives the two-merge-base interval
/// (`cc-left ^cc-a ^cc-b`) that a linear history cannot express;
/// [`Shape::Octopus`] gives an interval whose midpoint has four parents.
/// Neither shape appears in `bisect_replay.rs`. `--bisect-vars` and
/// `--bisect-all` are left out on purpose: both exit 129 on the port on every
/// input, which that module already reports, and re-spelling them here would
/// file one defect twice.
fn bisect_over_a_dag(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--bisect", "cc-left", "^cc-a", "^cc-b"],
            &["rev-list", "--bisect", "--first-parent", "cc-left", "^main"],
            &["rev-list", "--bisect", "--topo-order", "--all"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--bisect", "--first-parent", "--all"],
            &["rev-list", "--bisect", "--no-merges", "--all"],
        ],
        out,
    );
}
