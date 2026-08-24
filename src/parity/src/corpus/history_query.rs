//! Differential corpus cases for the history_query subsystem: the verbs that
//! *answer questions about* history without changing it.
//!
//! Scope: `merge-base`, `name-rev`, `range-diff`, `show-branch`, `cherry`,
//! `rev-list`'s traversal-*selection* flags, `shortlog`, `whatchanged` and
//! `merge-tree`. These are the commands scripts and tooling call constantly and
//! that a port is most tempted to answer approximately, because a read-only
//! verb leaves no state behind for the probe to catch: their whole contract is
//! stdout and an exit code. That is also why the shape matters more here than
//! anywhere else in the corpus — a `merge-base` on a one-commit history, or a
//! `show-branch` on a repository with one branch, measures argument parsing and
//! nothing else.
//!
//! # Which shape supplies which topology
//!
//! * [`Shape::Octopus`] — the only shape with a commit that has more than two
//!   parents (`main` = `octopus merge`, parents `main~1`, `oct-a`, `oct-b`,
//!   `oct-c`), plus `oct-side` forked from the root and never merged. It is the
//!   only place `--parents`/`--children` print more than two ids on a row,
//!   `--min-parents=3` selects anything, `merge-base --octopus` differs from
//!   two-way `merge-base`, `whatchanged -m` emits four `(from …)` blocks, and
//!   `show-branch` draws more than two columns. It is also the only shape where
//!   `show-branch --topo-order` and `--date-order` disagree: every commit in
//!   every fixture carries the same pinned timestamp (`env::FIXED_DATE`), so
//!   date order degenerates to insertion order and only a shape with a real
//!   fork exposes the difference.
//! * [`Shape::MergeableDirty`] — eight branches over two bases. `div-*` fork
//!   from `main~1`, `ff-*` from `main`, and the pairs `div-cold`/`ff-cold`,
//!   `div-hot`/`ff-hot` make the *same* one-line change to the same file with a
//!   different payload. That is the only near-identical pair of patches in any
//!   fixture, which makes it the only place `range-diff` matches two commits
//!   instead of listing both as unmatched — see the `range_diff` block for the
//!   `--creation-factor` this needs. `div-hot` vs `ff-hot` is also the only
//!   two-branch *content* conflict with a real merge base, which is what
//!   `merge-tree`'s old three-argument mode needs to print conflict markers.
//! * [`Shape::BehindRemote`] — the only shape with remote-tracking refs and the
//!   only one with a configured upstream, so it is the only one where bare
//!   `cherry`, `--remotes=`, and a `--fork-point` whose reflog has been rewound
//!   are reachable at all.
//! * [`Shape::Packed`] — nine commits of one 400-line file. The only history
//!   long enough for `--bisect*` to report a non-degenerate midpoint and the
//!   only one with enough objects for `--filter=`/`--disk-usage`/`--objects-edge`
//!   to print something a wrong implementation could get wrong.
//! * [`Shape::Attributes`] — the **only** shape with more than one author
//!   identity: three commits carry `--author=` overrides (`Old Name`,
//!   `Alias Name`, `Typo Name`) while the committer stays `zvcs parity`, and a
//!   `.mailmap` rewrites all three. Everywhere else every commit has one
//!   identity, so `shortlog`'s grouping, sorting and `-e` output are all
//!   constant and measure nothing.
//! * [`Shape::Renamed`] — rename/copy/rewrite detection, which is what
//!   `whatchanged`'s `diff.renames` configuration switches on.
//!
//! # Fixture constraints these cases work around
//!
//! * **No unrelated histories.** Every shape starts from the same `initial`
//!   commit — its id is `edfab1b7…` in all of them, because the seed content,
//!   message and the pinned identity/clock are identical (`fixture.rs` `build`,
//!   `env::harden`). So `merge-base` on histories with no common ancestor (exit
//!   1, no output) is unreachable, and so is `merge-tree
//!   --allow-unrelated-histories` doing anything an ordinary merge would not.
//!   The refusals that *are* reachable are argument-arity and option-conflict
//!   errors, plus `--is-ancestor` answering "no", which is exit 1 rather than an
//!   error.
//! * **No criss-cross merges**, so `merge-base --all` never prints two ids.
//!   Two branches would have to be merged into each other in both directions to
//!   produce a second base and no shape does that. `--all` is still worth
//!   asking, because an implementation that prints the wrong single base fails
//!   it — but it cannot distinguish "returns one base" from "returns all bases".
//! * **No cherry-picked commit.** `git cherry` marks a commit `-` when a commit
//!   with the same patch id exists on the upstream side. No shape carries the
//!   same patch twice: `div-*`/`ff-*` differ by a word, `Conflicted`'s two sides
//!   write different content to one path, and `Merged`'s side branch is merged
//!   rather than replayed. So the `-` branch of `builtin/log.c:cherry()` is
//!   **unmeasured by this corpus**, and every `cherry` case below exercises the
//!   `+` branch and the traversal that feeds it. Reaching `-` needs a fixture
//!   whose history contains a `cherry-pick`; that is a shape change, not a case.
//! * **No commit-graph.** No shape writes `.git/objects/info/commit-graph`
//!   (`fixture.rs` never runs `commit-graph write`, and the one shape with an
//!   `objects/info` directory — `Packed` — has only `packs` in it). So
//!   `core.commitGraph=true|false` selects between two code paths that read the
//!   same loose/packed commits and cannot change an answer here. It is recorded
//!   rather than faked: a case pinning a setting with no subject would report
//!   agreement about nothing.
//! * **One timestamp for every commit.** `--date-order` and
//!   `--author-date-order` can only differ from each other, or from
//!   `--topo-order`, where the *insertion* order the fixture built differs from
//!   the topology — which is why those are asked on `Octopus` rather than on a
//!   linear shape.
//!
//! Ids that appear literally in argv or stdin below are the shared root commit
//! (`edfab1b7…`) and the shared `README.md` blob (`9741694d…`). Both are
//! identical in every shape, for the reason given above. Nothing here
//! hard-codes a shape-specific id.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    merge_base(out);
    name_rev(out);
    range_diff(out);
    show_branch(out);
    cherry(out);
    rev_list_selection(out);
    rev_list_objects(out);
    shortlog_grouping(out);
    whatchanged(out);
    merge_tree(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// The `README.md` blob, present and identical in every shape. A well-formed
/// object id of the wrong type, which is a different answer from an id nothing
/// has.
const README_BLOB: &str = "9741694d75caeb49d3b7c1f59451c0c56bf6216c";

/// `merge-base`: the five modes of `builtin/merge-base.c`, each on a topology
/// where its answer differs from the others'.
///
/// What a port gets wrong without these: `--octopus` is not `merge-base` applied
/// pairwise (it reduces with `reduce_heads()`), `--independent` prints the
/// *input* revisions that nothing else reaches rather than any ancestor, and
/// `--is-ancestor` reports through the exit code with no output at all — an
/// implementation that prints the base and exits 0 passes every stdout
/// comparison and is still wrong in both directions.
fn merge_base(out: &mut Vec<Case>) {
    // Three or more parents. `main` is the octopus merge, so `HEAD^2`…`HEAD^4`
    // are the three merged branch tips and `HEAD^` is the trunk they joined.
    each(
        Shape::Octopus,
        "merge-base",
        &[
            &["merge-base", "--octopus", "oct-a", "oct-b", "oct-c"],
            // Three revisions without `--octopus`: the two-way base of the first
            // against each of the rest, which is a different reduction.
            &["merge-base", "HEAD^", "HEAD^2", "HEAD^3"],
            &["merge-base", "--all", "HEAD^2", "HEAD^3"],
            // `--independent` keeps the octopus itself and the unmerged lane and
            // drops the three tips it already contains.
            &["merge-base", "--independent", "HEAD", "oct-a", "oct-b", "oct-c", "oct-side"],
            &["merge-base", "--independent", "oct-a", "oct-b", "oct-side"],
            // Exit 0 and exit 1, both with empty stdout.
            &["merge-base", "--is-ancestor", "oct-a", "HEAD"],
            &["merge-base", "--is-ancestor", "oct-side", "HEAD"],
        ],
        out,
    );

    // A tag against a branch: the argument is peeled to a commit before the
    // walk, and the annotated tag has an object of its own to peel through.
    each(
        Shape::Branched,
        "merge-base",
        &[
            &["merge-base", "v0.2.0", "feature"],
            &["merge-base", "--octopus", "main", "feature", "v0.1.0", "v0.2.0"],
        ],
        out,
    );

    // `--fork-point` reads the *reflog* of its first argument rather than the
    // commit graph (`builtin/merge-base.c:handle_fork_point` →
    // `get_fork_point`). `main`'s reflog still holds the entry `origin/main`
    // was fetched over, so the fork point is found; `div` was rewound with
    // `reset --hard` after being pushed, so no reflog entry of `origin/div` is
    // an ancestor of `div` and git exits 1 with no output — the answer a port
    // that ignores reflogs and falls back to a plain merge base cannot give.
    each(
        Shape::BehindRemote,
        "merge-base",
        &[
            &["merge-base", "--fork-point", "origin/main", "main"],
            &["merge-base", "--fork-point", "origin/div", "div"],
            &["merge-base", "--independent", "main", "origin/main", "origin/div", "div"],
        ],
        out,
    );

    // Refusals. `--is-ancestor` is arity-checked before the walk and `--all` is
    // rejected against it by name, so both messages are the contract; the
    // three-argument `--fork-point` falls through to the usage block instead,
    // which is a third distinct failure shape.
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--is-ancestor", "main", "feature", "v0.1.0"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--all", "--is-ancestor", "main", "feature"],
        Shape::Branched,
    ));
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--fork-point", "main", "feature", "v0.1.0"],
        Shape::Branched,
    ));
}

