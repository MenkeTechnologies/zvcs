//! Differential corpus cases for **history simplification**: the rule that
//! decides which commits `log <path>` prints and, for a merge, which parents it
//! keeps walking.
//!
//! `git log -- <path>` is not "walk every commit and keep the ones that touched
//! the path". It is the algorithm `git-log(1)` documents under *History
//! Simplification* and `revision.c` implements in `simplify_commit()`,
//! `try_to_simplify_commit()` and `rewrite_parents()`: at every commit git asks
//! whether the commit's tree at the pathspec is the same as each parent's
//! (TREESAME), and for a merge that is TREESAME to *some* parent it drops every
//! other parent, follows that one, and does not print the merge at all. The
//! merge vanishes even though it is the commit that put the path's content on
//! the branch.
//!
//! That is the whole reason this module exists. A port that implements
//! "traverse, then filter by whether the diff touched the pathspec" produces the
//! right answer on every linear history and on every merge-free shape — and the
//! corpus is mostly merge-free, so the wrong implementation scores full marks.
//! **Every group below is anchored on a shape that contains a merge**, and the
//! comment above each group records the commit list stock git 2.55.0 printed, so
//! a divergence names the missing or extra commit rather than "output differs".
//!
//! # Territory
//!
//! * `history_query.rs` owns traversal *selection* as an argument-parsing
//!   question — `--parents`, `--children`, `--boundary`, `--cherry-mark`,
//!   `--left-right`, bare `--min-parents=`/`--merges` with no pathspec, and the
//!   `merge-base`/`show-branch`/`cherry` family. Nothing there passes a pathspec
//!   to a walk, which is exactly the input that turns simplification on.
//! * `log_format.rs` owns rendering: which bytes a commit that was already
//!   selected turns into. No case here varies the format beyond `--oneline`
//!   (and the raw line `whatchanged` adds), because the answer under test is the
//!   *list*, not the layout.
//! * `corpus.rs`'s `--graph` cases pin `graph.c`'s rows over an unsimplified
//!   walk. The `graph_rows` block here pins the opposite: the rows `graph.c`
//!   draws once simplification has already rewritten the parent set, where a
//!   four-parent octopus prints as a single `*` with no lanes.
//! * `misc_commands.rs` owns `log --follow` refusing two pathspecs and
//!   `log.follow=true`; `shape_reach.rs` owns bare `log --follow` on
//!   `moved/alpha.txt` and `moved/beta.txt`. Those three are deliberately **not**
//!   re-filed here even under a different spelling. The `--follow` block below
//!   is the part they do not reach: `--follow` crossed with the simplification
//!   modes, `--follow` on the copy and the rewrite, `--follow` on a path that
//!   never existed, and the refusals that belong to a *different front end*
//!   (`rev-list --follow`, `whatchanged --follow` with two paths) or a different
//!   cause (pathspec magic).
//!
//! # The rule, and where each shape sits in it
//!
//! Established by running the six modes against every candidate shape by hand
//! before a case was written. What was found:
//!
//! * [`Shape::Merged`] — **usable, and the clearest instrument in the corpus.**
//!   `initial` → `main commit` (adds `main.txt`) and `initial` → `side commit`
//!   (adds `side.txt`), joined by `merge side`. On `side.txt` the merge is
//!   TREESAME to its *second* parent, so default simplification rewrites it away
//!   and prints one commit; on `main.txt` it is TREESAME to its *first*; on
//!   `README.md` it is TREESAME to both. All six modes give different answers on
//!   `side.txt`, and `--show-pulls` separates `side.txt` from `main.txt` — the
//!   merge is a "pull" for the former and not for the latter.
//! * [`Shape::Octopus`] — **usable, and the only shape where a merge has four
//!   parents.** It is the only place the interaction that follows can be seen at
//!   all: with a pathspec, simplification rewrites the octopus down to a *single*
//!   parent, so `--merges` and `--min-parents=2` then reject the very merge
//!   `--sparse` just printed. A port that applies the parent-count filter to the
//!   original parent list instead of the rewritten one keeps a commit git drops.
//! * [`Shape::CrissCross`] — **usable, and the only shape where
//!   `--simplify-merges` *keeps* a merge.** On `clash.txt` both `cc-a` and
//!   `cc-b` change the file and `cc-left merge` resolves them, so after
//!   `--full-history` the merge still has two distinct relevant parents and
//!   survives the `--simplify-merges` pass — where on `Merged` the same pass
//!   removes it. Without this shape, "simplify-merges removes the merge" and
//!   "simplify-merges is a no-op" score identically. `HEAD` is `cc-left`, so
//!   `cc-right` is reachable only under `--all`.
//! * [`Shape::CommitGraph`] — **usable, and the only shape whose answer is
//!   computed from Bloom filters.** It carries a two-parent merge *and* a
//!   `commit-graph write --reachable --changed-paths`, so `log -- <path>` reads
//!   `revision.c`'s `get_bloom_filter_for_commit()` fast path instead of
//!   diffing trees. The filter is an optimisation and must not change the list;
//!   a port whose filter says "no change" for a commit that did change prunes a
//!   commit git keeps. `cg-late` was committed after the write, so part of the
//!   walk is outside the graph.
//! * [`Shape::Renamed`] — **not usable for the mode axis, and used anyway for
//!   `--follow`.** It has five commits on one line and *no merge at all*, so
//!   default, `--full-history`, `--full-history --simplify-merges`, `--dense`,
//!   `--show-pulls` and `--first-parent` were verified to print byte-identical
//!   lists on every path in it. Only `--sparse` and `--simplify-by-decoration`
//!   move, and they move for reasons that have nothing to do with merges. The
//!   handful of mode cases kept on it are there to pin that collapse, not to
//!   measure it. Rename-following is simplification's cousin — `--follow` is
//!   `diff_tree_oid()` under `try_to_simplify_commit()` with a mutable
//!   pathspec — and this is the only shape with renames to follow.
//! * Shapes checked and **rejected**: `Branched`, `Linear`, `Whitespace`,
//!   `Packed`, `Attributes` and `MergeableDirty` contain no merge commit at all,
//!   so every mode collapses to one list and a case on them would measure
//!   argument parsing. `Conflicted` and `Rerere` do run a merge, but both stop
//!   *mid*-merge on purpose — there is no merge commit in either object store to
//!   simplify. Only four shapes in `fixture.rs` end with a committed merge:
//!   `Merged`, `Octopus`, `CrissCross` and `CommitGraph`, and all four are used
//!   below.
//!
//! # Determinism
//!
//! `env::harden` pins `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` to
//! `1700000000 +0000` for *every* commit in *every* shape, so commit date is a
//! constant and date order degenerates to whatever order the traversal happened
//! to reach commits in. That makes the default `--date-order` a poor witness for
//! anything: two implementations with different queue behaviour can both be
//! "date ordered" and print different lists. **Every walk here that prints more
//! than one commit passes `--topo-order`**, which is a function of the graph
//! alone and therefore the only ordering the fixture can pin. `rev-list --count`
//! and `shortlog -s` are used where the count is the whole answer and order is
//! not.
//!
//! No case reads the clock, samples randomness, names an absolute path, or
//! touches the filesystem at generation time: every argv below is a literal.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    six_modes_merged(out);
    six_modes_octopus(out);
    six_modes_criss_cross(out);
    six_modes_renamed(out);
    six_modes_commit_graph(out);
    show_pulls(out);
    first_parent(out);
    ancestry_path(out);
    follow(out);
    parent_count_filters(out);
    pathspec_shapes(out);
    other_front_ends(out);
    graph_rows(out);
    refusals(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// The six modes on [`Shape::Merged`], the fixture's smallest real merge.
///
/// History: `edfab1b initial` → `2d98e86 main commit` (adds `main.txt`) and
/// `edfab1b initial` → `d4cf82a side commit` (adds `side.txt`), joined by
/// `f781761 merge side` whose parents are `2d98e86`, `d4cf82a` in that order.
///
/// Observed, stock git 2.55.0, `log --oneline --topo-order <mode> -- side.txt`:
///
/// ```text
/// (default)                        d4cf82a
/// --full-history                   f781761 d4cf82a
/// --full-history --simplify-merges d4cf82a
/// --sparse                         f781761 d4cf82a edfab1b
/// --dense                          d4cf82a
/// --simplify-by-decoration         f781761 d4cf82a
/// ```
///
/// Six modes, five distinct answers, one path. Default drops the merge because
/// it is TREESAME to parent 2 and rewrites it to that parent; `--full-history`
/// stops rewriting and the merge comes back; `--simplify-merges` then removes it
/// again because after the rewrite it has one relevant parent and is redundant;
/// `--sparse` keeps `initial`, which is TREESAME and therefore invisible in
/// every other mode; `--dense` is the default spelled out; and
/// `--simplify-by-decoration` keeps the merge because `main` decorates it.
///
/// The same six on `main.txt`, where the merge is TREESAME to parent *1*:
///
/// ```text
/// (default)                        2d98e86
/// --full-history                   f781761 2d98e86
/// --full-history --simplify-merges 2d98e86
/// --sparse                         f781761 2d98e86 edfab1b
/// --dense                          2d98e86
/// --simplify-by-decoration         f781761 d4cf82a 2d98e86
/// ```
///
/// `--simplify-by-decoration` is the one that moves: it keeps `d4cf82a` for
/// `side`'s ref even though `side.txt` is not the pathspec, because decoration
/// is a reason to keep a commit independent of TREESAME.
///
/// And on `README.md`, TREESAME to both parents and unchanged since `initial`:
///
/// ```text
/// (default)                        edfab1b
/// --full-history                   edfab1b
/// --sparse                         f781761 2d98e86 edfab1b
/// --simplify-by-decoration         f781761 d4cf82a edfab1b
/// ```
fn six_modes_merged(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--dense", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--", "main.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "main.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "main.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "main.txt"],
            &["log", "--oneline", "--topo-order", "--", "README.md"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "README.md"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "README.md"],
        ],
        out,
    );
    // `--sparse` and `--dense` are the same switch; the *last* one wins.
    // Observed: `--sparse --dense -- side.txt` prints `d4cf82a` alone, while
    // `--dense --sparse -- side.txt` prints `f781761 d4cf82a edfab1b`. An
    // implementation that treats either as a latch rather than an assignment
    // gets exactly one of these two right.
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--sparse", "--dense", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--dense", "--sparse", "--", "side.txt"],
        ],
        out,
    );
}

