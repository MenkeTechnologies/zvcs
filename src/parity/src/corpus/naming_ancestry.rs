//! Naming a commit, and answering ancestry questions about it: `describe`,
//! `name-rev`, `show-branch`, `merge-base`, and the handful of `rev-parse`
//! forms that enumerate a ref *set* rather than parse a revision.
//!
//! The four verbs are four front ends on one reachability engine. `describe`
//! walks backwards from a commit to the nearest tag; `describe --contains`
//! hands the same question to `name-rev` and walks forwards instead;
//! `show-branch --merge-base`/`--independent` and `merge-base`'s five modes are
//! two spellings of `commit.c:get_merge_bases_many` and `reduce_heads`. So the
//! sharpest thing this file can measure is not a stdout difference against
//! stock — it is **two of the four front ends contradicting each other on the
//! port while agreeing on stock**, which says the engine is right and one
//! caller is wrong, or the reverse. [`cross_verb`] is that group, and it found
//! one (see the module's "What the port gets wrong" list below).
//!
//! # How this divides territory with the five adjacent modules
//!
//! * **`tag_describe.rs`** owns `describe`'s *flag matrix on
//!   [`Shape::Branched`]* — `--tags`, `--long`, the five `--abbrev` widths,
//!   `--all`, `--exact-match`, `--candidates=0`, one `--match`, one
//!   `--exclude`, `--first-parent`, a blob, a tree — plus the no-tag fallback
//!   ladder on `Linear`/`Merged`/`Octopus`/`Detached` and `--dirty`/`--broken`
//!   on `Dirty`. Every one of its filter cases passes **one** pattern, so the
//!   accumulating list `--match` builds, the reset `--no-match` performs on it,
//!   and the union two `--exclude`s make were all unmeasured; and every one of
//!   its `--dirty` cases runs on a *dirty* tree, so the other half of that flag
//!   — the mark that must **not** appear — had no case. Those are
//!   [`describe_match_algebra`] and [`describe_dirty_negative`] here.
//!   `--debug` appears nowhere in it, and neither does any `describe.*`
//!   configuration key.
//! * **`revision_syntax.rs`** owns the revision *grammar* — `^`, `~`, `@{…}`,
//!   `:/text`, `^{}`, and the six `ref_rev_parse_rules` ambiguity cases. This
//!   file never asks what a revision *spells*; it asks what reaches what. The
//!   one place the two touch is [`Shape::AmbiguousRef`], where `top` is a
//!   branch *and* a tag *and* `refs/top`: `revision_syntax` asks which one
//!   `rev-parse top` picks, and [`cross_verb`] asks whether the name
//!   `describe --contains` *prints* for a commit is one that resolves back to
//!   that commit at all.
//! * **`history_query.rs`** owns the bulk of `merge-base`, `name-rev` and
//!   `show-branch` on [`Shape::Octopus`], [`Shape::BehindRemote`] and
//!   [`Shape::Branched`]: the five `merge-base` modes, `--fork-point` over a
//!   reflog, `name-rev`'s `--refs=`/`--exclude=`/`--no-undefined` set, and
//!   `show-branch`'s column matrix with `--topics`/`--sha1-name`/`--merge-base`
//!   /`--independent`/`--topo-order`/`--date-order`. Its own header records
//!   two fixtures it did not have: "**No criss-cross merges**, so `merge-base
//!   --all` never prints two ids" and "no unrelated histories". Both shapes
//!   exist now, and the reductions that need them —
//!   [`merge_base_reductions`], and every `show-branch` case here — run on
//!   [`Shape::CrissCross`], which that module never names.
//! * **`plumbing_refs.rs`** owns the floor: one plain `name-rev`/`merge-base`
//!   call per mode on `Branched`/`Merged`/`Linear`, including `--stdin` and
//!   `--annotate-stdin` **with stdin closed**, which is the empty-input path
//!   only. The payload-bearing forms are [`name_rev_stdin`] here.
//! * **`shape_reach.rs`** contains none of these four verbs at all.
//! * **`graft_partial.rs`** owns `name-rev`/`merge-base`/`describe` on
//!   [`Shape::Shallow`] and [`Shape::Promisor`]. Nothing here runs on either —
//!   in particular `name-rev --all` on `Shallow` is *its* case, and it is
//!   already failing (the port names two commits, `edfab1b7…` and `db3a7471…`,
//!   that `cat-file --batch-all-objects` says the shallow store does not
//!   contain). Re-spelling it here as `--name-only` would file one defect
//!   twice.
//! * **`fixture_gaps*.rs`** own the first pass over each new shape: `describe`
//!   /`show-branch`/`merge-base`/`name-rev` on `Unrelated`, `CrissCross`,
//!   `CommitGraph`, `TagChain` and `AmbiguousRef` in their simplest spellings.
//!   Everything here is a spelling they do not carry; the overlaps that matter
//!   are called out case by case.
//!
//! # What the port gets wrong, reproduced by hand before being written down
//!
//! 1. **`describe --debug` produces no output at all.** Every `--debug` case in
//!    [`describe_debug`] is `Case::strict` for that reason: the report goes to
//!    stderr, stdout is identical, and without strict comparison the flag is
//!    unmeasurable. Verified deterministic — the report names commits and
//!    counts, never a time or a path, and two stock runs are byte-identical.
//! 2. **`describe` names an annotated tag by its *ref* rather than by the name
//!    inside the tag object.** [`Shape::TagChain`]'s `light-to-tag` is a
//!    lightweight ref pointing at the tag object whose own `tag` header says
//!    `inner`. Stock prints `inner-2-g725c7d5` and warns
//!    `tag 'light-to-tag' is externally known as 'inner'`; the port prints
//!    `light-to-tag-2-g725c7d5` and is silent. Corroborated by git 2.50.1.
//! 3. **`describe --contains` with a filter passes the wrong pattern shape to
//!    `name-rev`.** On `AmbiguousRef`, `describe --contains --match top HEAD~1`
//!    is `tags/top~1` on both gits and `top~1` on the port — and `top~1` is not
//!    a longer spelling of the same thing, it is a *different commit's* name:
//!    `top` is `refs/top` under `ref_rev_parse_rules`, so `rev-parse top~1`
//!    exits 128. See [`cross_verb`] for the pair that isolates it.
//! 4. **`rev-parse --symbolic-full-name` over `--branches=<pat>`/`--tags=<pat>`
//!    prints names stock refuses to print.** Both gits emit
//!    `error: refname 'ambi' is ambiguous` on stderr and nothing on stdout; the
//!    port prints `refs/heads/ambi`. `--symbolic` and `--symbolic-full-name
//!    --all` agree, which is what localises it to the short-name round trip
//!    those two enumerators perform.
//! 5. **`name-rev` picks the lightweight tag where git picks the annotated one,
//!    and drops the `^0` that says so.** On `AmbiguousRef`, `refs/tags/ambi`
//!    (lightweight) and `refs/tags/ambi-ann` (a tag object) name the same
//!    commit. Both gits print `tags/ambi-ann~2` / `~1` / `^0`; the port prints
//!    `tags/ambi~2` / `~1` / `tags/ambi`. Two differences in one answer — the
//!    tie-break between two eligible tags, and the `^0` git appends when the
//!    naming ref is a tag *object* and therefore is not the commit itself.
//!    `fixture_gaps3.rs` owns the `name-rev --all` spelling of this shape and
//!    already fails on it; what the `--name-only` case here adds is the `^0`
//!    suffix, which appears in no other output in the corpus.
//!
//! # What is not measurable here, and why
//!
//! * **`describe --dirty` over a tag.** No shape in `fixture.rs` has both a tag
//!   and a dirty worktree — checked across all 43: the tagged shapes
//!   (`Branched`, `TagChain`, `Unrelated`, `AmbiguousRef`) are all clean, and
//!   every dirty shape is untagged. So `v0.2.0-1-g07e86d1-dirty` cannot be
//!   produced by any case, and `tag_describe.rs`'s `--dirty` cases have to use
//!   `--always`. The *negative* half is reachable and is
//!   [`describe_dirty_negative`].
//! * **`describe --broken` doing anything.** The mark appears only when
//!   `diff-index` itself fails; `Shape::Damaged`'s damage is in the ref store
//!   and the object store, and its `diff-index` succeeds. `--broken` on a clean
//!   tree is pinned instead, which is the "and must add nothing" half.
//! * **`describe --candidates=<n>` selecting between candidates.** It needs two
//!   tags at *different distances* on different lanes. No shape has that:
//!   `Branched`'s two tags sit on one commit, `TagChain`'s four peel to one
//!   commit, `AmbiguousRef`'s three sit on `HEAD`, and `Unrelated` has one.
//!   `--candidates=1` is therefore pinned only through `--debug`, where the
//!   candidate table is printed, and is honestly a weaker case than it looks.
//! * **`show-branch -g` as a pinned expectation.** Its header renders the
//!   reflog entry's age against the *wall clock*
//!   (`! [main@{0}] (2 years, 10 months ago) commit: add two`). The two sides
//!   run within milliseconds of each other so they always agree, which is why
//!   `diff_family.rs`'s `-g` and `history_query.rs`'s `--reflog=2` are sound
//!   cases — but the string is a function of today's date, so no *new* case
//!   here can add anything the existing ones do not already measure, and none
//!   is added.
//! * **The 2.55-vs-2.50 change in `describe --contains --all` filtering.**
//!   `describe --contains --all --exclude 'oct-*' HEAD^3` is `main^3` on stock
//!   2.55.0 and `oct-b` on git 2.50.1 — the *older* git ignores the filter
//!   under `--all`, and the port agrees with the older one. That is a version
//!   difference, not a port defect, and it is shipped as exactly one case
//!   ([`describe_contains_filters`]) so the harness's second oracle records it
//!   under `version-skew` rather than leaving it undocumented.
//!
//! Every object id written literally below is `edfab1b71619a22120a8da1a3d85d68e0200290a`,
//! the root commit every shape in `fixture.rs` descends from. No shape-specific
//! id is hard-coded anywhere in this file; everything else is a rev the fixture
//! resolves.