/// `name-rev`: naming a commit by the ref that reaches it, plus the `~n`/`^n`
/// suffix that says how.
///
/// What a port gets wrong without these: the *name* depends on which refs are
/// eligible, and every one of `--tags`, `--refs=`, `--exclude=` and the shape's
/// own ref set changes the eligible set — restricting it can change `main` into
/// `tags/v0.1.0`, into `remotes/origin/div~1`, or into `undefined`.
/// `--refs=refs/tags/v0.1.0` and `--refs=v0.*` name the *same* commit
/// differently (`tags/v0.1.0` vs `v0.1.0`) because `builtin/name-rev.c` strips
/// only the part of the ref prefix the pattern did not itself supply.
fn name_rev(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "name-rev",
        &[
            // Tag-only naming, including the `~1` step to the root.
            &["name-rev", "--tags", "--all"],
            &["name-rev", "--refs=refs/tags/v0.1.0", "HEAD"],
            &["name-rev", "--refs=v0.*", "--all"],
            // Excluding the branch that reaches the tip leaves it `undefined`
            // while the rest keep tag names.
            &["name-rev", "--exclude=refs/heads/feature", "--all"],
            // A well-formed id of the wrong type: named `undefined`, not an error.
            &["name-rev", README_BLOB],
        ],
        out,
    );

    // No tag reaches anything here, so `--refs=refs/tags/*` is the reachable way
    // to ask about a commit no eligible ref names — the `undefined` output and,
    // under `--no-undefined`, the refusal that replaces it. Both refusals below
    // print a partial line to stdout *before* dying, which is why they are worth
    // comparing byte for byte on both streams.
    each(
        Shape::Merged,
        "name-rev",
        &[&["name-rev", "--refs=refs/tags/*", "HEAD"], &["name-rev", "--name-only", "HEAD^2"]],
        out,
    );
    out.push(Case::strict(
        "name-rev",
        &["name-rev", "--no-undefined", "--refs=refs/tags/*", "HEAD"],
        Shape::Merged,
    ));
    out.push(Case::strict("name-rev", &["name-rev", "--no-undefined", README_BLOB], Shape::Branched));

    // Second-parent naming. With the branch tips eligible, `oct-a` names itself;
    // with them excluded the same commit becomes `main^2` — the `^n` suffix that
    // only a merge past the second parent can produce.
    each(
        Shape::Octopus,
        "name-rev",
        &[
            &["name-rev", "--name-only", "HEAD^2", "HEAD^3", "HEAD^4"],
            &["name-rev", "--exclude=refs/heads/oct-*", "--all"],
        ],
        out,
    );

    // Remote-tracking refs are eligible and print with their `remotes/` prefix;
    // restricting to them renames a local branch through the remote's history.
    each(
        Shape::BehindRemote,
        "name-rev",
        &[&["name-rev", "--all"], &["name-rev", "--refs=refs/remotes/*", "main"]],
        out,
    );

    // `--annotate-stdin` rewrites ids *in place inside arbitrary text*, which is
    // a different output path from the one-id-per-line form: the surrounding
    // words have to survive and the id is replaced by `id (name)`, or by the
    // bare name under `--name-only`.
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin"],
        Shape::Branched,
        b"the root is edfab1b71619a22120a8da1a3d85d68e0200290a in every shape\nand this line has none\n",
    ));
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin", "--name-only"],
        Shape::Branched,
        b"edfab1b71619a22120a8da1a3d85d68e0200290a trailing words\n",
    ));
    // An id of the wrong type and an id nothing stores, in one stream: neither
    // is annotated and both lines have to come back unchanged.
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin"],
        Shape::Merged,
        b"9741694d75caeb49d3b7c1f59451c0c56bf6216c blob\n4b825dc642cb6eb9a060e54bf8d69288fbee4904 empty tree\n",
    ));

    // `--all` names the whole object store and takes no revision list; asking
    // for both is rejected before any naming happens.
    out.push(Case::strict("name-rev", &["name-rev", "--all", "HEAD"], Shape::Branched));
}