/// The six modes on [`Shape::Octopus`], where the merge has four parents.
///
/// History: `edfab1b initial` → `ff8f0e8 main moves on` (adds `trunk.txt`);
/// `205fc50 oct-a`, `48e9604 oct-b`, `92df11d oct-c` each fork from `initial`
/// and add one file of their own; `a51b7b9 oct-side` forks too and is never
/// merged; `dc58074 octopus merge` has parents `ff8f0e8 205fc50 48e9604
/// 92df11d`.
///
/// Observed, `log --oneline --topo-order <mode> -- oct-a.txt`:
///
/// ```text
/// (default)                        205fc50
/// --full-history                   dc58074 205fc50
/// --full-history --simplify-merges 205fc50
/// --sparse                         dc58074 205fc50 edfab1b
/// --dense                          205fc50
/// --simplify-by-decoration         dc58074 92df11d 48e9604 205fc50
/// ```
///
/// The `--full-history` row is the one worth staring at: the four-parent merge
/// prints with **one** surviving relevant parent, because only `oct-a` touched
/// `oct-a.txt`. `--simplify-by-decoration` keeps all three branch tips because
/// each carries a ref.
///
/// `trunk.txt`, which only the first parent's side touches:
///
/// ```text
/// (default)                        ff8f0e8
/// --full-history                   dc58074 ff8f0e8
/// --simplify-by-decoration         dc58074 92df11d 48e9604 205fc50 ff8f0e8
/// ```
///
/// `oct-side.txt`, which is on a branch the walk from `HEAD` never enters — the
/// answer is empty in five of the six modes, and `--sparse` still prints the
/// first-parent spine because "no commit changed the path" is not the same
/// question as "which commits did the walk visit":
///
/// ```text
/// (default)                        <empty>
/// --sparse                         dc58074 ff8f0e8 edfab1b
/// --simplify-by-decoration         dc58074 92df11d 48e9604 205fc50
/// ```
///
/// `README.md`, TREESAME everywhere: `(default)` prints `edfab1b`, `--sparse`
/// prints `dc58074 ff8f0e8 edfab1b`, `--simplify-by-decoration` prints
/// `dc58074 92df11d 48e9604 205fc50 edfab1b`.
fn six_modes_octopus(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--", "trunk.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "trunk.txt"],
            &["log", "--oneline", "--topo-order", "--", "oct-side.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "oct-side.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "README.md"],
        ],
        out,
    );
    // `oct-side` is never merged, so it is reachable only under `--all`. Under
    // `--full-history --simplify-merges --all` the whole octopus collapses and
    // the answer is the one commit that touched the path: `a51b7b9`.
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--all", "--", "oct-side.txt"],
        Shape::Octopus,
    ));
}