use crate::fixture::Shape;
use crate::runner::Case;

/// The root commit, identical in every shape (`fixture.rs` `build`, plus the
/// pinned identity and clock in `env::harden`). The one id this file spells
/// out.
const ROOT: &str = "edfab1b71619a22120a8da1a3d85d68e0200290a";

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    describe_debug(out);
    describe_match_algebra(out);
    describe_tag_object_name(out);
    describe_dirty_negative(out);
    describe_configured(out);
    describe_contains_filters(out);
    name_rev_always(out);
    name_rev_suffix_algebra(out);
    name_rev_namespaces(out);
    name_rev_stdin(out);
    show_branch_merges(out);
    merge_base_reductions(out);
    cross_verb(out);
    rev_parse_ref_sets(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// Push one **strict** case per argv against `shape`.
fn each_strict(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::strict(cmd, args, shape));
    }
}

/// A stdin-bearing case with stderr compared too. `Case` has a constructor for
/// each half and none for both, and the fields are public, so the two are
/// combined here rather than in every call site.
fn strict_stdin(
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    stdin: &'static [u8],
) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// `describe --debug`: the search, printed.
///
/// The flag is the only window onto `describe`'s *internals* that is part of
/// its interface — how many commits it traversed, where the search stopped, and
/// the candidate table with each tag's flag and distance. A port can produce
/// every correct name in this corpus by any means at all; `--debug` is where it
/// has to say it did the walk.
///
/// All strict, because the entire report is on stderr and stdout is unchanged
/// by the flag. Deterministic: the report contains commit ids, tag names and
/// counts, and no time, path or address — two stock runs in two directories are
/// byte-identical.
///
/// Five different reports, not five spellings of one:
///
/// * exact match — one line, `describe HEAD`, and the search never starts.
/// * a real search — `No exact match…`, `finished search at <id>`, an
///   ` annotated <distance> <name>` row, and `traversed N commits`.
/// * `--all` — the ref hit is exact, so it stops after the first line even
///   though the tag walk would not have.
/// * no names at all — the search runs and finds nothing, so the candidate
///   table and the `traversed` line are both absent and `--always` prints an
///   id.
/// * `--contains` — routed through `name-rev`, which has no debug output, so
///   the correct report is **no report**. A port that prints its own debug line
///   here fails for the opposite reason to the other four.
fn describe_debug(out: &mut Vec<Case>) {
    each_strict(
        Shape::Branched,
        "describe",
        &[
            &["describe", "--debug"],
            &["describe", "--debug", "feature"],
            &["describe", "--tags", "--debug", "feature"],
            &["describe", "--contains", "--debug", "HEAD"],
        ],
        out,
    );
    each_strict(
        Shape::TagChain,
        "describe",
        &[
            &["describe", "--tags", "--debug"],
            &["describe", "--debug", "--all"],
            // `--candidates=1` cannot change *which* candidate wins on any
            // fixture (see the module header), but under `--debug` the
            // candidate table is printed, so the accounting is at least visible.
            &["describe", "--tags", "--candidates=1", "--debug"],
        ],
        out,
    );
    // No name of any kind: the search runs to the root and the fallback fires.
    out.push(Case::strict("describe", &["describe", "--debug", "--always"], Shape::Linear));
    out.push(Case::strict("describe", &["describe", "--debug", "--all", "--always"], Shape::Octopus));
}