/// `range-diff`: comparing two patch series.
///
/// The block that matters is the *matched* one. `range-diff` builds a cost
/// matrix over patch ids and diff sizes and only prints its interleaved
/// diff-of-diffs when two commits pair up (`range-diff.c:get_correspondences`);
/// with everything unmatched it degenerates to two lists and a port that never
/// implements the matrix passes. `div-cold`/`ff-cold` are the only pair of
/// commits in any fixture whose patches differ by one word, and even they need
/// `--creation-factor=100` to pair, because their subjects differ too — the cost
/// includes the commit message. Under it the output carries the
/// `## Metadata ##` / `## Commit message ##` / `## <path> ##` sections, the `!`
/// marker, and a `--stat` that renames `a => b`.
fn range_diff(out: &mut Vec<Case>) {
    // Matched pair, then the same pair through every output mode.
    each(
        Shape::MergeableDirty,
        "range-diff",
        &[
            &["range-diff", "--creation-factor=100", "main~1..div-cold", "main..ff-cold"],
            &["range-diff", "--creation-factor=100", "--no-dual-color", "main~1..div-cold", "main..ff-cold"],
            &["range-diff", "--creation-factor=100", "-s", "main~1..div-cold", "main..ff-cold"],
            &["range-diff", "--creation-factor=100", "--stat", "main~1..div-cold", "main..ff-cold"],
            &["range-diff", "--creation-factor=100", "--left-only", "main~1..div-cold", "main..ff-cold"],
            &["range-diff", "--creation-factor=100", "-U0", "main~1..div-cold", "main..ff-cold"],
            // The three-argument form: `<base> <rev1> <rev2>` expands to
            // `base..rev1` and `base..rev2`, which here adds `main moves` to the
            // right side and shifts the matched commit to index 2.
            &["range-diff", "--creation-factor=100", "main~1", "div-cold", "ff-cold"],
            // The same ranges at the default factor: no pair, two lists.
            &["range-diff", "main~1..div-cold", "main..ff-cold"],
        ],
        out,
    );
    // Abbreviation width inside the two id columns comes from `core.abbrev`.
    out.push(
        Case::new(
            "range-diff",
            &["range-diff", "--creation-factor=100", "main~1..div-cold", "main..ff-cold"],
            Shape::MergeableDirty,
        )
        .with_config(&[("core.abbrev", "12")]),
    );

    // Commits that are *identical* on both sides: the `=` marker, and the
    // renumbering that comes with a prefix appearing on only one side.
    out.push(Case::new("range-diff", &["range-diff", "main~3..main", "main~4..main"], Shape::Packed));
    out.push(Case::new("range-diff", &["range-diff", "main~1..main", "main~1..feature"], Shape::Branched));

    // Two branches that diverged in both directions: one commit on each side,
    // nothing matching, which is the `<`/`>` unmatched pair.
    each(
        Shape::BehindRemote,
        "range-diff",
        &[&["range-diff", "div...origin/div"]],
        out,
    );
}