/// The six modes on [`Shape::CrissCross`] — the only shape where
/// `--simplify-merges` *keeps* a merge.
///
/// History: `edfab1b initial` → `833f9fb base` → `0a24ba3 a` and `27e7a99 b`
/// (both fork from `base`, both edit `cc.txt` and `clash.txt`); `5b52389 cc-left
/// merge` (parents `0a24ba3 27e7a99`) resolves `clash.txt` to `a` and is
/// followed by `9efbd8f cc-left tip`; `251d57c cc-right merge` (parents
/// `27e7a99 0a24ba3`) resolves it to `b` and is followed by `9aaef3a cc-right
/// tip`. `HEAD` is `cc-left`, so `cc-right`'s two commits appear only under
/// `--all`.
///
/// Observed, `log --oneline --topo-order <mode> -- clash.txt`:
///
/// ```text
/// (default)                        0a24ba3 833f9fb
/// --full-history                   5b52389 27e7a99 0a24ba3 833f9fb
/// --full-history --simplify-merges 5b52389 27e7a99 0a24ba3 833f9fb
/// --sparse                         9efbd8f 5b52389 0a24ba3 833f9fb edfab1b
/// --dense                          0a24ba3 833f9fb
/// --simplify-by-decoration         9efbd8f 5b52389 27e7a99 0a24ba3 833f9fb
/// ```
///
/// `--simplify-merges` leaving the list alone is the entire point of this shape:
/// on [`Shape::Merged`] the same flag deleted the merge, because there the merge
/// had one relevant parent after rewriting. Here it has two — both sides really
/// did change `clash.txt` — so it is not redundant and stays. A port that
/// implements `--simplify-merges` as "drop every merge" passes `Merged` and
/// fails this; one that implements it as a no-op does the reverse.
///
/// `cc.txt`, which every commit on both sides edits:
///
/// ```text
/// (default)                        9efbd8f 5b52389 27e7a99 0a24ba3 833f9fb
/// --full-history --simplify-merges 9efbd8f 5b52389 27e7a99 0a24ba3 833f9fb
/// --sparse                         9efbd8f 5b52389 27e7a99 0a24ba3 833f9fb edfab1b
/// --first-parent                   9efbd8f 5b52389 0a24ba3 833f9fb
/// ```
///
/// `calm.txt`, written once at `base` and never touched again: `(default)`
/// prints `833f9fb`, `--sparse` prints `9efbd8f 5b52389 0a24ba3 833f9fb edfab1b`
/// — the *first-parent* spine, because the merge is TREESAME to both parents and
/// simplification therefore followed only parent 1.
///
/// Under `--all`, `clash.txt` reaches both merges:
///
/// ```text
/// --all                             0a24ba3 27e7a99 833f9fb
/// --full-history --all              5b52389 251d57c 0a24ba3 27e7a99 833f9fb
/// --full-history --simplify-merges --all
///                                   5b52389 251d57c 0a24ba3 27e7a99 833f9fb
/// --simplify-merges --all           5b52389 251d57c 0a24ba3 27e7a99 833f9fb
/// ```
///
/// The last row is `--simplify-merges` spelled *without* `--full-history`, which
/// git implies for it — it prints the same five commits as the row above and is
/// recorded here rather than filed, because the bare spelling is pinned once on
/// [`Shape::Merged`] instead: there it must print `d4cf82a` alone and so match
/// that shape's `--full-history --simplify-merges` answer rather than its
/// default one.
fn six_modes_criss_cross(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--", "cc.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "cc.txt"],
            &["log", "--oneline", "--topo-order", "--", "calm.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "calm.txt"],
            &["log", "--oneline", "--topo-order", "--all", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--all", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--all", "--", "clash.txt"],
        ],
        out,
    );
    // `--simplify-merges` implies `--full-history`, so this must equal the
    // `--full-history --simplify-merges` answer above: `d4cf82a` alone.
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--topo-order", "--simplify-merges", "--", "side.txt"],
        Shape::Merged,
    ));
}

/// The six modes on [`Shape::Renamed`], recorded because they **collapse**.
///
/// History is one line of five commits: `edfab1b initial` → `3fc09ba seed` →
/// `89b071f pure rename` → `1982909 rename with edit` → `06d06aa copy with
/// modified source` → `8aeb24d rewrite in place`. There is no merge, so the
/// merge-facing modes have nothing to decide and were verified to print the same
/// bytes as the default on every path in the shape.
///
/// Observed, `log --oneline --topo-order <mode> -- moved/alpha.txt`:
///
/// ```text
/// (default)                        89b071f
/// --full-history                   89b071f
/// --sparse                         8aeb24d 06d06aa 1982909 89b071f 3fc09ba edfab1b
/// --simplify-by-decoration         8aeb24d 89b071f
/// ```
///
/// Only the two non-merge modes move, and for reasons unrelated to merges:
/// `--sparse` stops hiding TREESAME commits, and `--simplify-by-decoration`
/// keeps `8aeb24d` because `main`/`HEAD` decorate it. `orig/gamma.txt` gives
/// `06d06aa 3fc09ba` by default and the same six-commit list under `--sparse`.
///
/// These four cases pin the collapse. They are not evidence about
/// simplification — that evidence is in the three groups above — and no further
/// mode case is spent on this shape.
fn six_modes_renamed(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "moved/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "moved/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "moved/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "moved/alpha.txt"],
        ],
        out,
    );
}