/// `--match`, `--no-match` and `--exclude` as a *list*, which is what they are.
///
/// `builtin/describe.c` keeps two string lists and appends to them, so the
/// flags accumulate; `--no-match` and `--no-exclude` **clear** their list rather
/// than negating the last entry. Every filter case in `tag_describe.rs` passes
/// a single pattern, under which an implementation that keeps one pattern
/// instead of a list, or that treats `--no-match` as a no-op, scores exactly
/// like git.
///
/// [`Shape::TagChain`] is the fixture this needs: six tags — `inner`, `outer`,
/// `outermost`, `light-to-tag`, `blobtag`, `treetag` — where the first four
/// peel to one commit two below `HEAD`, so which of them a filter leaves
/// standing is visible in the printed name and in nothing else.
fn describe_match_algebra(out: &mut Vec<Case>) {
    each(
        Shape::TagChain,
        "describe",
        &[
            // Two patterns, neither of which matches alone what both match
            // together.
            &["describe", "--tags", "--match", "nomatch*", "--match", "inner"],
            // `--no-match` clears everything before it: the surviving list is
            // `outer` alone, so the answer moves from `inner` to `outer`.
            &["describe", "--tags", "--match", "inner", "--no-match", "--match", "outer"],
            // …and a `--no-match` with nothing after it leaves the candidate set
            // unrestricted rather than empty.
            &["describe", "--tags", "--no-match", "--match", "inner"],
            // Three exclusions, applied together.
            &["describe", "--tags", "--exclude", "inner", "--exclude", "outer", "--exclude",
              "light-to-tag"],
            // Under `--all` the pattern is matched against the *short* name, so
            // a pattern spelled the way the output is spelled matches nothing.
            &["describe", "--all", "--match", "main", "HEAD"],
            &["describe", "--all", "--exclude", "main", "HEAD"],
        ],
        out,
    );
    // The refusal that proves the previous pair: `heads/*` is how `--all` prints
    // the answer and is not how `--match` reads it.
    out.push(Case::strict(
        "describe",
        &["describe", "--all", "--match", "heads/*", "HEAD"],
        Shape::TagChain,
    ));
}

/// The name inside the tag object, against the name of the ref that reached it.
///
/// `refs/tags/light-to-tag` and `refs/tags/inner` point at the *same* tag
/// object, whose own `tag` header says `inner`. `builtin/describe.c` reads that
/// header, uses it, and warns when the two disagree
/// (`warning: tag '<ref>' is externally known as '<embedded>'`). The port uses
/// the ref name and says nothing.
///
/// Both cases are strict: the warning is the only place git says *why* the name
/// it printed is not the ref it was asked about, and it is deterministic.
///
/// Two spellings, because they reach the candidate through different filter
/// code: one selects `light-to-tag` with `--match`, the other arrives at it by
/// excluding the three tags that would otherwise win. A port that special-cased
/// one filter would still fail the other.
///
/// Reproduced against git 2.50.1 as well, so this is a port defect and not a
/// version difference.
fn describe_tag_object_name(out: &mut Vec<Case>) {
    each_strict(
        Shape::TagChain,
        "describe",
        &[
            &["describe", "--tags", "--match", "light*"],
            &["describe", "--tags", "--exclude", "inner", "--exclude", "outer", "--exclude",
              "outermost"],
            // Two patterns whose union is one candidate, which is also the
            // narrowest form of the accumulating list in
            // [`describe_match_algebra`].
            &["describe", "--tags", "--match", "out*", "--match", "light*", "--debug"],
        ],
        out,
    );
    // Asked *by* the aliasing ref rather than *for* it: naming the tag directly
    // is the control, and it agrees.
    out.push(Case::new("describe", &["describe", "--tags", "light-to-tag"], Shape::TagChain));
}

/// `--dirty` and `--broken` where the mark must **not** appear.
///
/// `tag_describe.rs` measures both flags on [`Shape::Dirty`], where the answer
/// is always the mark; an implementation that appends unconditionally passes
/// every one of those cases. The complement needs a clean worktree, and — for
/// the untracked-only pair — a worktree that is *visibly* untidy while
/// `diff-index` still reports nothing, which is precisely what `--dirty`
/// ignores.
///
/// The tagged shapes are all clean and the dirty shapes are all untagged (see
/// the module header), so these are the tag-bearing half of the flag: the name
/// printed is a real tag name and the suffix is the whole question.
fn describe_dirty_negative(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "describe",
        &[
            &["describe", "--dirty", "feature"],
            &["describe", "--dirty=+wip", "feature"],
            &["describe", "--broken", "feature"],
            &["describe", "--dirty", "--broken", "feature"],
        ],
        out,
    );
    out.push(Case::new("describe", &["describe", "--tags", "--dirty"], Shape::TagChain));
    // Untracked files only. `Sparse` has one inside the excluded cone and
    // `Attributes` has two at the top level; `diff-index` reports neither, so
    // both must describe as clean.
    out.push(Case::new("describe", &["describe", "--always", "--dirty"], Shape::Sparse));
    out.push(Case::new("describe", &["describe", "--always", "--dirty"], Shape::Attributes));
}