/// `show-branch`: the column matrix.
///
/// What a port gets wrong without `Octopus`: the header assigns one column per
/// named ref and the body marks each commit `+`/`*`/`-` per column, with `-`
/// reserved for a *merge* — so a two-branch fixture never exercises the third
/// column, the merge marker, or the `[main^]`/`[main^4]` fallback names for
/// commits no ref points at. It is also the only shape where `--topo-order` and
/// `--date-order` produce different row orders (every fixture commit shares one
/// timestamp, so date order is insertion order).
fn show_branch(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "show-branch",
        &[
            &["show-branch"],
            &["show-branch", "--all"],
            &["show-branch", "--topics", "--all"],
            &["show-branch", "--sha1-name", "--all"],
            &["show-branch", "--merge-base", "--all"],
            &["show-branch", "--independent", "--all"],
            &["show-branch", "--topo-order", "--all"],
            &["show-branch", "--date-order", "--all"],
            // A named subset: the columns are the three arguments, but the rows
            // still include the commits only reachable through the merge, which
            // is where the `[main^3]`/`[main^4]` names come from.
            &["show-branch", "main", "oct-a", "oct-side"],
        ],
        out,
    );
    // `--sha1-name` prints ids at the configured abbreviation width.
    out.push(
        Case::new("show-branch", &["show-branch", "--sha1-name", "--all"], Shape::Octopus)
            .with_config(&[("core.abbrev", "12")]),
    );

    // Remote-tracking refs get their own selector, and `-a` mixes both
    // namespaces into one column set.
    each(
        Shape::BehindRemote,
        "show-branch",
        &[
            &["show-branch", "-r"],
            &["show-branch", "-a"],
            // `--reflog=<n>` replaces the ref columns with reflog entries, which
            // is a different name source *and* a different label format — the
            // header carries a relative date the ref form never prints.
            &["show-branch", "--reflog=2", "main"],
        ],
        out,
    );

    // Detached HEAD is a column with no branch name: `--all` alone does not
    // include it and prints a single row, `--current` adds it as `[HEAD]`.
    each(
        Shape::Detached,
        "show-branch",
        &[&["show-branch", "--all"], &["show-branch", "--current"]],
        out,
    );
}