/// The six modes on [`Shape::CommitGraph`], where the answer comes out of a
/// changed-path Bloom filter rather than a tree diff.
///
/// History: five commits rewriting `cg.txt` (`2ca90a6`, `0d223e8`, `8215c26`,
/// `60ad1e7`, `333b6d9`), a never-merged fork off `main~3`, `0198521 side`
/// adding `cg-side.txt`, `011cb96 merge side` (parents `333b6d9 0198521`), then
/// `commit-graph write --reachable --changed-paths`, then `0687895 after the
/// write` — which is deliberately outside the graph.
///
/// Observed, `log --oneline --topo-order <mode> -- cg-side.txt`:
///
/// ```text
/// (default)                        0198521
/// --full-history                   011cb96 0198521
/// --full-history --simplify-merges 0198521
/// --sparse                         0687895 011cb96 0198521 333b6d9 60ad1e7 8215c26 0d223e8 2ca90a6 edfab1b
/// --dense                          0198521
/// --simplify-by-decoration         0687895 0198521
/// ```
///
/// Structurally the same answers as [`Shape::Merged`] and that is the point: the
/// Bloom filter is an *optimisation*, and the mode-by-mode list must not move
/// because it is present. A filter consulted at the merge that answers "this
/// commit did not change `cg-side.txt`" turns the `--full-history` row into the
/// default row. `--sparse` walks past the graph boundary in one direction
/// (`0687895` is not in the graph) and into it in the other, so both code paths
/// are on one line of output.
///
/// `cg.txt`, changed by five consecutive commits and by neither merge parent
/// afterwards: `--sparse` prints
/// `0687895 011cb96 333b6d9 60ad1e7 8215c26 0d223e8 2ca90a6 edfab1b` and
/// `--simplify-by-decoration` prints
/// `0687895 0198521 333b6d9 60ad1e7 8215c26 0d223e8 2ca90a6`.
fn six_modes_commit_graph(out: &mut Vec<Case>) {
    each(
        Shape::CommitGraph,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "cg-side.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "cg-side.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "cg-side.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "cg-side.txt"],
            &["log", "--oneline", "--topo-order", "--simplify-by-decoration", "--", "cg-side.txt"],
            &["log", "--oneline", "--topo-order", "--sparse", "--", "cg.txt"],
        ],
        out,
    );
}

/// `--show-pulls`: add back exactly the merges default simplification removed —
/// and only those.
///
/// The rule is narrower than "print merges under simplification". A merge is
/// shown when it is **not** TREESAME to its *first* parent but is TREESAME to
/// some later one: that is a merge which brought the path's change onto this
/// branch from elsewhere, which is what a "pull" is. A merge TREESAME to parent
/// 1 brought nothing in and stays hidden.
///
/// Observed, `log --oneline --topo-order --show-pulls -- <path>`:
///
/// ```text
/// merged       side.txt   f781761 d4cf82a      (merge added back)
/// merged       main.txt   2d98e86              (merge NOT added back)
/// octopus      oct-c.txt  dc58074 92df11d      (added back)
/// octopus      trunk.txt  ff8f0e8              (NOT added back)
/// commit-graph cg-side.txt 011cb96 0198521     (added back)
/// criss-cross  clash.txt  0a24ba3 833f9fb      (NOT added back)
/// criss-cross  clash.txt --all
///                         0a24ba3 27e7a99 833f9fb
/// ```
///
/// The two `criss-cross` rows are the discriminating ones. Both merges there
/// *do* resolve a conflict on `clash.txt`, and both are TREESAME to their own
/// first parent by construction (`cc-left merge` keeps `a`, which is `cc-a`'s
/// content; `cc-right merge` keeps `b`, which is `cc-b`'s). So `--show-pulls`
/// adds neither, and the output is identical to the default. An implementation
/// that reads `--show-pulls` as "keep merges that are TREESAME to *any* parent"
/// prints two extra commits here and is right everywhere else in this table.
///
/// `--show-pulls --full-history` on `merged side.txt` is `f781761 d4cf82a`, the
/// same as `--full-history` alone: `--show-pulls` can only add commits that
/// simplification was about to drop, and `--full-history` already kept this one.
fn show_pulls(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--show-pulls", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--show-pulls", "--", "main.txt"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--show-pulls", "--", "oct-c.txt"],
            &["log", "--oneline", "--topo-order", "--show-pulls", "--", "trunk.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--show-pulls", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--show-pulls", "--all", "--", "clash.txt"],
        ],
        out,
    );
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--topo-order", "--show-pulls", "--", "cg-side.txt"],
        Shape::CommitGraph,
    ));
}

/// `--first-parent`, alone and crossed with every mode above.
///
/// It changes two things at once, which is why it needs its own group: it
/// truncates the *traversal* to parent 1, and it makes every merge TREESAME-or-
/// not against that one parent only. On `merged side.txt` the two effects point
/// in opposite directions — the merge is no longer TREESAME to anything, so it
/// is printed, while `side commit` is now unreachable, so it is not.
///
/// Observed, `log --oneline --topo-order --first-parent <mode> -- <path>`:
///
/// ```text
/// merged      side.txt   (alone)                   f781761
/// merged      side.txt   --full-history            f781761
/// merged      side.txt   --full-history --simplify-merges
///                                                  f781761
/// merged      side.txt   --sparse                  f781761 2d98e86 edfab1b
/// merged      side.txt   --show-pulls              f781761
/// merged      side.txt   --simplify-by-decoration  f781761
/// octopus     oct-a.txt  (alone)                   dc58074
/// octopus     oct-a.txt  --full-history            dc58074
/// octopus     oct-a.txt  --sparse                  dc58074 ff8f0e8 edfab1b
/// criss-cross cc.txt     (alone)                   9efbd8f 5b52389 0a24ba3 833f9fb
/// criss-cross clash.txt  --full-history            0a24ba3 833f9fb
/// criss-cross cc.txt     --sparse                  9efbd8f 5b52389 0a24ba3 833f9fb edfab1b
/// commit-graph cg-side.txt (alone)                 011cb96
/// ```
///
/// The `merged side.txt` block is a single-commit answer under five different
/// modes and a three-commit answer under the sixth, from *one* pathspec. Every
/// wrong parent-set decision shows up as a different one of those two lists.
/// `criss-cross clash.txt --first-parent --full-history` is the reverse case:
/// `--full-history` would print the merge, `--first-parent` makes it TREESAME to
/// its only remaining parent, and the merge disappears again.
fn first_parent(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--first-parent", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--full-history", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--sparse", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--show-pulls", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--simplify-by-decoration", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--first-parent", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--sparse", "--", "oct-a.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--first-parent", "--", "cc.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--full-history", "--", "clash.txt"],
        ],
        out,
    );
}