/// The `describe.*` keys, which no case in the corpus set.
///
/// `tag_describe.rs`'s `configured` block reaches `describe` only through
/// `core.abbrev`; `describe.tags` and `describe.abbrev` are `describe`'s own
/// namespace and were never read. Each pair below is chosen so the setting
/// *changes the answer*, except the last, whose point is that it does not.
fn describe_configured(out: &mut Vec<Case>) {
    let d = |cfg: &[(&str, &str)], args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("describe", args, Shape::Branched).with_config(cfg));
    };
    // `describe.tags=true` is `--tags` by configuration. On this fixture the
    // answer is the same either way, so it is paired with the `TagChain` case
    // below where it is not.
    d(&[("describe.tags", "true")], &["describe", "feature"], out);
    // …and `describe.tags=false` must not undo an explicit `--tags`.
    d(&[("describe.tags", "false")], &["describe", "--tags", "feature"], out);
    // `describe.abbrev` is the width `--abbrev` would set, from configuration.
    // At zero distance no id is printed, so every case names `feature`.
    d(&[("describe.abbrev", "12")], &["describe", "feature"], out);
    d(&[("describe.abbrev", "40")], &["describe", "feature"], out);
    // Zero suppresses the suffix entirely — a different code path from a narrow
    // id, and the one `--abbrev=0` reaches from the command line.
    d(&[("describe.abbrev", "0")], &["describe", "feature"], out);
    // A value that is not a number. `core.abbrev=nonsense` makes git die with
    // the config parser's unit diagnostic (`tag_describe.rs` pins that);
    // `describe.abbrev=nonsense` does **not** — measured on stock 2.55.0, it
    // prints the ordinary seven-character answer. Two keys of the same shape
    // with two different failure modes is exactly what a port collapses.
    out.push(
        Case::strict("describe", &["describe", "feature"], Shape::Branched)
            .with_config(&[("describe.abbrev", "nonsense")]),
    );
    // The key against the flag: the flag wins.
    d(&[("describe.abbrev", "0")], &["describe", "--abbrev=16", "feature"], out);
    // On `TagChain` the default (annotated tags only) and `--tags` differ in
    // nothing, but `describe.tags` still has to reach the same walk.
    out.push(
        Case::new("describe", &["describe"], Shape::TagChain)
            .with_config(&[("describe.tags", "true")]),
    );
}

/// `describe --contains`: the forward walk, and what a filter does to it.
///
/// `--contains` does not run `describe`'s own search at all — it builds a
/// `name-rev` invocation and prints its answer, which is why the offsets come
/// out as `~n`/`^n` rather than as `-<n>-g<id>`. The pattern it builds is the
/// thing at issue: with no filter it asks for `--tags`, and with `--match`
/// /`--exclude` it asks for `refs/tags/<pattern>`. Those two pattern *shapes*
/// print different names for the same commit, because `builtin/name-rev.c`
/// strips only the prefix the pattern did not itself supply.
///
/// On [`Shape::AmbiguousRef`] that difference is not cosmetic. `top` is
/// `refs/heads/top` (at `HEAD~1`), `refs/tags/top` (at `HEAD`) **and**
/// `refs/top` (at the root), and `ref_rev_parse_rules` gives the bare name to
/// `refs/top`. So `tags/top~1` names the commit asked about and `top~1` does
/// not resolve at all: measured on stock, `rev-parse top~1` exits 128.
///
/// The port prints `top~1`. Both gits print `tags/top~1`. See [`cross_verb`]
/// for the `name-rev` half of the same question, which the port gets right.
fn describe_contains_filters(out: &mut Vec<Case>) {
    each(
        Shape::AmbiguousRef,
        "describe",
        &[
            // No filter: the pattern is the short form and the printed name has
            // no prefix. This is the control — port and stock agree.
            &["describe", "--contains", "HEAD~1"],
            // A filter that still leaves an unprefixed answer, because the
            // surviving tag's short name is what the pattern named.
            &["describe", "--contains", "--match", "ambi", "HEAD~1"],
            // The two that diverge.
            &["describe", "--contains", "--match", "top", "HEAD~1"],
            &["describe", "--contains", "--exclude", "ambi*", "HEAD~1"],
        ],
        out,
    );
    // Through a merge's second parent and across a criss-cross, where the
    // forward walk has to choose a lane.
    each(
        Shape::Octopus,
        "describe",
        &[
            &["describe", "--contains", "--all", "HEAD^3"],
            &["describe", "--contains", "--all", "--abbrev=0", "HEAD^3"],
        ],
        out,
    );
    out.push(Case::new("describe", &["describe", "--contains", "--all", "cc-a"], Shape::CrissCross));
    out.push(Case::new(
        "describe",
        &["describe", "--contains", "--all", "oct-side"],
        Shape::Octopus,
    ));
    // A commit no tag reaches, on a shape whose only tag is on an unrelated
    // root: `--contains` dies rather than printing `undefined`, which is the
    // one place it and `name-rev` disagree *on stock* (see [`cross_verb`]).
    out.push(Case::strict("describe", &["describe", "--contains", "HEAD"], Shape::Unrelated));
    // The single version-skew case this file ships, kept so the difference is
    // recorded rather than lost: stock 2.55.0 honours `--exclude` under
    // `--all` and prints `main^3`; git 2.50.1 ignores it and prints `oct-b`.
    // The port agrees with 2.50.1. The harness's second oracle is what tells
    // those two verdicts apart, and this is the case that exercises it.
    out.push(Case::new(
        "describe",
        &["describe", "--contains", "--all", "--exclude", "oct-*", "HEAD^3"],
        Shape::Octopus,
    ));
}