/// `cherry`: which commits on `head` are not already upstream, by patch id.
///
/// Only the `+` branch is reachable — see the module doc for why no fixture
/// carries the same patch twice. What these still measure is the traversal that
/// feeds it and the argument defaults: with no arguments `cherry` resolves the
/// current branch's *upstream* and refuses if there is none, with one argument
/// it takes `HEAD` as the head, and with three the third is a limit that bounds
/// the walk.
fn cherry(out: &mut Vec<Case>) {
    each(
        Shape::BehindRemote,
        "cherry",
        &[
            // No arguments at all: `main` has an upstream and is ahead of
            // nothing, so the answer is empty output and exit 0.
            &["cherry"],
            &["cherry", "origin/div", "div"],
            &["cherry", "-v", "origin/div", "div"],
            // The reverse direction is a different walk with a different answer.
            &["cherry", "-v", "div", "origin/div"],
        ],
        out,
    );
    // A three-argument call, where the third bounds the walk.
    out.push(Case::new("cherry", &["cherry", "-v", "main~1", "div-cold", "main"], Shape::MergeableDirty));
    out.push(Case::new("cherry", &["cherry", "-v", "oct-a", "main"], Shape::Octopus));
    // `core.abbrev` does not reach `cherry`: it prints full ids unless `--abbrev`
    // is given. Pinning that is the point — an implementation that routes every
    // id through the configured width fails here and nowhere else.
    out.push(
        Case::new("cherry", &["cherry", "-v", "main", "feature"], Shape::Branched)
            .with_config(&[("core.abbrev", "16")]),
    );
    // No upstream configured, no arguments: the refusal is the contract.
    out.push(Case::strict("cherry", &["cherry"], Shape::Branched));
}