/// `--ancestry-path`, bare and as `=<commit>`, over ranges whose endpoints
/// straddle a merge.
///
/// `--ancestry-path` needs a *range*: it keeps only commits that lie on some
/// directed path from an excluded (bottom) commit to an included (top) one. That
/// makes it the one limiting option that can put a merge *back* into a
/// path-limited walk — the merge is on the path even when it is TREESAME to the
/// parent simplification wanted to rewrite it to.
///
/// The existing corpus asks this question without a pathspec
/// (`history_query.rs`, `fixture_gaps.rs`, `plumbing_refs.rs`). Every case here
/// carries either a pathspec or another simplification mode, which is the
/// interaction those do not reach.
///
/// Observed on [`Shape::Merged`], where `main~2` is `edfab1b initial`:
///
/// ```text
/// --ancestry-path main~2..main -- side.txt              f781761 d4cf82a
/// --ancestry-path main~2..main -- main.txt              f781761 2d98e86
/// --ancestry-path --full-history main~2..main -- side.txt  f781761 d4cf82a
/// --ancestry-path --show-pulls  main~2..main -- side.txt   f781761 d4cf82a
/// --ancestry-path=side          main~2..main -- side.txt   d4cf82a
/// --ancestry-path=main~1        main~2..main              f781761 2d98e86
/// --ancestry-path side..main                            f781761
/// --ancestry-path side..main -- side.txt                <empty>
/// ```
///
/// Row 1 against the default answer for the same pathspec (`d4cf82a` alone, top
/// of this file) is the headline: `--ancestry-path` keeps a merge that plain
/// `log -- side.txt` deletes. Row 5 is its counterweight — `=side` narrows the
/// path to one through `side commit`, and once the merge is no longer required
/// to be on that path it is simplified away again. Rows 7 and 8 are the same
/// range with and without the pathspec, and differ: the merge is on the path but
/// is TREESAME to the surviving parent for `side.txt`.
///
/// On [`Shape::Octopus`], `main~1` is `ff8f0e8 main moves on`:
///
/// ```text
/// --ancestry-path main~1..main -- oct-a.txt                   dc58074
/// --ancestry-path oct-a..main -- oct-a.txt                    <empty>
/// --ancestry-path --full-history main~1..main -- oct-a.txt    dc58074
/// --ancestry-path --first-parent main~1..main -- oct-a.txt    dc58074
/// --ancestry-path=oct-a main~1..main -- :(glob)*.txt          dc58074 205fc50
/// --ancestry-path=oct-b oct-a..main                           dc58074 48e9604
/// ```
///
/// `oct-a commit` forks from `initial`, not from `main~1`, so it is not on any
/// path out of the bottom commit and drops out even though it is the only commit
/// that touched the pathspec — row 1 keeps the merge and nothing else.
///
/// On [`Shape::CrissCross`], where both endpoints of a range can sit on
/// *different* sides of the criss-cross:
///
/// ```text
/// --ancestry-path cc-a..cc-left -- clash.txt                <empty>
/// --ancestry-path cc-b..cc-left -- clash.txt                5b52389
/// --ancestry-path main..cc-left -- cc.txt      9efbd8f 5b52389 27e7a99 0a24ba3
/// --ancestry-path=cc-b main..cc-left -- cc.txt 9efbd8f 5b52389 27e7a99
/// --ancestry-path --full-history main..cc-left -- clash.txt
///                                              5b52389 27e7a99 0a24ba3
/// ```
///
/// Rows 1 and 2 are the same range shape with the bottom moved from one merge
/// base to the other, and they disagree completely: from `cc-b` the only commit
/// on a path to `cc-left` that is not TREESAME-simplified away is the merge
/// itself; from `cc-a` there is none.
fn ancestry_path(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--ancestry-path", "main~2..main", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "main~2..main", "--", "main.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "--full-history", "main~2..main", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path=side", "main~2..main", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "side..main", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--ancestry-path", "main~1..main", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "oct-a..main", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "--full-history", "main~1..main", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path=oct-a", "main~1..main", "--", ":(glob)*.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--ancestry-path", "cc-a..cc-left", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "cc-b..cc-left", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "main..cc-left", "--", "cc.txt"],
            &["log", "--oneline", "--topo-order", "--ancestry-path", "--full-history", "main..cc-left", "--", "clash.txt"],
        ],
        out,
    );
}

/// `--follow`: rename-following, which is simplification's cousin and shares its
/// plumbing.
///
/// `--follow` is not a separate walk. It is `try_to_simplify_commit()` with a
/// *mutable* pathspec: when the one path being followed shows up as the
/// destination of a rename in the commit being examined, git rewrites the
/// pathspec to the source name and keeps walking. That is why it takes exactly
/// one pathspec, why it refuses pathspec magic, and why it interacts with the
/// modes above at all.
///
/// `misc_commands.rs` owns `log --follow` with two pathspecs and
/// `log.follow=true`; `shape_reach.rs` owns bare `log --follow` on
/// `moved/alpha.txt` and `moved/beta.txt`. None of those is repeated here.
///
/// Observed on [`Shape::Renamed`], `log --oneline --topo-order --follow -- <p>`:
///
/// ```text
/// copies/gamma.txt              06d06aa 3fc09ba
/// orig/delta.txt                8aeb24d 3fc09ba
/// orig/alpha.txt                89b071f 3fc09ba
/// does/not/exist.txt            <empty>, exit 0
/// moved                         1982909 89b071f
/// ```
///
/// `orig/alpha.txt` is the pre-rename name and gives the same two commits as the
/// post-rename name, because following runs in both directions of the walk.
/// `copies/gamma.txt` reaches `3fc09ba seed` through the *copy*, which only
/// happens if copy detection is on inside the follow path. `orig/delta.txt` is
/// the rewrite: content shares nothing with its predecessor, and `--follow`
/// still reaches `seed` because the path never changed. `moved` is a directory,
/// which git accepts and does not follow — the two commits are the two that
/// wrote into it.
///
/// Crossed with the modes and with the rename threshold:
///
/// ```text
/// --follow --full-history  -- moved/alpha.txt   89b071f 3fc09ba
/// --follow --sparse        -- moved/alpha.txt   89b071f 3fc09ba
/// --follow --first-parent  -- moved/alpha.txt   89b071f 3fc09ba
/// --follow --all           -- moved/alpha.txt   89b071f 3fc09ba
/// --follow -M50%           -- moved/beta.txt    1982909 3fc09ba
/// --follow -M90%           -- moved/beta.txt    1982909
/// ```
///
/// `--sparse` collapsing to the same two commits is the one to notice: on every
/// other path in this shape `--sparse` prints all six commits, and under
/// `--follow` it does not, because following replaces the walk's simplification
/// rather than layering on top of it.
///
/// The `-M` pair is the threshold: `renames: rename with edit` rewrites 8 of 40
/// lines, which stock scores `R072`. At `-M50%` the rename is found and the walk
/// continues into `3fc09ba seed`; at `-M90%` it is not, and the walk stops. A
/// port that hard-codes the default 50% similarity passes the first and fails
/// the second.
///
/// `log --oneline --topo-order --follow -- side.txt` on [`Shape::Merged`] prints
/// `d4cf82a` — a merge in the history and no rename to find, which is the
/// combination that catches a `--follow` implementation that forgets to keep
/// simplifying.
fn follow(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--follow", "--", "copies/gamma.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "--", "orig/delta.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "--", "orig/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "--", "does/not/exist.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "--", "moved"],
            &["log", "--oneline", "--topo-order", "--follow", "--full-history", "--", "moved/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "--sparse", "--", "moved/alpha.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "-M50%", "--", "moved/beta.txt"],
            &["log", "--oneline", "--topo-order", "--follow", "-M90%", "--", "moved/beta.txt"],
        ],
        out,
    );
    out.push(Case::new(
        "log",
        &["log", "--oneline", "--topo-order", "--follow", "--", "side.txt"],
        Shape::Merged,
    ));
}

