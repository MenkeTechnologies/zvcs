//! Differential corpus cases for the merge **strategy** layer: which program
//! `git merge` hands the trees to, what that program does with more than one
//! merge base, and the `-X`/`merge.*` knobs that steer it.
//!
//! # What this module owns, and what it deliberately leaves alone
//!
//! Four modules already stand next to this one and each keeps its own half of
//! the merge surface. Read their headers before adding here; every case below
//! was written to sit outside all four.
//!
//! * [`super::merge_family`] owns `merge-file`, `merge-tree`, `mergetool`,
//!   `rerere`, and the ll-merge text driver — the *bytes* a three-way text merge
//!   produces. It reaches the backends only as one more way to run that driver
//!   (`branched`/`conflicted`/`detached`, one base, two heads).
//! * [`super::merge_dirty`] owns the dirty-worktree gates: index-versus-`HEAD`
//!   against this-path-on-the-way-past, and which of the two a refusal came
//!   from. The [`Shape::MergeableStaged`] cases here ask a narrower question it
//!   does not — *which strategy's own gate fires*, since `merge-ort`,
//!   `git-merge-resolve` and `git-merge-octopus` each open with a different
//!   check and word the refusal differently.
//! * [`super::fixture_gaps`] owns [`Shape::CrissCross`] as a *graph*:
//!   `merge-base --all`, `--independent`, and the plain `merge cc-right`.
//! * [`super::sequences`] owns everything that needs a second invocation —
//!   `--continue`, `--abort`, resolve-then-commit.
//!
//! # Why this layer, and why now
//!
//! `git merge -s <name>` execs a different program per name, and three of the
//! seven are shell scripts with their own argument grammar and their own
//! refusals. The port just gained a **virtual merge base** — merging the two
//! merge bases of a criss-cross with each other to build the base it then merges
//! against. Before that it merged a criss-cross against a single base and
//! produced the wrong content silently, and the blast radius of the fix is
//! exactly this layer, so the criss-cross shape carries the weight here:
//!
//! * `merge -s <name> cc-right` — one case per strategy, all reaching the same
//!   two incomparable bases through different code.
//! * `merge-recursive cc-a cc-b -- cc-left cc-right` — the recursion driven
//!   *directly*, with the two bases named rather than computed, beside the
//!   single-base forms (`cc-a`, `cc-b`, `main`) that produce a **different and
//!   wrong** answer. Stock resolves `clash.txt` to a conflict with two bases and
//!   cleanly with either one alone, so a port that silently picks one base
//!   passes the two-base case's stdout and fails these.
//! * `merge.verbosity=5` prints `  From inner merge:` lines — the inner merge
//!   *is* the virtual base being built, so this is the one surface on which the
//!   recursion is visible in stdout rather than only in the result.
//!
//! # Where the merged bytes are asserted
//!
//! `merge_family`'s header predates [`crate::runner`]'s `probe_worktree_content`
//! probe, which now reads every worktree file and compares small UTF-8 ones byte
//! for byte. So a strategy that writes the wrong content, the wrong conflict
//! markers or the wrong marker labels is caught directly, and the older routes
//! still apply on top: the backends stage their result, so `ls-files --stage -v`
//! carries the blob id and the stage numbers, and every blob written shows up in
//! `cat-file --batch-check --batch-all-objects`. That is what makes a *wrong
//! merge result* — as opposed to a differently worded message — a failure here
//! rather than a silent pass.
//!
//! # Constraints these cases work around
//!
//! * **No literal object ids.** The two sides are separate copies of the
//!   fixture, but every shape is built by the same script from the same seeds
//!   under a pinned identity and clock, so ids are equal across the pair;
//!   nevertheless every positional argument below is a rev the fixture resolves
//!   (`cc-a`, `main~1`, `main:conflict.txt`), never a transcribed id, so a case
//!   cannot rot when a shape gains a commit.
//! * **`merge-recursive`'s `<head>` must be the checked-out one.** It updates
//!   the real index and worktree through `unpack_trees`, so naming any other rev
//!   there fails the up-to-date check rather than merging. That rules out the
//!   [`Shape::Whitespace`] and [`Shape::Renamed`] histories as `-X`-separating
//!   fixtures: measured on stock 2.55.0, every
//!   `merge-recursive main~4 -- main~3 main~1` form on `Whitespace` exits 128
//!   with `Your local changes … would be overwritten`, because that shape holds
//!   an unstaged edit.
//! * **Rename thresholds cannot be *separated* by any shape in this corpus.**
//!   [`Shape::Renamed`] holds its renames in a linear history and no commit
//!   modifies a path another commit renames, so no rename/modify pair exists for
//!   a threshold to fall on either side of: measured on stock, the index after
//!   `merge-recursive main~2 -- main main~4` is byte-identical under the
//!   default, `--no-renames`, `--find-renames=90` and `--find-renames=20`. The
//!   `-X rename-threshold=`/`find-renames=`/`no-renames` cases below therefore
//!   pin acceptance and result and not detection quality, which is stated here
//!   rather than left for a reader to rediscover.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    criss_cross_strategies(out);
    criss_cross_xoptions(out);
    criss_cross_config(out);
    criss_cross_backends(out);
    unrelated_strategies(out);
    symlink_strategies(out);
    octopus_and_gates(out);
    one_file_driver(out);
    other_shapes(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// Push one strict case per argv: the refusal itself is the contract, so stderr
/// is compared byte for byte too.
fn each_strict(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::strict(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// `merge -s <name>` over two incomparable merge bases
// ---------------------------------------------------------------------------

/// One strategy per case against the criss-cross, where each of them has to
/// decide what to do about two merge bases.
///
/// The three that run reach the two bases through different mechanisms:
/// `recursive` and `subtree` build a virtual base by merging them (`subtree`
/// with a path shift on top), while `ours` never looks at a base at all and so
/// is the control — it must produce `HEAD`'s tree exactly, which the worktree
/// probe checks directly. (`-s resolve cc-right`, which hands both bases to
/// `read-tree -m` and lets the index resolution fail, is already in
/// [`super::fixture_gaps`]; it reappears below only inside a `-s a -s b` pair
/// and under `merge.verbosity=5`.)
///
/// Measured on stock 2.55.0: `recursive` and `subtree` conflict on `clash.txt`
/// (exit 1), `ours` commits `HEAD`'s tree (exit 0), `octopus` refuses a
/// two-head merge with `Merge with strategy octopus failed.` (exit 2), and an
/// unknown name exits 1 listing the five available strategies.
///
/// The `-s a -s b` pairs are the retry loop `cmd_merge` runs when more than one
/// strategy is named: it tries each in order, rewinds the tree between
/// attempts, and keeps the best result. Both orders are here because they
/// disagree — `recursive` first reports its own conflict and then `resolve`'s,
/// while `resolve` first fails through `git-merge-one-file` and only then is
/// `recursive` tried — and `ours` first short-circuits the loop entirely.
fn criss_cross_strategies(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge",
        &[
            &["merge", "-s", "recursive", "cc-right"],
            &["merge", "-s", "ours", "cc-right"],
            &["merge", "-s", "subtree", "cc-right"],
            // The multi-strategy retry loop, in both orders.
            &["merge", "-s", "recursive", "-s", "resolve", "cc-right"],
            &["merge", "-s", "resolve", "-s", "recursive", "cc-right"],
            &["merge", "-s", "ours", "-s", "recursive", "cc-right"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge",
        &[
            // `git-merge-octopus` exits 2 unless it is given two or more
            // remotes; the refusal is what a two-head `-s octopus` means.
            &["merge", "-s", "octopus", "cc-right"],
            // The name lookup itself, including the list it prints.
            &["merge", "-s", "no-such-strategy", "cc-right"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// `-X` strategy options
// ---------------------------------------------------------------------------

/// The `-X` options `merge-ort` accepts that no case in the corpus had yet
/// reached, on the shape where the option has a virtual merge base underneath
/// it. (`-X ours`, `-X theirs` and `-X diff-algorithm=histogram` on their own
/// are already in [`super::fixture_gaps`].)
///
/// Most of them cannot change *this* merge's outcome — `clash.txt` is one line
/// of ordinary text on both sides — and that is deliberate: what is pinned is
/// that the option parses, reaches the strategy, and leaves the result
/// untouched. An implementation that rejects `-Xignore-cr-at-eol`, or that lets
/// it flip a result it must not, fails here. The two that *do* change the
/// outcome are the contradicting pairs below.
///
/// `-X ours -X theirs` and `-X theirs -X ours` are the two contradicting
/// options the mandate asks for, and they are not symmetric: measured on stock
/// 2.55.0 the first commits with `cc.txt | 2 +-` and `clash.txt | 2 +-` in the
/// stat and the second with `cc.txt` alone, so last-wins is observable in
/// stdout as well as in the tree.
fn criss_cross_xoptions(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge",
        &[
            &["merge", "-X", "patience", "cc-right"],
            &["merge", "-X", "diff-algorithm=minimal", "cc-right"],
            &["merge", "-X", "diff-algorithm=myers", "cc-right"],
            &["merge", "-X", "ignore-space-change", "cc-right"],
            &["merge", "-X", "ignore-all-space", "cc-right"],
            &["merge", "-X", "ignore-space-at-eol", "cc-right"],
            &["merge", "-X", "ignore-cr-at-eol", "cc-right"],
            &["merge", "-X", "renormalize", "cc-right"],
            &["merge", "-X", "no-renormalize", "cc-right"],
            &["merge", "-X", "rename-threshold=25", "cc-right"],
            &["merge", "-X", "find-renames=25", "cc-right"],
            &["merge", "-X", "no-renames", "cc-right"],
            // A subtree shift onto a path that exists, so the shift is a no-op
            // the strategy still has to compute.
            &["merge", "-X", "subtree=cc", "cc-right"],
            &["merge", "-X", "ours", "-X", "theirs", "cc-right"],
            &["merge", "-X", "theirs", "-X", "ours", "cc-right"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge",
        &[&["merge", "-X", "no-such-option", "cc-right"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// `merge.*` configuration
// ---------------------------------------------------------------------------

/// The `merge.*` keys that steer the strategy rather than the porcelain around
/// it, delivered as `-c` so the case id carries the setting.
///
/// `merge.verbosity` is the interesting one. Measured on stock 2.55.0: `0`
/// suppresses the `Auto-merging`/`CONFLICT` lines and leaves only `Automatic
/// merge failed`; `2`, `3` and `4` are indistinguishable from the default here,
/// so none of them is spent on a case; and `5` prefixes the recursion's
/// own output with `  From inner merge:` — three extra lines that exist only
/// because the two bases had to be merged into a virtual one first. That makes
/// the `verbosity=5` case the only one in the corpus whose *stdout* proves the
/// virtual base was built at all.
fn criss_cross_config(out: &mut Vec<Case>) {
    let settings: &[(&str, &str)] = &[
        ("merge.conflictStyle", "merge"),
        ("merge.conflictStyle", "diff3"),
        ("merge.conflictStyle", "zdiff3"),
        ("merge.verbosity", "0"),
        ("merge.verbosity", "5"),
        ("merge.renames", "false"),
        ("merge.renameLimit", "1"),
        ("merge.directoryRenames", "conflict"),
    ];
    for (key, value) in settings {
        out.push(
            Case::new("merge", &["merge", "cc-right"], Shape::CrissCross)
                .with_config(&[(key, value)]),
        );
    }
    // The recursion made visible, through the shell strategy as well as the
    // built-in one: `git-merge-resolve` echoes its own progress, so the two
    // print different things at the same verbosity.
    out.push(
        Case::new("merge", &["merge", "-s", "resolve", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.verbosity", "5")]),
    );
    // An unparseable value: git fails before it reaches a strategy at all.
    out.push(
        Case::strict("merge", &["merge", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.conflictStyle", "nonsense")]),
    );
}

// ---------------------------------------------------------------------------
// The backends, invoked directly
// ---------------------------------------------------------------------------

/// The strategy programs run as commands, with the merge bases *named* instead
/// of computed.
///
/// This is the sharpest instrument in the module. `git merge` computes the
/// bases itself, so a port that computes them wrongly and a port that merges
/// them wrongly are indistinguishable through it. Here the argv fixes the
/// bases, so the two questions separate:
///
/// * `cc-a cc-b -- cc-left cc-right` is the two-base recursion. Stock conflicts
///   on `clash.txt` (exit 1) because the virtual base holds conflict markers
///   that exist in no commit.
/// * `cc-a -- …` and `cc-b -- …` are the *wrong* answers a single-base port
///   produces, and stock resolves both cleanly (exit 0, `Auto-merging cc.txt`
///   alone). They are here so a port that quietly drops the second base has
///   somewhere to fail that is not also failing for another reason.
/// * `main -- …` is the common ancestor of both bases — a third answer again
///   (exit 1), and the one a port that walks past the criss-cross would give.
/// * `-- cc-left cc-right` names no base at all, and `cc-a cc-b main -- …`
///   names three; both are accepted by `builtin/merge-recursive.c`'s argument
///   loop, which takes everything before `--` as a base.
///
/// The argv grammar is `<base>... -- <head> <remote>` for all of
/// `merge-recursive`, `merge-recursive-ours`, `merge-recursive-theirs`,
/// `merge-subtree` (all one binary, dispatching on `argv[0]`), `merge-ours`,
/// and the two shell scripts `git-merge-resolve` and `git-merge-octopus`, whose
/// opening `for arg` loop splits on the first `--`.
fn criss_cross_backends(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge-recursive",
        &[
            &["merge-recursive", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
            &["merge-recursive", "cc-a", "--", "cc-left", "cc-right"],
            &["merge-recursive", "cc-b", "--", "cc-left", "cc-right"],
            &["merge-recursive", "main", "--", "cc-left", "cc-right"],
            &["merge-recursive", "--", "cc-left", "cc-right"],
            &["merge-recursive", "cc-a", "cc-b", "main", "--", "cc-left", "cc-right"],
            // `parse_merge_opt` is reached for every `--<opt>` before the bare
            // `--`, which is the same option table `-X` feeds.
            &["merge-recursive", "--ours", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
            &["merge-recursive", "--theirs", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
            &["merge-recursive", "--patience", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
            &[
                "merge-recursive",
                "--diff-algorithm=histogram",
                "cc-a",
                "cc-b",
                "--",
                "cc-left",
                "cc-right",
            ],
            &["merge-recursive", "--no-renames", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge-recursive",
        &[
            // No `--` at all: the usage line, exit 129.
            &["merge-recursive", "cc-left", "cc-right"],
            // One head, and three: `fatal: not handling anything other than two
            // heads merge.`
            &["merge-recursive", "cc-a", "cc-b", "--", "cc-left"],
            &["merge-recursive", "cc-a", "cc-b", "--", "cc-left", "cc-right", "cc-a"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "merge-recursive-ours",
        &[&["merge-recursive-ours", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
    each(
        Shape::CrissCross,
        "merge-recursive-theirs",
        &[&["merge-recursive-theirs", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
    each(
        Shape::CrissCross,
        "merge-subtree",
        &[&["merge-subtree", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
    // `git-merge-resolve` hands both bases to `read-tree -m` and then drives
    // `git-merge-one-file` over what is left unmerged. With two bases the
    // read-tree resolution reports `Added clash.txt in both, but differently.`
    // and the per-file driver fails on the empty stage-1 blob — an error path
    // stock reaches on its own, not a fixture accident.
    each(
        Shape::CrissCross,
        "merge-resolve",
        &[&["merge-resolve", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge-resolve",
        // No base: `read-tree -m <head> <remote>` is a two-way merge, which the
        // script rejects with exit 2 and no output at all.
        &[&["merge-resolve", "--", "cc-left", "cc-right"]],
        out,
    );
    // `merge-ours` never reads a base or a remote; naming two bases must change
    // nothing, and the worktree probe is what says so.
    each(
        Shape::CrissCross,
        "merge-ours",
        &[&["merge-ours", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
    each(
        Shape::CrissCross,
        "merge-octopus",
        &[&["merge-octopus", "main", "--", "cc-left", "cc-right", "cc-a"]],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge-octopus",
        &[&["merge-octopus", "cc-a", "cc-b", "--", "cc-left", "cc-right"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// No merge base at all
// ---------------------------------------------------------------------------

/// The same strategies over two roots, where the base is empty rather than
/// ambiguous.
///
/// The strategies split three ways here and the split is the point. Measured on
/// stock 2.55.0: `recursive` and `subtree` merge two roots happily; `resolve`
/// and `octopus` refuse with `Merge with strategy <name> failed.` (exit 2)
/// because `read-tree -m` needs a base tree; and `ours` succeeds without
/// looking. A port that routes every strategy through one implementation gets
/// two of those wrong while passing the third.
///
/// `-s subtree` is not a synonym for `recursive` on this shape: the shift finds
/// `src/` and lands the far root's files *under it* — stock writes
/// `src/alien.txt` rather than `alien.txt`, and `src/README.md` rather than a
/// conflicted `README.md`. `-X subtree=src` asks for the same shift explicitly.
/// Those are the two cases in this module where a wrong strategy produces a
/// wrong *tree layout* rather than a wrong message.
fn unrelated_strategies(out: &mut Vec<Case>) {
    each(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "-s", "recursive", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-s", "ours", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-s", "subtree", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-X", "subtree=src", "--allow-unrelated-histories", "-m", "join", "alien"],
            &[
                "merge",
                "-s",
                "recursive",
                "--allow-unrelated-histories",
                "-m",
                "join",
                "alien-clash",
            ],
            &["merge", "-s", "subtree", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "-X", "theirs", "-m", "join", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "-X", "patience", "-m", "join", "alien-clash"],
        ],
        out,
    );
    each_strict(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "-s", "resolve", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-s", "octopus", "--allow-unrelated-histories", "-m", "join", "alien"],
        ],
        out,
    );

    // The backends over the same pair. `main` is named as the base even though
    // it is not an ancestor of `alien`: the backends take whatever tree they are
    // given, which is how a base that shares nothing with either head is
    // reachable at all.
    each(
        Shape::Unrelated,
        "merge-recursive",
        &[&["merge-recursive", "main", "--", "main", "alien"]],
        out,
    );
    each(
        Shape::Unrelated,
        "merge-recursive-ours",
        &[&["merge-recursive-ours", "main", "--", "main", "alien-clash"]],
        out,
    );
    each(
        Shape::Unrelated,
        "merge-recursive-theirs",
        &[&["merge-recursive-theirs", "main", "--", "main", "alien-clash"]],
        out,
    );
    each(
        Shape::Unrelated,
        "merge-resolve",
        &[&["merge-resolve", "main", "--", "main", "alien"]],
        out,
    );
    each(
        Shape::Unrelated,
        "merge-subtree",
        &[
            &["merge-subtree", "main", "--", "main", "alien"],
            &["merge-subtree", "main", "--", "main", "alien-clash"],
        ],
        out,
    );
    each_strict(
        Shape::Unrelated,
        "merge-octopus",
        // `Unable to find common commit with alien`, exit 1 — the script's own
        // `merge-base` probe, not the porcelain's.
        &[&["merge-octopus", "main", "--", "main", "alien", "alien-clash"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// Symlinks and a mode change
// ---------------------------------------------------------------------------

/// The strategies over a merge that changes a file into a symlink.
///
/// `sym-pending` replaces the regular file `dir/target.txt` with a symlink and
/// adds one more symlink and one empty file, so every strategy has to carry a
/// `100644 => 120000` mode change and a zero-byte blob through. Stock's stat
/// block names the mode change explicitly, and `probe_worktree_content` reads
/// the link *target* rather than following it, so a port that writes a regular
/// file holding the target text — the usual way this breaks — is caught even
/// though the merge "succeeded".
///
/// `-s resolve` takes a path none of the others do: `cmd_merge`'s
/// `read_tree_trivial` succeeds here, so stock prints `Trying really trivial
/// in-index merge... / Wonderful. / In-index merge` and never runs a strategy
/// program at all. That fast path is only reachable on a merge no path
/// conflicts on, which is why it appears on this shape and on `Octopus` below.
fn symlink_strategies(out: &mut Vec<Case>) {
    each(
        Shape::Symlinks,
        "merge",
        &[
            &["merge", "-s", "recursive", "-m", "sym", "sym-pending"],
            &["merge", "-s", "resolve", "-m", "sym", "sym-pending"],
            &["merge", "-s", "ours", "-m", "sym", "sym-pending"],
            &["merge", "-s", "subtree", "-m", "sym", "sym-pending"],
            &["merge", "-X", "ours", "-m", "sym", "sym-pending"],
        ],
        out,
    );
    each(
        Shape::Symlinks,
        "merge-recursive",
        &[&["merge-recursive", "main~1", "--", "main", "sym-pending"]],
        out,
    );
    each(
        Shape::Symlinks,
        "merge-recursive-ours",
        &[&["merge-recursive-ours", "main~1", "--", "main", "sym-pending"]],
        out,
    );
    each(
        Shape::Symlinks,
        "merge-recursive-theirs",
        &[&["merge-recursive-theirs", "main~1", "--", "main", "sym-pending"]],
        out,
    );
    each(
        Shape::Symlinks,
        "merge-resolve",
        &[&["merge-resolve", "main~1", "--", "main", "sym-pending"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// More than two parents, and each strategy's own refusal
// ---------------------------------------------------------------------------

/// The octopus strategy where it is the only one that can run, and the
/// index-versus-`HEAD` gate where each strategy words it differently.
///
/// Two questions share this function because they share the two `Mergeable`
/// shapes.
///
/// **More than two parents.** `git-merge-octopus` is the only strategy that
/// accepts three or more heads; `merge-recursive` refuses with `error: Not
/// handling anything other than two heads merge.` and exit 2. `div-cold` and
/// `div-other` are two branches diverged from `main` that touch different
/// paths, so `merge -s octopus div-cold div-other` is a real three-parent merge
/// — stock prints one `Trying simple merge with <branch>` line per remote and
/// commits. Driven directly, `merge-octopus main -- main div-cold div-other`
/// prints the same lines with the script's own `<subject>:-<ref>` labels.
///
/// **Whose gate refused.** Each strategy opens with a different check and says
/// so differently, and [`Shape::MergeableStaged`] — a staged change on a path no
/// branch touches — trips all of them. Measured on stock 2.55.0:
/// `git-merge-octopus` echoes `Error: Your local changes …` with a
/// four-space-indented path list from its own `diff-index --cached` and exits 2;
/// `git-merge-resolve` prints the two-space-indented `error:` form; and
/// `merge-ort` (behind `-s recursive`/`-s subtree`) prints its own `error:`
/// before `merge` adds `Merge with strategy <name> failed.` A port with one
/// shared refusal passes none of these. They are strict for that reason: the
/// wording is the identity of the gate, and it lands on stderr for two of the
/// three.
fn octopus_and_gates(out: &mut Vec<Case>) {
    each(
        Shape::MergeableDirty,
        "merge",
        &[
            &["merge", "-s", "octopus", "div-cold", "div-other"],
            &["merge", "-s", "resolve", "div-cold"],
            &["merge", "-s", "subtree", "div-cold"],
        ],
        out,
    );
    each_strict(
        Shape::MergeableDirty,
        "merge",
        &[&["merge", "-s", "recursive", "div-cold", "div-other"]],
        out,
    );
    each(
        Shape::MergeableDirty,
        "merge-octopus",
        &[&["merge-octopus", "main", "--", "main", "div-cold", "div-other"]],
        out,
    );
    each(
        Shape::MergeableDirty,
        "merge-resolve",
        &[&["merge-resolve", "main", "--", "main", "div-cold"]],
        out,
    );
    each(
        Shape::MergeableDirty,
        "merge-recursive",
        &[&["merge-recursive", "main", "--", "main", "div-cold"]],
        out,
    );

    // `merge.*` keys whose effect is a *decision* the strategy layer makes:
    // whether to stash first, whether to fast-forward at all, and what goes in
    // the message the strategy's result is committed under. `merge.log` writes a
    // shortlog into the commit message, so it moves the commit id rather than
    // stdout — the state probe is what reads it.
    let dirty_cfg: &[(&[&str], &[(&str, &str)])] = &[
        (&["merge", "div-hot"], &[("merge.autoStash", "true")]),
        (&["merge", "div-cold"], &[("merge.stat", "false")]),
        (&["merge", "--no-ff", "div-cold"], &[("merge.log", "true")]),
        (&["merge", "--no-ff", "div-other"], &[("merge.log", "2")]),
        (&["merge", "ff-cold"], &[("merge.ff", "false")]),
    ];
    for (args, config) in dirty_cfg {
        out.push(Case::new("merge", args, Shape::MergeableDirty).with_config(config));
    }
    // The two refusals: a merge that would overwrite a dirty path with
    // autostash off, and a diverged merge under `merge.ff=only`.
    out.push(
        Case::strict("merge", &["merge", "div-hot"], Shape::MergeableDirty)
            .with_config(&[("merge.autoStash", "false")]),
    );
    out.push(
        Case::strict("merge", &["merge", "div-cold"], Shape::MergeableDirty)
            .with_config(&[("merge.ff", "only")]),
    );

    each_strict(
        Shape::MergeableStaged,
        "merge",
        &[
            &["merge", "-s", "octopus", "div-cold", "div-other"],
            &["merge", "-s", "resolve", "div-cold"],
            &["merge", "-s", "subtree", "div-cold"],
            &["merge", "-s", "recursive", "div-cold"],
        ],
        out,
    );
    each_strict(
        Shape::MergeableStaged,
        "merge-octopus",
        &[&["merge-octopus", "main", "--", "main", "div-cold", "div-other"]],
        out,
    );
    each_strict(
        Shape::MergeableStaged,
        "merge-ours",
        &[&["merge-ours", "main", "--", "main", "div-cold"]],
        out,
    );

    // The trivial in-index path and the strategies over a shape that already
    // holds a four-parent merge, so the octopus backend runs beside one.
    each(
        Shape::Octopus,
        "merge",
        &[
            &["merge", "-s", "octopus", "oct-side"],
            &["merge", "-s", "resolve", "oct-side"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "merge-octopus",
        &[
            &["merge-octopus", "main~2", "--", "main", "oct-side", "oct-a"],
            &["merge-octopus", "main~2", "--", "main", "oct-a", "oct-b", "oct-c"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "merge-resolve",
        &[&["merge-resolve", "main~2", "--", "main", "oct-side"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// `merge-one-file`, the per-path driver
// ---------------------------------------------------------------------------

/// `git-merge-one-file` reached both ways: driven by `merge-index`, and called
/// directly with the seven positional arguments its usage names.
///
/// The signature is `<orig blob> <our blob> <their blob> <path> <orig mode>
/// <our mode> <their mode>`, and an *empty* id-and-mode pair is how a side that
/// does not have the file is spelled. Each case below picks a different branch
/// of the script's `case "${1:-.}${2:-.}${3:-.}"` dispatch:
///
/// * all three present, modes equal, and the two sides agreeing — the ordinary
///   three-way merge, which resolves.
/// * their mode `120000` — `Not merging symbolic link changes.`, the branch
///   that refuses rather than merging bytes.
/// * our side empty, and their side empty — the two `Not handling case` forms,
///   which echo the ids they were given back.
/// * orig and ours empty — a plain add, driven through `update-index
///   --cacheinfo`.
///
/// # The branch that cannot be measured, and why
///
/// Any argv that makes `git-merge-one-file` **write a conflicted file** is
/// excluded from this module, because stock git does not reproduce it. The
/// script unpacks its three inputs with `git unpack-file`, which names each
/// temporary file `.merge_file_<6 random chars>`, and hands those names to
/// `git merge-file` — which writes them into the conflict markers. Measured on
/// stock 2.55.0, two runs of the same argv in two fresh copies of
/// [`Shape::Conflicted`] left `conflict.txt` reading
/// `<<<<<<< .merge_file_MOwyFV` and `<<<<<<< .merge_file_XlgpLy`. So the
/// permissions-conflict branch (`100644 100755 100644`) and the add/add branch
/// (empty orig) are both non-deterministic on the *stock* side, and the runner
/// files them under `NONDETERMINISTIC` rather than measuring them. One
/// pre-existing case in [`super::merge_family`] already carries that status for
/// the same reason. The same rules out
/// `merge-resolve main -- cc-left cc-right`, whose single base leaves
/// `clash.txt` for the per-file driver to conflict on; the two-base form is
/// kept because `read-tree` refuses it earlier, before any file is written.
///
/// Every id here is a `<rev>:<path>` the fixture resolves rather than a
/// transcribed object id, which is also what makes the `Not handling case`
/// messages deterministic: they print the argument, not the object.
fn one_file_driver(out: &mut Vec<Case>) {
    const OURS: &str = "main:conflict.txt";
    const THEIRS: &str = "theirs:conflict.txt";
    const BASE: &str = "main^:README.md";

    each(
        Shape::Conflicted,
        "merge-one-file",
        &[
            &["merge-one-file", BASE, OURS, THEIRS, "conflict.txt", "100644", "100644", "120000"],
            &["merge-one-file", BASE, OURS, OURS, "conflict.txt", "100644", "100644", "100644"],
            &["merge-one-file", OURS, OURS, THEIRS, "conflict.txt", "100644", "100644", "100644"],
            &["merge-one-file", BASE, "", THEIRS, "conflict.txt", "100644", "", "100644"],
            &["merge-one-file", BASE, OURS, "", "conflict.txt", "100644", "100644", ""],
            &["merge-one-file", "", "", THEIRS, "only-theirs.txt", "", "", "100644"],
        ],
        out,
    );

    // The same driver reached through `merge-index`, named per path rather than
    // with `-a`, which is the form the existing `merge_family` cases do not use.
    each(
        Shape::Conflicted,
        "merge-index",
        &[
            &["merge-index", "-o", "git-merge-one-file", "conflict.txt"],
            &["merge-index", "-o", "-q", "git-merge-one-file", "conflict.txt"],
            &["merge-index", "-o", "git-merge-one-file", "--", "conflict.txt"],
        ],
        out,
    );
    each_strict(
        Shape::Conflicted,
        "merge-index",
        &[&["merge-index", "-o", "git-merge-one-file", "--", "no-such-file"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// The remaining shapes
// ---------------------------------------------------------------------------

/// Strategy cases on shapes whose value is a single question each.
///
/// * [`Shape::Branched`] — every strategy on a merge that *can* fast-forward,
///   which splits them two ways. Measured on stock 2.55.0, `-s recursive`,
///   `-s resolve`, `-s octopus` and `-X ours` all print `Updating <a>..<b> /
///   Fast-forward` and create no merge commit, while `-s ours` and `-s subtree`
///   print `Merge made by the '<name>' strategy.` and do. The split is the
///   `NO_FAST_FORWARD` bit in git's strategy table, and a port that either
///   applies it to every named strategy or to none passes half these cases and
///   fails the other half.
/// * [`Shape::Merged`] — the two parents of a real merge commit named as two
///   bases, which is the only place in the corpus where a multi-base argv is
///   built from `HEAD^1`/`HEAD^2` rather than from branch names.
/// * [`Shape::Conflicted`] — a merge attempted while one is unresolved. The
///   refusal is `cmd_merge`'s, before any strategy, and it is strict because
///   that ordering is the claim.
/// * [`Shape::Renamed`] — one rename-threshold case, kept as a regression pin
///   for option acceptance. See this module's header for why no shape in this
///   corpus can make the threshold change an answer.
fn other_shapes(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "merge",
        &[
            &["merge", "-s", "recursive", "feature"],
            &["merge", "-s", "resolve", "feature"],
            &["merge", "-s", "octopus", "feature"],
            &["merge", "-s", "ours", "feature"],
            &["merge", "-s", "subtree", "feature"],
            &["merge", "-X", "ours", "feature"],
        ],
        out,
    );
    each(
        Shape::Branched,
        "merge-recursive",
        &[&["merge-recursive", "main^", "main", "--", "main", "feature"]],
        out,
    );
    each(
        Shape::Merged,
        "merge-recursive",
        &[&["merge-recursive", "HEAD^1", "HEAD^2", "--", "main", "side"]],
        out,
    );
    each(
        Shape::Merged,
        "merge-resolve",
        &[&["merge-resolve", "HEAD^1", "HEAD^2", "--", "main", "side"]],
        out,
    );
    each_strict(
        Shape::Conflicted,
        "merge",
        &[&["merge", "-s", "resolve", "theirs"]],
        out,
    );
    each(
        Shape::Renamed,
        "merge-recursive",
        &[&["merge-recursive", "--find-renames=90", "main~2", "--", "main", "main~4"]],
        out,
    );
}