/// `rev-list` traversal *selection*: which commits come back, in what order,
/// and with what decoration — as opposed to the formatting flags the corpus
/// already covers.
///
/// What a port gets wrong without a multi-parent shape: `--parents` prints every
/// parent on the row, `--children` inverts the graph and prints five children
/// for the root, `--first-parent` walks only parent 1 while `--boundary` then
/// prints the parents it skipped with a `-` prefix, and `--min-parents=3`
/// selects exactly the octopus. On two-parent history every one of those is
/// indistinguishable from a simpler wrong answer.
fn rev_list_selection(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "rev-list",
        &[
            &["rev-list", "--parents", "HEAD"],
            &["rev-list", "--children", "--all"],
            &["rev-list", "--boundary", "HEAD^..HEAD"],
            &["rev-list", "--first-parent", "--boundary", "HEAD"],
            &["rev-list", "--min-parents=3", "--all"],
            &["rev-list", "--merges", "--all"],
            // `--ancestry-path=<commit>` restricts to paths through one commit,
            // which on an octopus keeps one branch tip and drops two.
            &["rev-list", "--ancestry-path=oct-a", "HEAD~1..HEAD"],
            &["rev-list", "--topo-order", "--all"],
            &["rev-list", "--author-date-order", "--all"],
            &["rev-list", "--reverse", "--topo-order", "--all"],
            &["rev-list", "--no-walk", "--all"],
        ],
        out,
    );

    // Symmetric difference: which side each commit came from, whether an
    // equivalent patch exists on the other side, and the two-column `--count`.
    each(
        Shape::BehindRemote,
        "rev-list",
        &[
            &["rev-list", "--left-right", "main...origin/main"],
            &["rev-list", "--left-right", "--boundary", "main...origin/main"],
            &["rev-list", "--count", "--left-right", "div...origin/div"],
            &["rev-list", "--cherry-mark", "div...origin/div"],
            &["rev-list", "--cherry-pick", "--left-right", "div...origin/div"],
            &["rev-list", "--remotes=origin", "--count"],
        ],
        out,
    );

    // Ref-set selection. `--branches=`/`--tags=`/`--glob=` take a *shell glob*
    // and, per `revision.c`, append `/*` to a pattern that has no wildcard —
    // which is why `--tags=v0.1.0` matches nothing while `--tags=v0.*` matches
    // both tags. An implementation that treats the pattern as a literal ref name
    // gets the first of those backwards.
    each(
        Shape::MergeableDirty,
        "rev-list",
        &[
            &["rev-list", "--exclude=refs/heads/div-*", "--all", "--count"],
            &["rev-list", "--glob=refs/heads/ff-*", "--count"],
            &["rev-list", "--branches=div-*", "--count"],
            &["rev-list", "--no-walk", "--branches"],
        ],
        out,
    );
    each(
        Shape::Branched,
        "rev-list",
        &[&["rev-list", "--tags=v0.1.0", "--count"], &["rev-list", "--tags=v0.*", "--count"]],
        out,
    );

    // Bisection over the only history long enough for it to have a midpoint.
    // `--bisect-vars` prints a shell fragment, `--bisect-all` prints every
    // candidate with its distance and any ref decoration.
    each(
        Shape::Packed,
        "rev-list",
        &[
            &["rev-list", "--bisect", "HEAD"],
            &["rev-list", "--bisect-vars", "HEAD"],
            &["rev-list", "--bisect-all", "HEAD"],
        ],
        out,
    );

    // `--parents` and `--children` are mutually exclusive by name in
    // `revision.c`; a port that simply prints both columns passes every other
    // case in this block and fails this one.
    out.push(Case::strict("rev-list", &["rev-list", "--parents", "--children", "--all"], Shape::Octopus));
}