/// `--merges`, `--no-merges` and `--min-parents=`/`--max-parents=` as
/// *simplification* questions rather than as bare filters.
///
/// `history_query.rs` and `plumbing_refs.rs` already ask these with no pathspec,
/// where the parent count is whatever the object header says. With a pathspec
/// the count git tests is the one **left after `rewrite_parents()`**, and the two
/// differ. This is the group most likely to catch a port that filters before it
/// simplifies.
///
/// The clearest instance, on [`Shape::Octopus`] with `README.md` — a path no
/// commit after `initial` touches, so the four-parent merge is TREESAME to every
/// parent and is rewritten down to one:
///
/// ```text
/// --sparse             -- README.md   dc58074 ff8f0e8 edfab1b
/// --merges --sparse    -- README.md   <empty>
/// --min-parents=2 --sparse -- README.md  <empty>
/// ```
///
/// `--sparse` prints the octopus. Adding `--merges` — which is `--min-parents=2`
/// — removes it, because by the time the filter runs the commit has one parent.
/// The same holds on [`Shape::Merged`]: `--merges --sparse -- README.md` is
/// empty while `--sparse -- README.md` prints `f781761 2d98e86 edfab1b`. An
/// implementation that consults `commit->parents` before rewriting prints the
/// merge in all four of those and is otherwise indistinguishable.
///
/// The rest, with `--full-history` so the merge survives simplification and the
/// filter has something to act on:
///
/// ```text
/// merged   --merges                 -- side.txt   <empty>
/// merged   --no-merges              -- side.txt   d4cf82a
/// merged   --merges --full-history  -- side.txt   f781761
/// merged   --no-merges --full-history -- side.txt d4cf82a
/// octopus  --min-parents=2 --full-history -- oct-a.txt  dc58074
/// octopus  --max-parents=1 --full-history -- oct-a.txt  205fc50
/// octopus  --min-parents=4 --full-history -- oct-b.txt  dc58074
/// criss-cross --merges --full-history --all -- clash.txt
///                                               5b52389 251d57c
/// criss-cross --no-merges --full-history --all -- clash.txt
///                                               0a24ba3 27e7a99 833f9fb
/// ```
///
/// `merged --merges -- side.txt` being **empty** while
/// `merged --merges --full-history -- side.txt` prints the merge is the same
/// order-of-operations fact from the other direction: default simplification
/// deleted the commit before `--merges` could select it.
///
/// `--min-parents=4` on `oct-b.txt` still finds the octopus under
/// `--full-history`, so full-history really does leave all four parents on the
/// commit rather than only the relevant ones — even though the *printed* parent
/// list under `--parents` for that same walk is `dc58074 48e9604`, one entry.
fn parent_count_filters(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--merges", "--sparse", "--", "README.md"],
            &["log", "--oneline", "--topo-order", "--min-parents=2", "--sparse", "--", "README.md"],
            &["log", "--oneline", "--topo-order", "--min-parents=2", "--full-history", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--max-parents=1", "--full-history", "--", "oct-a.txt"],
            &["log", "--oneline", "--topo-order", "--min-parents=4", "--full-history", "--", "oct-b.txt"],
        ],
        out,
    );
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--merges", "--sparse", "--", "README.md"],
            &["log", "--oneline", "--topo-order", "--merges", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--no-merges", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--merges", "--full-history", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--merges", "--full-history", "--all", "--", "clash.txt"],
            &["log", "--oneline", "--topo-order", "--no-merges", "--full-history", "--all", "--", "clash.txt"],
        ],
        out,
    );
}