// ---------------------------------------------------------------------------
// name-rev
// ---------------------------------------------------------------------------

/// `--always`, which does nothing until `--no-undefined` is also given.
///
/// `builtin/name-rev.c` consults `always` only on the path where a missing name
/// would otherwise be fatal: with `--no-undefined` alone the command dies, and
/// with both it prints an abbreviated id instead. `--always` **by itself** does
/// not replace the word `undefined` — measured on stock 2.55.0:
///
/// ```text
/// $ git name-rev --always --refs=refs/tags/'*' HEAD
/// HEAD undefined
/// $ git name-rev --always --no-undefined --refs=refs/tags/'*' HEAD
/// HEAD dc58074
/// ```
///
/// A port that implements `--always` the way `describe --always` works — as an
/// unconditional fallback — passes the second line and fails the first, and no
/// case in the corpus asked for `--always` on `name-rev` at all.
///
/// [`Shape::Octopus`] has no tags, so `--refs=refs/tags/*` is the reachable way
/// to make every commit unnameable without inventing a bad id.
fn name_rev_always(out: &mut Vec<Case>) {
    each(
        Shape::Octopus,
        "name-rev",
        &[
            &["name-rev", "--always", "--refs=refs/tags/*", "HEAD"],
            &["name-rev", "--always", "--name-only", "--refs=refs/tags/*", "HEAD"],
            &["name-rev", "--always", "--no-undefined", "--refs=refs/tags/*", "HEAD"],
            &["name-rev", "--always", "--refs=refs/tags/*", "--all"],
        ],
        out,
    );
    // The refusal `--always` suppresses, so the pair is complete. Strict: the
    // message names the id it could not describe.
    out.push(Case::strict(
        "name-rev",
        &["name-rev", "--no-undefined", "--refs=refs/tags/*", "oct-a"],
        Shape::Octopus,
    ));
    // `--always` on a commit that *is* nameable must change nothing.
    out.push(Case::new("name-rev", &["name-rev", "--always", "--all"], Shape::Octopus));
}

/// The suffix algebra: `~n`, `^n`, and the compound `~n^m`.
///
/// `history_query.rs` reaches `^n` on [`Shape::Octopus`] — one merge, one hop —
/// and never reaches a name that needs both operators. [`Shape::CrissCross`]
/// does: with only `cc-left` eligible, `cc-b` is `cc-left~1^2`, because the
/// path runs one first-parent step back to the merge and then out along its
/// *second* parent. That is the form a port producing names by counting
/// `~` alone cannot spell.
///
/// The second half of the group is the prefix rule the printed name obeys —
/// `--refs=refs/heads/cc-left` prints `cc-left…` while `--refs=cc-left` prints
/// the same thing, and `--refs=refs/tags/*` prints `tags/…` while `--tags`
/// prints the bare name. `history_query.rs` states the rule and measures it for
/// one tag on `Branched`; here it is measured where the two spellings pick out
/// *different refs*, on `AmbiguousRef`.
fn name_rev_suffix_algebra(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "name-rev",
        &[
            // Every commit in the shape named through one branch: the compound
            // suffix, the plain `~n` chain, and the two tips that stay
            // `undefined` because nothing eligible reaches them.
            &["name-rev", "--refs=refs/heads/cc-left", "--all"],
            &["name-rev", "--name-only", "--refs=refs/heads/cc-left", "cc-b", "main"],
            // The same restriction spelled as a short pattern.
            &["name-rev", "--name-only", "--refs=cc-left", "cc-b"],
            // Unrestricted, the two bases name themselves and the suffix
            // disappears — the control for the pair above.
            &["name-rev", "--name-only", "cc-a", "cc-b"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "name-rev",
        &[
            // `^2`…`^4` forced by restricting to the branch that merged them.
            &["name-rev", "--refs=refs/heads/main", "--name-only", "HEAD^2", "HEAD^3", "HEAD^4"],
            &["name-rev", "--refs=main", "--name-only", "oct-c"],
            &["name-rev", "--refs=refs/heads/main", "--all"],
            // The lane that was never merged: not reachable from `main`, so it
            // is `undefined` however the pattern is spelled.
            &["name-rev", "--refs=refs/heads/main", "--name-only", "oct-side"],
        ],
        out,
    );
    each(
        Shape::AmbiguousRef,
        "name-rev",
        &[
            // The prefix rule, twice over one commit: fully-qualified pattern
            // keeps `tags/`, short pattern and `--tags` drop it.
            &["name-rev", "--name-only", "--refs=refs/tags/*", "HEAD~1"],
            &["name-rev", "--name-only", "--tags", "HEAD~1"],
            &["name-rev", "--name-only", "--refs=top", "HEAD~1"],
        ],
        out,
    );
}

/// Ref namespaces that are neither `heads/`, `tags/` nor `remotes/`.
///
/// `name-rev` walks every ref it is given, and the corpus only ever gave it the
/// three ordinary namespaces. [`Shape::NotesReplace`] has `refs/notes/commits`,
/// `refs/notes/review`, `refs/notes/other` and two `refs/replace/<oid>` entries,
/// and [`Shape::AmbiguousRef`] has a bare `refs/top`. Those refs point at note
/// commits and at replacement commits, so `--all` names objects that are not on
/// any branch — and a `refs/replace/<oid>` name embeds a full object id in the
/// output, which is the one ref name in the corpus that is itself an id.
fn name_rev_namespaces(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "name-rev",
        &[
            &["name-rev", "--all"],
            &["name-rev", "--refs=refs/replace/*", "--all"],
            &["name-rev", "--refs=refs/notes/*", "--all"],
            &["name-rev", "--exclude=refs/notes/*", "--exclude=refs/replace/*", "--all"],
            &["name-rev", "--name-only", "--refs=refs/notes/commits", "HEAD"],
        ],
        out,
    );
    each(
        Shape::AmbiguousRef,
        "name-rev",
        &[
            // `refs/top` is in no standard namespace at all, so it is eligible
            // only because `name-rev` walks whatever `--refs=` names.
            &["name-rev", "--refs=refs/top", "--all"],
            // Two tags on one commit, one lightweight and one a tag object.
            // Both gits break the tie towards the annotated one and print
            // `tags/ambi-ann^0` for the commit it points at — the `^0` says the
            // naming ref is not itself that commit. The port answers
            // `tags/ambi` and drops the suffix. `fixture_gaps3.rs` owns the
            // plain `--all` spelling; this is the `--name-only` rendering,
            // which is where the `^0` is unmistakable.
            &["name-rev", "--all", "--name-only"],
        ],
        out,
    );
}