/// `rev-list --objects`: the object walk, its filters, and its size accounting.
///
/// Separated from the commit walk because it is a different traversal — trees
/// and blobs are enumerated with their paths — and because only `Packed` has
/// enough objects for the answers to differ from each other. `--filter=blob:none`
/// and `--filter=tree:0` omit different sets, and `--filter-print-omitted`
/// prints exactly what was dropped with a `~` prefix, so the two filters produce
/// two different omitted lists over the same commits.
fn rev_list_objects(out: &mut Vec<Case>) {
    each(
        Shape::Packed,
        "rev-list",
        &[
            &["rev-list", "--objects", "--filter=blob:none", "--filter-print-omitted", "HEAD"],
            &["rev-list", "--objects", "--filter=tree:0", "--filter-print-omitted", "HEAD"],
            // `--objects-edge` prints the uninteresting boundary commits with a
            // `-` prefix ahead of the objects, which plain `--objects` never does.
            &["rev-list", "--objects-edge", "main~2..main"],
            &["rev-list", "--in-commit-order", "--objects", "main~1..main"],
            &["rev-list", "--missing=allow-any", "--objects", "HEAD"],
            &["rev-list", "--disk-usage", "--all"],
        ],
        out,
    );
}

/// `shortlog`: grouping commits by identity.
///
/// `Attributes` is the only shape with more than one author — three commits
/// carry `--author=` overrides while the committer stays pinned — and it also
/// carries the `.mailmap` that rewrites two of those authors into one
/// `Proper Name` and gives the third a canonical address. So it is the only
/// shape where `--group=author` and `--group=committer` disagree, where `-n`
/// reorders anything, where `-e` prints an address the commit does not contain,
/// and where the count column has more than one row.
///
/// The second block is the *other* usage: `shortlog` reading `git log
/// --pretty=short` output on stdin instead of walking. It parses `Author:`
/// lines and indented subjects, and it does not sort within a group — the
/// stream's own order is what survives, which is why the fixture stdin below
/// interleaves two identities.
fn shortlog_grouping(out: &mut Vec<Case>) {
    each(
        Shape::Attributes,
        "shortlog",
        &[
            &["shortlog", "HEAD"],
            &["shortlog", "-n", "HEAD"],
            &["shortlog", "-e", "-n", "HEAD"],
            &["shortlog", "--group=committer", "-s", "HEAD"],
            // `-w<width>,<indent1>,<indent2>` wraps the subject lines, which is
            // the only place `shortlog` reflows text at all.
            &["shortlog", "-w20,2,4", "HEAD"],
            &["shortlog", "--format=%h%x20%s", "HEAD"],
            &["shortlog", "-sn", "HEAD", "--", "docs"],
        ],
        out,
    );

    const LOG_SHORT: &[u8] = b"commit 1111111111111111111111111111111111111111\n\
Author: Alpha One <a@example.invalid>\n\
\n    first subject\n\n\
commit 2222222222222222222222222222222222222222\n\
Author: Beta Two <b@example.invalid>\n\
\n    second subject\n\n\
commit 3333333333333333333333333333333333333333\n\
Author: Alpha One <a@example.invalid>\n\
\n    third subject\n";
    out.push(Case::with_stdin("shortlog", &["shortlog"], Shape::Linear, LOG_SHORT));
    out.push(Case::with_stdin("shortlog", &["shortlog", "-sn"], Shape::Linear, LOG_SHORT));
    // `--group=committer` over a stream that has no `Commit:` lines: every
    // commit falls out of the grouping and the output is empty.
    out.push(Case::with_stdin("shortlog", &["shortlog", "--group=committer"], Shape::Linear, LOG_SHORT));
}