/// Pathspecs that are not one plain file: a directory, `:(glob)`, `:!`, and
/// `--remove-empty`.
///
/// Simplification's TREESAME test is per *pathspec*, not per file, so widening
/// the pathspec changes which parents a merge matches. On [`Shape::Octopus`],
/// `:(glob)*.txt` matches `trunk.txt` and all three `oct-*.txt`, so the merge is
/// TREESAME to **no** parent and every mode keeps it:
///
/// ```text
/// (default)                        dc58074 92df11d 48e9604 205fc50 ff8f0e8
/// --full-history --simplify-merges dc58074 92df11d 48e9604 205fc50 ff8f0e8
/// --first-parent                   dc58074 ff8f0e8
/// ```
///
/// That convergence is the finding, not a gap: a pathspec matching every side of
/// a merge is the case where "walk and filter" and real simplification agree,
/// and it is worth pinning precisely because a port that is wrong elsewhere is
/// right here. `:!README.md :!src` on the same shape was verified to produce the
/// identical three lists by matching the same four files from the other
/// direction, and is therefore not filed a second time.
///
/// On [`Shape::Merged`], `:(exclude)README.md :(exclude)src` gives
/// `f781761 d4cf82a 2d98e86` by default and `f781761 2d98e86` under
/// `--first-parent`.
///
/// Directory and glob pathspecs on [`Shape::Renamed`], which has the only
/// multi-level paths in a shape with a real path history:
///
/// ```text
/// -- orig                        8aeb24d 06d06aa 1982909 89b071f 3fc09ba
/// --full-history -- orig         8aeb24d 06d06aa 1982909 89b071f 3fc09ba
/// --full-history -- moved        1982909 89b071f
/// -- :(glob)orig/*.txt           8aeb24d 06d06aa 1982909 89b071f 3fc09ba
/// --full-history -- :(glob)**/*.txt
///                                8aeb24d 06d06aa 1982909 89b071f 3fc09ba
/// --full-history -- :!orig       06d06aa 1982909 89b071f edfab1b
/// -- :!orig :!moved              06d06aa edfab1b
/// ```
///
/// `:!orig` reaching `edfab1b initial` is the exclusion working: with `orig`
/// excluded the pathspec still matches `README.md` and `src/lib.rs`, which
/// `initial` created.
///
/// `--remove-empty` stops the walk when a path disappears from the tree. It was
/// verified to change nothing on any fixture path — `merged -- side.txt` gives
/// `d4cf82a`, `merged --full-history -- side.txt` gives `f781761 d4cf82a`,
/// `renamed -- orig/alpha.txt` gives `89b071f 3fc09ba`, and
/// `renamed --sparse -- orig/alpha.txt` gives all six commits — because no shape
/// deletes a path and then keeps committing above the deletion on the same
/// branch. The three cases filed pin those answers rather than measure the flag;
/// reaching the flag's own branch needs a shape with a `git rm` followed by more
/// commits, which is a fixture change and not a case.
fn pathspec_shapes(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", ":(glob)*.txt"],
            &["log", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", ":(glob)*.txt"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--", ":(glob)*.txt"],
        ],
        out,
    );
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", ":(exclude)README.md", ":(exclude)src"],
            &["log", "--oneline", "--topo-order", "--first-parent", "--", ":(exclude)README.md", ":(exclude)src"],
            &["log", "--oneline", "--topo-order", "--remove-empty", "--", "side.txt"],
            &["log", "--oneline", "--topo-order", "--remove-empty", "--full-history", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--topo-order", "--", "orig"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", "moved"],
            &["log", "--oneline", "--topo-order", "--full-history", "--", ":!orig"],
            &["log", "--oneline", "--topo-order", "--", ":!orig", ":!moved"],
            &["log", "--oneline", "--topo-order", "--remove-empty", "--", "orig/alpha.txt"],
        ],
        out,
    );
}