/// The two stdin modes, with a payload.
///
/// `plumbing_refs.rs` calls both `--stdin` and `--annotate-stdin` with stdin
/// closed, which measures the argument and the empty-input exit and nothing
/// about the rewriting. These carry bytes.
///
/// `--stdin` is the deprecated spelling and prints a two-line notice on stderr
/// before doing exactly what `--annotate-stdin` does, so it is strict: the
/// notice is the difference between the two flags and is otherwise invisible.
///
/// Every id in a payload is [`ROOT`], which every shape contains.
fn name_rev_stdin(out: &mut Vec<Case>) {
    out.push(strict_stdin(
        "name-rev",
        &["name-rev", "--stdin"],
        Shape::Branched,
        b"the root is edfab1b71619a22120a8da1a3d85d68e0200290a and this line ends here\n",
    ));
    // Annotation under a ref restriction: the substituted name is the
    // compound-suffix form from [`name_rev_suffix_algebra`], embedded mid-line.
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin", "--refs=refs/heads/cc-left"],
        Shape::CrissCross,
        b"root edfab1b71619a22120a8da1a3d85d68e0200290a sits four back\n",
    ));
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin", "--name-only", "--refs=refs/tags/*"],
        Shape::AmbiguousRef,
        b"edfab1b71619a22120a8da1a3d85d68e0200290a\n",
    ));
    // An id that is not at the start of the line, twice on one line, with the
    // second occurrence identical to the first — the substitution has to be
    // per-token rather than per-line.
    out.push(Case::with_stdin(
        "name-rev",
        &["name-rev", "--annotate-stdin"],
        Shape::CrissCross,
        b"a edfab1b71619a22120a8da1a3d85d68e0200290a b edfab1b71619a22120a8da1a3d85d68e0200290a c\n",
    ));
}

// ---------------------------------------------------------------------------
// show-branch
// ---------------------------------------------------------------------------

/// The column matrix over histories that contain merges, and the `-` glyph.
///
/// `show-branch` marks each commit `*` in the current branch's column, `+` in
/// every other column that reaches it, and `-` when the commit is a **merge**.
/// The merge glyph is only ever printed for a row that is *shown*, and an
/// ordinary run hides merges: `--sparse` is what stops the traversal from
/// collapsing them away. `diff_family.rs` runs `--sparse --all` on
/// [`Shape::Branched`], which has no merge at all, so the `-` column has never
/// been produced by this corpus.
///
/// Four shapes with four different merge structures, so a port that draws the
/// glyph but places it wrong is caught by which row carries it:
///
/// * [`Shape::Merged`] — one two-parent merge.
/// * [`Shape::Octopus`] — one four-parent merge, plus an unmerged lane.
/// * [`Shape::CrissCross`] — two merges that each contain the other's parent.
/// * [`Shape::CommitGraph`] — a merge inside a longer trunk, with a fork that
///   is not merged.
fn show_branch_merges(out: &mut Vec<Case>) {
    for shape in [Shape::Merged, Shape::Octopus, Shape::CrissCross, Shape::CommitGraph] {
        out.push(Case::new("show-branch", &["show-branch", "--sparse", "--all"], shape));
    }
    each(
        Shape::CrissCross,
        "show-branch",
        &[
            // The five-column header and the body, without `--sparse`, so the
            // pair above is readable as a difference rather than in isolation.
            &["show-branch", "--all"],
            // `--current` adds the checked-out branch as its own column; here it
            // is already named by `--all`, which is the case that separates
            // "adds a column" from "adds a duplicate column".
            &["show-branch", "--current"],
            // `--no-name` keeps the glyph grid and drops the `[name]` field, so
            // it is the narrowest possible test of the grid's width.
            &["show-branch", "--no-name"],
            // `--list` is the header with no grid at all.
            &["show-branch", "--list", "--all"],
            // `--more=<n>` extends the body past the merge base, which is where
            // the `~n` fallback names come from.
            &["show-branch", "--more=3", "cc-left", "cc-right"],
            &["show-branch", "--sparse", "--more=1", "cc-left", "cc-right"],
            // `--topics` drops the first argument's own commits and shows what
            // the others add.
            &["show-branch", "--topics", "cc-left", "cc-right", "cc-a"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "show-branch",
        &[
            &["show-branch", "--topics", "main", "oct-a", "oct-b"],
            &["show-branch", "--more=2", "main", "oct-side"],
        ],
        out,
    );
    // Unrelated roots: no column reaches another, so the grid is diagonal and
    // `--merge-base` has nothing to print.
    each(
        Shape::Unrelated,
        "show-branch",
        &[
            &["show-branch", "--sparse", "--all"],
            &["show-branch", "--topics", "main", "alien"],
        ],
        out,
    );
    out.push(Case::strict(
        "show-branch",
        &["show-branch", "--merge-base", "main", "alien"],
        Shape::Unrelated,
    ));
}

// ---------------------------------------------------------------------------
// merge-base
// ---------------------------------------------------------------------------

/// The reductions, on the shape whose whole point is that they are not the
/// identity.
///
/// `history_query.rs`'s header records the gap this fills verbatim: "**No
/// criss-cross merges**, so `merge-base --all` never prints two ids. […]
/// `--all` […] cannot distinguish 'returns one base' from 'returns all bases'."
/// [`Shape::CrissCross`] exists now. `fixture_gaps.rs` took the first pass over
/// it — two-head `--all`, `--octopus`, `--independent` and `--is-ancestor`;
/// what is left is the arity edges and the mode conflicts, which is what a
/// reduction implemented as a loop gets wrong.
fn merge_base_reductions(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge-base",
        &[
            // Three heads over two bases: the reduction has to survive an odd
            // number of inputs.
            &["merge-base", "--all", "cc-left", "cc-right", "cc-a"],
            &["merge-base", "--octopus", "cc-left", "cc-right", "cc-a", "cc-b"],
            // One argument, in each of the three modes that accept it. The
            // answer is the argument itself, which is the degenerate output an
            // implementation that requires two revisions never produces.
            &["merge-base", "--independent", "cc-left"],
            &["merge-base", "--octopus", "cc-left"],
            &["merge-base", "--all", "cc-left"],
            // A commit against itself: the base is the commit, not its parent.
            &["merge-base", "cc-left", "cc-left"],
            &["merge-base", "--all", "cc-left", "cc-left"],
            // `--fork-point` with no reflog entry to find (exit 1, no output),
            // and with one (the plain base).
            &["merge-base", "--fork-point", "cc-left", "cc-right"],
            &["merge-base", "--fork-point", "main", "cc-left"],
        ],
        out,
    );
    // The mode conflict `history_query.rs` does not cover: it pins
    // `--all --is-ancestor`, and this is the other pair.
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--all", "--independent", "cc-left", "cc-right"],
        Shape::CrissCross,
    ));
    // Generation numbers: the same reductions with a commit-graph file present
    // and one commit deliberately missing from it, so the walk has to mix
    // graph-supplied and computed generations.
    each(
        Shape::CommitGraph,
        "merge-base",
        &[
            &["merge-base", "--all", "main", "cg-loose", "cg-side"],
            &["merge-base", "--independent", "main", "cg-loose", "cg-side"],
            &["merge-base", "--all", "--octopus", "main", "cg-loose", "cg-side"],
        ],
        out,
    );
    // Peeling: the argument is a tag object three deep, and a tag whose target
    // is a blob rather than a commit.
    each(
        Shape::TagChain,
        "merge-base",
        &[
            &["merge-base", "--is-ancestor", "outermost", "main"],
            &["merge-base", "--is-ancestor", "main", "outermost"],
            &["merge-base", "outermost", "main"],
        ],
        out,
    );
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--is-ancestor", "blobtag", "main"],
        Shape::TagChain,
    ));
}

// ---------------------------------------------------------------------------
// the four verbs against each other
// ---------------------------------------------------------------------------

/// Pairs that ask one question through two front ends.
///
/// A stdout difference against stock says the port is wrong somewhere. A
/// difference between two of *these* cases says where: the engine, or the
/// caller. Each pair below is two invocations that must produce the same fact
/// about the same commits, in whatever spelling each verb uses, and each is
/// verified to agree on stock before being written down.
///
/// **The pair that already contradicts itself on the port** is the first one.
/// `describe --contains --match top HEAD~1` and
/// `name-rev --name-only --refs=refs/tags/top HEAD~1` are the same question:
/// name `HEAD~1` using only `refs/tags/top`. Measured on stock 2.55.0 and on
/// git 2.50.1, both answer `tags/top~1`. The port answers `tags/top~1` through
/// `name-rev` and `top~1` through `describe --contains` — so its `name-rev` is
/// right, its `describe` is wrong, and the two disagree with each other about a
/// commit they both reached correctly. On [`Shape::AmbiguousRef`] the wrong
/// spelling is not merely shorter: `top` resolves to `refs/top` by
/// `ref_rev_parse_rules`, so `top~1` does not name that commit and
/// `rev-parse top~1` exits 128.
fn cross_verb(out: &mut Vec<Case>) {
    // ---- pair 1: describe --contains vs name-rev, one restricted ref ----
    out.push(Case::new(
        "name-rev",
        &["name-rev", "--name-only", "--refs=refs/tags/top", "HEAD~1"],
        Shape::AmbiguousRef,
    ));
    out.push(Case::new(
        "name-rev",
        &["name-rev", "--name-only", "--exclude=refs/tags/ambi*", "--refs=refs/tags/*", "HEAD~1"],
        Shape::AmbiguousRef,
    ));

    // ---- pair 2: the two spellings of reduce_heads() ----
    // `merge-base --independent` and `show-branch --independent` run the same
    // reduction over the same five heads and print it in two formats: one id
    // per surviving head, and one header line per surviving head.
    out.push(Case::new(
        "merge-base",
        &["merge-base", "--independent", "cc-left", "cc-right", "cc-a", "cc-b", "main"],
        Shape::CrissCross,
    ));
    out.push(Case::new(
        "show-branch",
        &["show-branch", "--independent", "cc-left", "cc-right", "cc-a", "cc-b", "main"],
        Shape::CrissCross,
    ));

    // ---- pair 3: the octopus base, from both ends ----
    // `merge-base --octopus a b c d` and `show-branch --merge-base a b c d` are
    // the same call into `get_octopus_merge_bases`.
    out.push(Case::new(
        "merge-base",
        &["merge-base", "--octopus", "main", "oct-a", "oct-b", "oct-c"],
        Shape::Octopus,
    ));
    out.push(Case::new(
        "show-branch",
        &["show-branch", "--merge-base", "main", "oct-a", "oct-b", "oct-c"],
        Shape::Octopus,
    ));

    // ---- pair 4: "is X reachable from main", asked three ways ----
    // `merge-base --is-ancestor` answers through the exit code with no output,
    // `name-rev` restricted to `main` answers `undefined`, and `describe --all`
    // answers with a ref of its own. All three are true of `oct-side`, which
    // `main` does not contain, and false of `oct-a`, which it does.
    each(
        Shape::Octopus,
        "merge-base",
        &[
            &["merge-base", "--is-ancestor", "oct-side", "main"],
            &["merge-base", "--is-ancestor", "oct-a", "main"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "name-rev",
        &[
            &["name-rev", "--name-only", "--refs=refs/heads/main", "oct-side", "oct-a"],
        ],
        out,
    );

    // ---- pair 5: an unnameable commit, described and named ----
    // On [`Shape::Unrelated`] the only tag is on the alien root, so `main`'s tip
    // is reachable from no tag. `name-rev` prints `undefined` and exits 0;
    // `describe --contains` **dies**. The two verbs genuinely differ here on
    // stock, and pinning both is what stops a port from making them agree.
    out.push(Case::new(
        "name-rev",
        &["name-rev", "--name-only", "--tags", "HEAD"],
        Shape::Unrelated,
    ));
    out.push(Case::strict(
        "name-rev",
        &["name-rev", "--no-undefined", "--tags", "HEAD"],
        Shape::Unrelated,
    ));

    // ---- pair 6: the criss-cross bases, as ids and as columns ----
    //
    // Two heads is `fixture_gaps.rs`'s pair (`merge-base --all cc-left
    // cc-right` and `show-branch --merge-base cc-left cc-right`, both already
    // in the corpus); three heads is not, and three is where the two
    // reductions can first disagree — `show-branch --merge-base` reduces
    // pairwise while `merge-base --all` does not.
    out.push(Case::new(
        "merge-base",
        &["merge-base", "--all", "cc-left", "cc-right", "cc-b"],
        Shape::CrissCross,
    ));
    out.push(Case::new(
        "show-branch",
        &["show-branch", "--merge-base", "cc-left", "cc-right", "cc-b"],
        Shape::CrissCross,
    ));
}

// ---------------------------------------------------------------------------
// rev-parse, where it enumerates a ref set
// ---------------------------------------------------------------------------

/// `--branches`/`--tags`/`--remotes` with a pattern, and the three printers
/// that render what they select.
///
/// `fixture_gaps.rs` and `fixture_gaps2.rs` call the bare `--branches` and
/// `--tags`; `fixture_gaps3.rs` calls `--symbolic-full-name` and `--abbrev-ref`
/// on *named* refs. The combination — an enumerator feeding a symbolic printer
/// — is what neither reaches, and it is where the port and both gits part
/// company.
///
/// `revision.c:handle_refs` hands each selected ref to the printer by its
/// **short** name, and on [`Shape::AmbiguousRef`] `ambi` and `ambi-ann` are each
/// a branch *and* a tag, so the round trip is ambiguous and stock refuses it:
///
/// ```text
/// $ git rev-parse --symbolic-full-name --branches=ambi'*'
/// error: refname 'ambi' is ambiguous
/// error: refname 'ambi-ann' is ambiguous
/// ```
///
/// with **nothing** on stdout and exit 0. The port prints `refs/heads/ambi` and
/// `refs/heads/ambi-ann`. Reproduced on git 2.50.1 as well, so it is a port
/// defect. `--symbolic` (which does not re-resolve) and `--symbolic-full-name
/// --all` (which is fed full ref names) both agree, and are here as the
/// controls that localise it.
fn rev_parse_ref_sets(out: &mut Vec<Case>) {
    each(
        Shape::AmbiguousRef,
        "rev-parse",
        &[
            // The enumerators alone: ids only, and they agree.
            &["rev-parse", "--branches=ambi*"],
            &["rev-parse", "--tags=ambi*"],
            &["rev-parse", "--branches=rem/*"],
            // The controls.
            &["rev-parse", "--symbolic", "--branches=ambi*"],
            &["rev-parse", "--symbolic-full-name", "--all"],
            // The three that diverge.
            &["rev-parse", "--symbolic-full-name", "--branches=ambi*"],
            &["rev-parse", "--symbolic-full-name", "--tags=ambi*"],
            &["rev-parse", "--abbrev-ref", "--branches=ambi*"],
            // `--abbrev-ref` takes an optional strictness, which decides whether
            // a name that is ambiguous may be shortened at all.
            &["rev-parse", "--abbrev-ref=strict", "refs/heads/ambi"],
            &["rev-parse", "--abbrev-ref=loose", "refs/heads/ambi"],
            &["rev-parse", "--abbrev-ref=strict", "refs/top"],
        ],
        out,
    );
    each(
        Shape::BehindRemote,
        "rev-parse",
        &[
            &["rev-parse", "--remotes=origin/*"],
            &["rev-parse", "--symbolic-full-name", "--remotes=origin/*"],
            &["rev-parse", "--abbrev-ref", "--remotes=origin/*"],
        ],
        out,
    );
    // A pattern that selects nothing: empty output and exit 0, which is a
    // different answer from a pattern that is not a pattern.
    out.push(Case::new("rev-parse", &["rev-parse", "--branches=nosuch*"], Shape::CrissCross));
    out.push(Case::new("rev-parse", &["rev-parse", "--tags=nosuch*"], Shape::CrissCross));
    // `--disambiguate=` over the constructed four-character collision. The
    // three prefixes `fixture_gaps3.rs` asks for are `edfa`, `a366` and
    // `edfab`; this is the fourth answer — a prefix nothing carries.
    out.push(Case::strict(
        "rev-parse",
        &["rev-parse", "--disambiguate=ffff"],
        Shape::PrefixCollision,
    ));
    // …and the root's own prefix, which is a commit on every shape, asked on a
    // shape where it is *not* ambiguous, so the widening is not in play.
    out.push(Case::new("rev-parse", &["rev-parse", "--disambiguate=edfa"], Shape::CrissCross));
    // The root named as a literal id rather than as a rev, so the answer does
    // not depend on any ref: `--symbolic-full-name` has nothing symbolic to
    // print for it and must fall back to the id.
    out.push(Case::new("rev-parse", &["rev-parse", "--symbolic-full-name", ROOT], Shape::CrissCross));
}