/// `whatchanged`: `log` with a raw diff, still shipped behind
/// `--i-still-use-this`.
///
/// The corpus already covers its output formats on `Branched`. What it could not
/// reach is the two things a *diff of a commit* depends on that a two-parent
/// linear history hides: how a merge with more than two parents is expanded, and
/// what rename detection does when there are renames to find.
///
/// `-m` prints one block per parent — four for the octopus, each headed
/// `<id> (from <parent>)` — while `-c` collapses the merge to a single entry
/// that is empty here because every path came from exactly one side.
fn whatchanged(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "whatchanged",
        &[
            &["whatchanged", "--i-still-use-this", "--oneline", "-m"],
            &["whatchanged", "--i-still-use-this", "--oneline", "-c"],
            &["whatchanged", "--i-still-use-this", "--oneline", "--first-parent"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "whatchanged",
        &[
            &["whatchanged", "--i-still-use-this", "--oneline", "-M"],
            &["whatchanged", "--i-still-use-this", "--oneline", "-C", "--find-copies-harder"],
            &["whatchanged", "--i-still-use-this", "--oneline", "-B", "--numstat"],
        ],
        out,
    );
    // The same walk under configured rename detection: `false` splits every
    // rename back into an add and a delete, and `copies` turns the
    // modified-source copy into `C100` without `-C` on the command line.
    for value in ["false", "copies"] {
        out.push(
            Case::new(
                "whatchanged",
                &["whatchanged", "--i-still-use-this", "--oneline", "--raw"],
                Shape::Renamed,
            )
            .with_config(&[("diff.renames", value)]),
        );
    }
    // The header block is `log`'s, so `log.date` and `log.abbrevCommit` reach it.
    out.push(
        Case::new("whatchanged", &["whatchanged", "--i-still-use-this", "-1"], Shape::Branched)
            .with_config(&[("log.date", "iso")]),
    );
    out.push(
        Case::new("whatchanged", &["whatchanged", "--i-still-use-this", "-1"], Shape::Branched)
            .with_config(&[("log.abbrevCommit", "true")]),
    );
}

/// `merge-tree`: merging two commits with no worktree and no index.
///
/// The strategy backends and `merge-file` belong to `merge_family.rs`; what is
/// added here is the one topology that shape has not had — a *content* conflict
/// between two branches with a real merge base, from `MergeableDirty`'s
/// `div-hot`/`ff-hot`, which both rewrite `hot.txt` from the same base line.
///
/// It matters because the two modes report a conflict differently. `--write-tree`
/// prints the tree id, then the stage 1/2/3 index entries, then the messages,
/// and exits 1; the old three-argument mode prints the conflicted *content*, so
/// `merge.conflictStyle` reaches stdout there and only there — `diff3` and
/// `zdiff3` add the `|||||||` base section that `merge` omits.
/// `merge_family.rs` pins `conflictStyle` under `--write-tree`, where it changes
/// the blob written to the object store and not a byte of the output.
fn merge_tree(out: &mut Vec<Case>) {
    each(
        Shape::MergeableDirty,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "div-hot", "ff-hot"],
            &["merge-tree", "--write-tree", "--name-only", "div-hot", "ff-hot"],
            &["merge-tree", "--write-tree", "--no-messages", "div-hot", "ff-hot"],
            &["merge-tree", "--write-tree", "-z", "div-hot", "ff-hot"],
            // Clean merge of two branches that touch different paths: tree id
            // only, exit 0.
            &["merge-tree", "--write-tree", "div-cold", "div-other"],
            // Old mode: the merge base is given explicitly and the conflicted
            // hunk is printed rather than staged.
            &["merge-tree", "main~1", "div-hot", "ff-hot"],
        ],
        out,
    );
    for style in ["diff3", "zdiff3"] {
        out.push(
            Case::new("merge-tree", &["merge-tree", "main~1", "div-hot", "ff-hot"], Shape::MergeableDirty)
                .with_config(&[("merge.conflictStyle", style)]),
        );
    }
    // `--merge-base=` overrides the computed base. On `Octopus` both branches
    // fork from the root, so naming the trunk instead still merges cleanly —
    // which pins that the override is honoured rather than recomputed.
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--merge-base=main~1", "oct-a", "oct-b"],
        Shape::Octopus,
    ));
    // `--stdin` reads one pair per line and answers each with a status-prefixed
    // record, which is a third output format again.
    out.push(Case::with_stdin(
        "merge-tree",
        &["merge-tree", "--stdin"],
        Shape::MergeableDirty,
        b"div-hot ff-hot\ndiv-cold div-other\n",
    ));
}