/// The same questions through the other front ends onto the same engine.
///
/// `rev-list`, `shortlog` and `whatchanged` all call `setup_revisions()` and
/// `prepare_revision_walk()`; only the emitter differs. A port that implements
/// simplification inside its `log` command rather than inside its revision
/// walker answers `log` correctly and these incorrectly.
///
/// ```text
/// merged  rev-list --count HEAD -- side.txt                     1
/// merged  rev-list --count --full-history HEAD -- side.txt      2
/// merged  rev-list --count --sparse HEAD -- side.txt            3
/// merged  rev-list --show-pulls HEAD -- side.txt        f781761 d4cf82a
/// merged  rev-list --parents --full-history --simplify-merges HEAD -- side.txt
///                                                       d4cf82a (no parent)
/// octopus rev-list --parents --full-history HEAD -- oct-a.txt
///                                                       dc58074 205fc50
///                                                       205fc50
/// octopus rev-list --parents --full-history --simplify-merges HEAD -- oct-a.txt
///                                                       205fc50
/// octopus rev-list --parents --simplify-merges --full-history --all -- :(glob)*.txt
///                            dc58074 ff8f0e8 205fc50 48e9604 92df11d
///                            92df11d / 48e9604 / 205fc50 / ff8f0e8 / a51b7b9
/// criss-cross rev-list --parents --full-history --simplify-merges --all -- clash.txt
///                            5b52389 0a24ba3 27e7a99
///                            251d57c 27e7a99 0a24ba3
///                            0a24ba3 833f9fb / 27e7a99 833f9fb / 833f9fb
/// ```
///
/// The `--count` triple is the cheapest possible witness for the three modes and
/// has no ordering to get wrong, which is what makes it worth having beside the
/// `--oneline` lists: a count that is 1 when it should be 2 localises the bug to
/// selection rather than to rendering. The two `--parents` rows on the octopus
/// are the parent-*rewriting* result printed directly — five ids on the merge
/// row when full history is kept, one commit and no merge at all once
/// `--simplify-merges` runs.
///
/// `shortlog`, which groups by author and therefore reduces to a count per
/// identity — every fixture commit has one identity, so the number is the
/// answer:
///
/// ```text
/// merged      shortlog HEAD -- side.txt                 (1)  side commit
/// merged      shortlog --full-history HEAD -- side.txt  (2)  side commit, merge side
/// merged      shortlog -s --full-history HEAD -- side.txt  2
///     (bare `shortlog -s HEAD -- side.txt`, which answers 1, is owned by
///      `diff_family.rs` and is not re-filed here)
/// criss-cross shortlog --all -- clash.txt               (3)
/// criss-cross shortlog --full-history --all -- clash.txt (5)
/// ```
///
/// `whatchanged` is the interesting one, because it does **not** behave like
/// `log`:
///
/// ```text
/// merged  whatchanged --oneline --full-history -- side.txt        d4cf82a only
/// merged  whatchanged --oneline -- side.txt                       d4cf82a only
/// octopus whatchanged --oneline --full-history --simplify-merges -- oct-a.txt
///                                                                 205fc50 only
/// renamed whatchanged --oneline --full-history -- moved/alpha.txt 89b071f only
/// ```
///
/// `log --full-history -- side.txt` prints two commits; `whatchanged` with the
/// same flags prints one. `whatchanged` suppresses the merge that `--full-history`
/// restored, so a port that implements it as an alias for `log --raw` gets an
/// extra commit here and nowhere else in this file.
fn other_front_ends(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "rev-list",
        &[
            &["rev-list", "--count", "HEAD", "--", "side.txt"],
            &["rev-list", "--count", "--full-history", "HEAD", "--", "side.txt"],
            &["rev-list", "--count", "--sparse", "HEAD", "--", "side.txt"],
            &["rev-list", "--show-pulls", "HEAD", "--", "side.txt"],
            &["rev-list", "--parents", "--full-history", "--simplify-merges", "HEAD", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--parents", "--full-history", "HEAD", "--", "oct-a.txt"],
            &["rev-list", "--parents", "--full-history", "--simplify-merges", "HEAD", "--", "oct-a.txt"],
            &["rev-list", "--parents", "--simplify-merges", "--full-history", "--all", "--", ":(glob)*.txt"],
        ],
        out,
    );
    out.push(Case::new(
        "rev-list",
        &["rev-list", "--parents", "--full-history", "--simplify-merges", "--all", "--", "clash.txt"],
        Shape::CrissCross,
    ));
    each(
        Shape::Merged,
        "shortlog",
        &[
            &["shortlog", "HEAD", "--", "side.txt"],
            &["shortlog", "--full-history", "HEAD", "--", "side.txt"],
            &["shortlog", "-s", "--full-history", "HEAD", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "shortlog",
        &[
            &["shortlog", "--full-history", "--all", "--", "clash.txt"],
        ],
        out,
    );
    each(
        Shape::Merged,
        "whatchanged",
        &[
            &["whatchanged", "--i-still-use-this", "--oneline", "--", "side.txt"],
            &["whatchanged", "--i-still-use-this", "--oneline", "--full-history", "--", "side.txt"],
        ],
        out,
    );
    out.push(Case::new(
        "whatchanged",
        &["whatchanged", "--i-still-use-this", "--oneline", "--full-history", "--simplify-merges", "--", "oct-a.txt"],
        Shape::Octopus,
    ));
}

/// `log --graph`, where the drawn rows have to reflect the parent set
/// simplification left behind rather than the one in the object header.
///
/// This is the cheapest place for a port to be caught being right about the
/// commit list and wrong about the graph: `graph.c` is fed
/// `commit->parents` *after* `rewrite_parents()`, so a merge that survived
/// simplification with one relevant parent draws as a plain `*` with no `|\`
/// and no lane.
///
/// Observed on [`Shape::Merged`], `log --graph --oneline --topo-order`:
///
/// ```text
/// -- side.txt                            * d4cf82a
/// --full-history -- side.txt             * f781761
///                                        * d4cf82a
/// --full-history --simplify-merges -- side.txt
///                                        * d4cf82a
/// --sparse -- README.md                  * f781761
///                                        * 2d98e86
///                                        * edfab1b
/// ```
///
/// `f781761` is a two-parent merge and draws with no merge row at all in both
/// places it appears, because for that pathspec it has one parent left.
///
/// On [`Shape::Octopus`], the four-parent merge under three different modes:
///
/// ```text
/// --full-history -- oct-a.txt      * dc58074 / * 205fc50
/// --sparse -- oct-a.txt            * dc58074 / * 205fc50 / * edfab1b
/// --show-pulls -- oct-a.txt        * dc58074 / * 205fc50
/// --full-history --simplify-merges --all -- :(glob)*.txt
///     *---.   dc58074
///     |\ \ \
///     | | | * 92df11d
///     | | * 48e9604
///     | * 205fc50
///     * ff8f0e8
///     * a51b7b9
/// ```
///
/// The last row is the only one that draws the octopus as an octopus, and it
/// needs a pathspec that matches all four sides to get there. Everything above
/// it is the same commit flattened to one column.
///
/// On [`Shape::CrissCross`] under `--all`, where the merge rows have to survive:
///
/// ```text
/// --all -- clash.txt                  * 0a24ba3 / | * 27e7a99 / |/ / * 833f9fb
/// --full-history --all -- clash.txt   *   5b52389 with |\, then the
///                                     | |/| / |/  / |/|  reflow rows around
///                                     251d57c, then 0a24ba3, 27e7a99, 833f9fb
/// --full-history --simplify-merges --all -- clash.txt   identical to the above
/// ```
fn graph_rows(out: &mut Vec<Case>) {
    each(
        Shape::Merged,
        "log",
        &[
            &["log", "--graph", "--oneline", "--topo-order", "--", "side.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--", "side.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--", "side.txt"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "log",
        &[
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--", "oct-a.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--sparse", "--", "oct-a.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--all", "--", ":(glob)*.txt"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--graph", "--oneline", "--topo-order", "--all", "--", "clash.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--all", "--", "clash.txt"],
            &["log", "--graph", "--oneline", "--topo-order", "--full-history", "--simplify-merges", "--all", "--", "clash.txt"],
        ],
        out,
    );
}

/// The refusals simplification owns, each `Case::strict` so the message is
/// compared byte for byte.
///
/// ```text
/// merged  log --oneline --ancestry-path -- side.txt
///     fatal: --ancestry-path given but there are no bottom commits   exit 128
/// merged  rev-list --ancestry-path HEAD -- side.txt
///     fatal: --ancestry-path given but there are no bottom commits   exit 128
/// renamed log --oneline --follow -- :(glob)moved/*.txt
///     fatal: pathspec magic not supported by --follow: 'glob'        exit 128
/// renamed whatchanged --i-still-use-this --oneline --follow -- moved/alpha.txt moved/beta.txt
///     fatal: --follow requires exactly one pathspec                  exit 128
/// ```
///
/// `--ancestry-path` without a range is the refusal a port is most likely to
/// miss, because the flag parses fine and the walk it describes is simply empty:
/// answering "no commits, exit 0" is the plausible wrong behaviour and git
/// treats it as a usage error instead. Asked through both front ends because the
/// check lives in `setup_revisions()`, not in either builtin.
///
/// The `--follow` pair is the mutable-pathspec precondition stated as two
/// different errors: exactly one pathspec, and no magic on it. The two-pathspec
/// refusal is asked through `whatchanged` rather than `log` because
/// `misc_commands.rs` already owns the `log` spelling.
///
/// `rev-list --follow HEAD -- moved/alpha.txt` is the fifth: `rev-list` does not
/// accept `--follow` at all and answers with its whole usage block on stderr,
/// exit 129. It is strict like the rest — the two binaries were checked to emit
/// that block byte for byte identically before the case was filed — which makes
/// it a pin on the usage text as well as on the refusal.
fn refusals(out: &mut Vec<Case>) {
    out.push(Case::strict(
        "log",
        &["log", "--oneline", "--ancestry-path", "--", "side.txt"],
        Shape::Merged,
    ));
    out.push(Case::strict(
        "rev-list",
        &["rev-list", "--ancestry-path", "HEAD", "--", "side.txt"],
        Shape::Merged,
    ));
    out.push(Case::strict(
        "log",
        &["log", "--oneline", "--follow", "--", ":(glob)moved/*.txt"],
        Shape::Renamed,
    ));
    out.push(Case::strict(
        "whatchanged",
        &["whatchanged", "--i-still-use-this", "--oneline", "--follow", "--", "moved/alpha.txt", "moved/beta.txt"],
        Shape::Renamed,
    ));
    out.push(Case::strict(
        "rev-list",
        &["rev-list", "--follow", "HEAD", "--", "moved/alpha.txt"],
        Shape::Renamed,
    ));
}
