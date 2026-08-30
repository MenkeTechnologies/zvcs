//! What git says about whether a repository is sound, and what it deletes.
//!
//! # How this divides from what already exists
//!
//! Six modules already touch this family, and each owns a different axis. What
//! they own, and what none of them asks:
//!
//! * `corpus/maintenance.rs` — the first sweep of `gc`, `repack`, `prune`,
//!   `prune-packed`, `fsck`, `count-objects`, `commit-graph` and
//!   `multi-pack-index` over the **healthy read shapes** (`Linear`, `Branched`,
//!   `Merged`, `Detached`, `Dirty`, `Conflicted`, `AwkwardPaths`, `Submodule`)
//!   plus every unknown-flag and bad-date refusal. Those repositories have one
//!   loose object store, nothing unreachable, no pack and no derived index, so a
//!   flag that decides *what survives* has nothing to decide.
//! * `corpus/maintenance_repack.rs` — the same verbs over [`Shape::Packed`] and
//!   [`Shape::Damaged`], where each flag leaves a distinguishable mark, plus
//!   `maintenance run --task=`, `pack-refs`, `reflog expire`, and the `gc.*` /
//!   `repack.*` configuration.
//! * `corpus/object_pack.rs` — `cat-file`, `pack-objects`, `index-pack`,
//!   `verify-pack -v`/`show-index`, the `prune-packed`/`count-objects`/
//!   `pack-redundant`/`unpack-file` accounting, and `commit-graph`/
//!   `multi-pack-index` over [`Shape::Packed`].
//! * `corpus/graft_partial.rs` — owns the two deliberately incomplete
//!   repositories, [`Shape::Shallow`] and [`Shape::Promisor`], and asks them
//!   **reading** questions only: the walks, the reachability queries, the naming
//!   verbs, and what must refuse past the graft. It runs no integrity verb and
//!   no lifetime verb at all — its own header says the rule is "the verb has to
//!   be able to tell", and it applied that rule to `log` and `blame`, never to
//!   `fsck` or `gc`.
//! * `corpus/plumbing_objects.rs` — the read-tree/write-tree/hash-object
//!   plumbing under its empty-stdin limitation, with one `verify-pack` and one
//!   `unpack-objects` error path.
//!
//! Two more reach into it sideways and are named because half this module's
//! shapes are theirs. `corpus/shape_reach.rs` and `corpus/fixture_gaps*.rs`
//! sweep a handful of verbs per *shape* rather than per verb — a bare `fsck`,
//! `gc --prune=now`, `prune -n -v`, `count-objects -v` — so several shapes here
//! have one plain invocation of a verb already and nothing that turns a flag
//! on. Every case below was checked against the whole corpus's case-id listing
//! for an exact duplicate before it was kept; `corpus/fixture_gaps2.rs`'s own
//! `log`/`status` cases over the incomplete shapes are the reason it appears in
//! this module's brief and the reason it contributes nothing to divide from.
//!
//! So the hole is not a flag anybody forgot. It is a **pairing**: the shapes
//! that can answer an integrity question were only ever asked reading
//! questions, and the verbs that answer integrity questions were only ever
//! pointed at shapes that have nothing wrong with them and nothing unreachable
//! in them. This module is that pairing, in six groups:
//!
//! 1. `fsck` over the two repositories that are missing objects **lawfully** —
//!    a shallow clone and a partial clone. A repository whose parents stop at a
//!    graft and one whose blobs live on a promisor remote are both *sound*, and
//!    saying so is a different judgement from finding nothing wrong with a
//!    complete store. A port that reports either as broken has failed the
//!    single most consequential question in this territory.
//! 2. `fsck`'s listing flags over the shapes that actually **hold** unreachable
//!    and dangling objects: `Rerere`'s two orphaned conflict trees, `Octopus`'s
//!    two, `Promisor`'s seven, and `Stashed`'s eight — which are unreachable
//!    only once `--no-reflogs` stops the stash log from anchoring them.
//! 3. The verbs that **delete**, over the shapes `maintenance*.rs` never points
//!    them at. [`Shape::NotesReplace`] is the one that matters and the reason
//!    this group exists: see [`lifetime_replace`].
//! 4. `commit-graph` over the one shape that already **has** a commit-graph.
//!    Every existing `commit-graph write` case runs against a repository with
//!    no graph on disk, so the whole of the merge-an-existing-chain path —
//!    which is what `--split`, `--split=replace` and `--split=no-merge` select
//!    between — was reached by no case.
//! 5. `multi-pack-index`'s real verbs over the stores that have packs it can
//!    index, including the two whose packs carry a `.promisor` sidecar.
//! 6. The remaining spellings of `verify-pack`'s one read, and
//!    `verify-tag`/`verify-commit` over a chain of tags rather than a tag.
//!
//! # Determinism
//!
//! Every case here was run twice against stock alone, in a scratch copy of the
//! shape, and compared on stdout, stderr, exit code, the whole object listing,
//! the loose count, the pack directory and the git directory. Three habits keep
//! it that way and are worth stating because breaking any one of them produces
//! a false failure rather than a finding:
//!
//! * **No wall clock.** `--expire=` and `gc.pruneExpire` take an approxidate,
//!   and `2.weeks.ago` is a different instant on every run. Only `now`, `never`
//!   and `all` appear here — the three spellings that name a fixed point
//!   rather than an offset from one. (`maintenance_repack.rs` carries the
//!   `2.weeks.ago` case, where nothing in the fixture is near the boundary.)
//! * **No progress meters.** `--progress` writes to stderr, and every case that
//!   compares stderr passes `--no-progress` or `--quiet` instead.
//! * **`--quiet` on every `gc`.** Without it `gc` prints a repack summary whose
//!   numbers are a function of the pack the implementation chose to write, and
//!   pack bytes are this crate's standing relaxation.
//!
//! What is deliberately *not* here, with the reason:
//!
//! * **`.git/gc.log`.** It is written only when a detached auto-`gc` fails, and
//!   a detached child is exactly what `maintenance_repack.rs` documents as
//!   unreadable mid-rewrite. Measured on stock 2.55.0 over [`Shape::Damaged`]
//!   with `gc.auto=1`, both with and without `gc.autoDetach`: no `gc.log` was
//!   produced either way, so there is nothing a case could pin.
//! * **`fsck --lost-found`'s output directory.** It writes
//!   `.git/lost-found/<type>/<oid>` and no probe in `runner.rs` reads it, so
//!   those cases are pinned on stdout and exit code alone.
//! * **`verify-pack` against a pack inside `.git/objects/pack`.** A pack's file
//!   name embeds its own checksum, so it cannot be spelled as a literal.
//!   [`Shape::Packed`] keeps `packs/sample.pack` in the worktree at a stable
//!   path for exactly this reason, and that is the only pack any case can name.
//! * **The commit-graph's *bytes*.** `probe_storage` enumerates `objects/info`
//!   by name, so a graph that exists is distinguishable from one that does not
//!   and from a split chain under `objects/info/commit-graphs/`, but two graphs
//!   of the same shape holding different data are not. `commit-graph verify` is
//!   the only case here that reads inside one.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    fsck_lawfully_incomplete(out);
    fsck_listings(out);
    lifetime_replace(out);
    lifetime_incomplete(out);
    lifetime_unswept_shapes(out);
    commit_graph_over_a_graph(out);
    midx_over_packs(out);
    verify_reads(out);
}

// ---------------------------------------------------------------------------
// 1. fsck over the two repositories that are incomplete on purpose
// ---------------------------------------------------------------------------

/// `fsck` asked about a shallow clone and a partial clone.
///
/// Both repositories are missing objects, and in both the absence is lawful:
/// [`Shape::Shallow`] grafts its history two commits below the tip and simply
/// does not have the parents, [`Shape::Promisor`] filtered out three blobs that
/// the promisor remote still has. Stock 2.55.0 exits **0** on both under every
/// flag below — `builtin/fsck.c` reads `.git/shallow` into the graft table
/// before it walks, and skips a missing object that a promisor pack accounts
/// for. Nothing in the corpus asked either question: `graft_partial.rs` owns
/// these shapes and runs only reading verbs on them, and every existing `fsck`
/// case runs over a store that is either complete or [`Shape::Damaged`].
///
/// The direction of the risk is what makes the group worth its cost. A port
/// whose `fsck` treats a graft boundary or a promisor absence as damage reports
/// a healthy repository as broken, and there is no louder way for a version
/// control system to be wrong. So the silent cases are `strict`: stock's whole
/// answer is an empty stderr and an exit code, and without comparing stderr a
/// port that prints `error: invalid object <oid>` and still exits 0 matches.
///
/// Three of these do not agree, and each is a different half of the same
/// blindness — measured against stock 2.55.0:
///
/// * `--root` over the shallow clone prints the two grafted commits as roots
///   (`root bd1c76c6…`, `root fc222945…`), because a commit whose parents are
///   absent *is* a root of the walk. The port prints nothing.
/// * `--connectivity-only` over the partial clone prints three `dangling`
///   lines. The port prints none.
/// * `--unreachable` over the partial clone prints seven objects. The port
///   refuses outright — `refusing to guess the output order` on stderr, exit 1
///   — which is a considered refusal rather than a wrong answer, and is pinned
///   here as the difference it is.
fn fsck_lawfully_incomplete(out: &mut Vec<Case>) {
    // The shallow clone. Silence is the whole answer for all but `--root`.
    for args in [
        &["fsck", "--no-progress", "--full", "--strict"][..],
        &["fsck", "--no-progress", "--connectivity-only", "--strict"],
        &["fsck", "--no-progress", "--dangling"],
        &["fsck", "--no-progress", "--cache", "--no-reflogs"],
        &["fsck", "--no-progress", "--tags", "--references"],
    ] {
        out.push(Case::strict("fsck", args, Shape::Shallow));
    }
    // Stdout carries the answer for these two, so they are compared on it.
    out.push(Case::new("fsck", &["fsck", "--no-progress", "--root"], Shape::Shallow));
    out.push(Case::new("fsck", &["fsck", "--no-progress", "--unreachable"], Shape::Shallow));
    out.push(Case::new("fsck", &["fsck", "--no-progress", "--lost-found"], Shape::Shallow));

    // The partial clone. `--strict` matters more here than anywhere: a strict
    // walk opens every object it names, which is the walk most likely to demand
    // a blob the filter left behind.
    for args in [
        &["fsck", "--no-progress", "--full", "--strict"][..],
        &["fsck", "--no-progress", "--cache", "--no-reflogs"],
        &["fsck", "--no-progress", "--tags", "--references"],
    ] {
        out.push(Case::strict("fsck", args, Shape::Promisor));
    }
    for args in [
        &["fsck", "--no-progress", "--connectivity-only"][..],
        &["fsck", "--no-progress", "--connectivity-only", "--dangling"],
        &["fsck", "--no-progress", "--connectivity-only", "--no-dangling"],
        &["fsck", "--no-progress", "--unreachable"],
        &["fsck", "--no-progress", "--dangling"],
        &["fsck", "--no-progress", "--root", "--tags"],
        &["fsck", "--no-progress", "--lost-found"],
    ] {
        out.push(Case::new("fsck", args, Shape::Promisor));
    }

    // `fsck-objects` is the same command under its historical name, and the two
    // must agree about a repository that is incomplete for a reason as much as
    // they agree about a healthy one.
    out.push(Case::strict("fsck-objects", &["fsck-objects", "--strict"], Shape::Shallow));
    out.push(Case::strict("fsck-objects", &["fsck-objects", "--strict"], Shape::Promisor));
}

/// `fsck`'s listing flags over the shapes that hold unreachable and dangling
/// objects.
///
/// `--unreachable`, `--dangling` and `--lost-found` differ from every other
/// `fsck` flag in that their output is a *list of object ids*, and a list is
/// only a measurement if the repository has more than nothing in it. Across the
/// corpus those three flags run almost entirely over stores where the list is
/// empty: `maintenance.rs` asks the healthy read shapes, and
/// `maintenance_repack.rs` asks [`Shape::Packed`] (one unreachable commit) and
/// [`Shape::Damaged`]. Four shapes hold such objects and were never asked —
/// counted on stock 2.55.0:
///
/// | shape       | what is unreachable                                  |
/// |-------------|------------------------------------------------------|
/// | `Rerere`    | two conflict trees and their blobs, five objects      |
/// | `Octopus`   | two trees left by the four-parent merge               |
/// | `Stashed`   | eight objects, but **only** under `--no-reflogs`      |
/// | `Promisor`  | seven, covered in [`fsck_lawfully_incomplete`]        |
///
/// [`Shape::Stashed`] is the one worth spelling out. Its three stash entries are
/// reachable through `refs/stash` *and* through that ref's reflog, so plain
/// `--unreachable` there prints nothing and the case
/// `maintenance_repack.rs` already carries measures an empty list.
/// `--no-reflogs` removes the reflog from the root set and eight objects fall
/// out — which is the difference between a port that consults reflogs for
/// reachability and one that does not, and it is invisible without the flag.
///
/// Ordering is the risk and it is settled rather than assumed: git emits these
/// lines in `obj_hash` slot order, not sorted, so the listing is only a valid
/// comparison if it is stable. Every case here was run five times against stock
/// 2.55.0 in a fresh copy of the shape and produced byte-identical output each
/// time — the slot order is a function of the object set and the traversal, and
/// both are fixed by the fixture.
fn fsck_listings(out: &mut Vec<Case>) {
    for args in [
        &["fsck", "--no-progress", "--unreachable", "--dangling"][..],
        &["fsck", "--no-progress", "--lost-found"],
        &["fsck", "--no-progress", "--cache", "--no-reflogs"],
    ] {
        out.push(Case::new("fsck", args, Shape::Rerere));
    }
    for args in [
        &["fsck", "--no-progress", "--unreachable", "--no-dangling"][..],
        &["fsck", "--no-progress", "--lost-found"],
        &["fsck", "--no-progress", "--unreachable", "--root"],
    ] {
        out.push(Case::new("fsck", args, Shape::Octopus));
    }
    // The reflog is the whole question here: with it, nothing is unreachable;
    // without it, eight objects are.
    out.push(Case::new(
        "fsck",
        &["fsck", "--no-progress", "--no-reflogs", "--unreachable"],
        Shape::Stashed,
    ));
    out.push(Case::new(
        "fsck",
        &["fsck", "--no-progress", "--no-reflogs", "--dangling"],
        Shape::Stashed,
    ));
    // The same pair over a linked worktree, whose second `HEAD` and second
    // reflog are two more root sets a port can forget.
    out.push(Case::new(
        "fsck",
        &["fsck", "--no-progress", "--no-reflogs", "--unreachable"],
        Shape::Worktree,
    ));
}

// ---------------------------------------------------------------------------
// 3. The verbs that delete
// ---------------------------------------------------------------------------

/// `gc` over a repository that has `refs/replace/*` entries.
///
/// **This group found the worst defect in the territory, and the shape it needs
/// is one no maintenance case had ever used.** [`Shape::NotesReplace`] carries
/// two replacements: a commit replaced by another commit with the same tree and
/// a different message, and `README.md`'s blob replaced by a different blob.
/// Every read verb in the corpus already runs over it (`corpus/notes_family.rs`
/// owns that half); no write verb did.
///
/// What `gc --quiet` does there, measured against stock 2.55.0 in a fresh copy
/// of the shape and reproduced on all five `gc` spellings below:
///
/// ```text
/// $ git cat-file --batch-check --batch-all-objects   # after stock's gc
///   0dc1e64f34767c0cd0f35ad39a53bb0ad697ae04 commit 236
///   9741694d75caeb49d3b7c1f59451c0c56bf6216c blob 10
/// $ git cat-file --batch-check --batch-all-objects   # after the port's gc
///   0dc1e64f34767c0cd0f35ad39a53bb0ad697ae04 commit 252
///   9741694d75caeb49d3b7c1f59451c0c56bf6216c blob 18
/// ```
///
/// Same ids, different sizes: the port applied the replacement while *packing*
/// and wrote each replacement's content under the original object's name. The
/// object's name is supposed to be the hash of its content, and after this it
/// is not, so the repository is corrupt from that point onward — reading the
/// original id back with `--no-replace-objects` yields the replacement's bytes
/// (`notes: replacement for commit 1` where the object says `notes: commit 1`).
/// Stock's `fsck --strict` over the finished store says
/// `error: packed 0dc1e64f… is corrupt`, exit 4.
///
/// The two contrasting cases are what make it a diagnosis rather than a report.
/// `--no-replace-objects` and `core.useReplaceRefs=false` both turn the
/// substitution off, and under either one the port's `gc` agrees with stock
/// object for object. So the defect is precisely "the replacement table is
/// consulted on the write path", not "packing is wrong".
///
/// `prune` is included and is *not* affected, which narrows it further: the
/// corruption enters through the repack, not through the reachability walk.
fn lifetime_replace(out: &mut Vec<Case>) {
    for args in [
        &["gc", "--quiet"][..],
        &["gc", "--prune=now", "--quiet"],
        &["gc", "--cruft", "--prune=now", "--quiet"],
        &["gc", "--aggressive", "--prune=now", "--quiet"],
        &["gc", "--keep-largest-pack", "--quiet"],
    ] {
        out.push(Case::new("gc", args, Shape::NotesReplace));
    }
    // The same `gc` with the replacement table switched off, two ways. Both
    // agree with stock, which is what identifies the table as the cause.
    out.push(
        Case::new("gc", &["gc", "--prune=now", "--quiet"], Shape::NotesReplace)
            .with_globals(&[&["--no-replace-objects"]]),
    );
    out.push(
        Case::new("gc", &["gc", "--prune=now", "--quiet"], Shape::NotesReplace)
            .with_config(&[("core.useReplaceRefs", "false")]),
    );
    // The reachability half on its own: `prune` never rewrites a pack, and it
    // agrees. Both spellings, because a dry run that consulted the table would
    // be the same defect with nothing written.
    out.push(Case::new("prune", &["prune", "--expire=now", "-v"], Shape::NotesReplace));
    out.push(Case::new("prune", &["prune", "-n", "--expire=now", "-v"], Shape::NotesReplace));
    out.push(Case::new("count-objects", &["count-objects", "-v"], Shape::NotesReplace));
}

/// `gc`, `prune` and `count-objects` over the shallow clone and the partial
/// clone.
///
/// These are the highest-stakes invocations in the module: a repository that is
/// *supposed* to be missing objects is the one where a collector most easily
/// removes something it should have kept, or drags something in that should
/// have stayed away. Neither shape had ever had a deleting verb pointed at it.
///
/// Measured against stock 2.55.0, and every one of these diverges:
///
/// * **Shallow.** Stock's `gc --quiet` exits 0 in silence and rewrites the
///   store. The port prints `error: invalid object edfab1b7…` on stderr — the
///   commit below the graft, which the port's walk demands and the graft table
///   is supposed to stop it reaching — and still exits 0, so **only** a
///   stderr comparison sees it. `gc --prune=now` and every `prune` refuse
///   outright (exit 1, `prune in a shallow repository is not supported`).
///   `--expire=never` is included because a refusal there is maximally wrong:
///   the invocation deletes nothing by construction, so there is nothing for a
///   missing shallow-file writer to get wrong.
/// * **Promisor.** Stock's `gc` and `prune` — including `prune -n`, a dry run —
///   fetch nothing. The port fetches three filtered-out blobs from the promisor
///   remote and lands three new `pack-<hash>.{pack,idx,promisor}` triples in
///   the object store:
///
///   ```text
///   $ comm -13 <stock's objects> <the port's objects>
///     0880af1bf76c7ecadcd75b4365be837f7ed24b14
///     64055193280dd61767e77ba8edca06d97f71967e
///     7eefafcac1e67b8d4cccd29a48ee216fd80468fa
///   ```
///
///   All three are `hist.txt`'s historical blobs, which is exactly the set
///   `rev-list --missing=print` reports as absent. Nothing is *lost* — the
///   divergence is a lazy fetch that stock does not perform — but a collector
///   that reaches for every blob in history is one that cannot run offline, and
///   it is invisible to stdout, to the exit code and to reachability.
///
/// `--exclude-promisor-objects` is the flag whose entire purpose is this
/// repository; `maintenance_repack.rs` measures it over [`Shape::Packed`],
/// which has no promisor pack for it to exclude.
fn lifetime_incomplete(out: &mut Vec<Case>) {
    // Shallow: stderr is the whole difference on the two that succeed, so all
    // of these compare it.
    for args in [
        &["gc", "--quiet"][..],
        &["gc", "--no-prune", "--quiet"],
        &["gc", "--prune=now", "--quiet"],
        &["prune", "--expire=now", "-v"],
        &["prune", "--expire=never", "-v"],
        &["prune", "-n", "--expire=now", "-v"],
    ] {
        out.push(Case::strict(args[0], args, Shape::Shallow));
    }
    out.push(Case::new("prune-packed", &["prune-packed", "-n", "-q"], Shape::Shallow));
    out.push(
        Case::new("gc", &["gc", "--auto", "--quiet"], Shape::Shallow)
            .with_config(&[("gc.auto", "1"), ("gc.autoDetach", "false")]),
    );

    // Promisor: the object listing is the whole difference, so these are
    // compared on state rather than on stderr.
    for args in [
        &["gc", "--quiet"][..],
        &["gc", "--prune=now", "--quiet"],
        &["gc", "--no-prune", "--quiet"],
        &["gc", "--keep-largest-pack", "--quiet"],
    ] {
        out.push(Case::new("gc", args, Shape::Promisor));
    }
    for args in [
        &["prune", "--expire=now", "-v"][..],
        &["prune", "--expire=never", "-v"],
        &["prune", "-n", "--expire=now", "-v"],
        &["prune", "--exclude-promisor-objects", "--expire=now", "-v"],
        &["prune", "--exclude-promisor-objects", "-n", "-v"],
    ] {
        out.push(Case::new("prune", args, Shape::Promisor));
    }
    out.push(Case::new("prune-packed", &["prune-packed", "-n", "-q"], Shape::Promisor));
    out.push(
        Case::new("gc", &["gc", "--auto", "--quiet"], Shape::Promisor)
            .with_config(&[("gc.auto", "1"), ("gc.autoDetach", "false")]),
    );
}

/// The deleting verbs over the remaining shapes no maintenance case sweeps.
///
/// `maintenance.rs` and `maintenance_repack.rs` between them run `gc` over
/// eleven shapes; these six are not among them, and each holds a kind of object
/// or a kind of root that the eleven do not:
///
/// * [`Shape::TagChain`] — a tag pointing at a tag pointing at a tag, plus tags
///   on a blob and on a tree. A collector that peels one step and stops loses
///   `inner`'s target; one that assumes a tag's target is a commit loses the
///   blob and the tree.
/// * [`Shape::SplitIndex`] — the root set includes a second index file
///   (`.git/sharedindex.<hash>`), which is a file no other shape has.
/// * [`Shape::Symlinks`] — mode `120000` entries and the empty blob, which is a
///   zero-length object no packing path had been asked to carry.
/// * [`Shape::Rerere`] — `.git/rr-cache` holds preimages that are *not* roots,
///   so the two conflict trees must stay unreachable and must survive
///   `--no-prune` and not survive `--prune=now`.
/// * [`Shape::Octopus`] — a four-parent merge, where a walk that follows only
///   the first two parents drops a whole lane.
/// * [`Shape::CommitGraph`] — `prune` beside a commit-graph that is stale by
///   construction, which is the read path most likely to answer reachability
///   from the graph rather than from the objects.
///
/// All of these agree today. They are pinned as a floor: the object listing is
/// what carries the assertion, so a future change that starts collecting a tag
/// chain's middle link or a shared index's blobs is caught at the shape that
/// has one rather than at the next repository that happens to.
fn lifetime_unswept_shapes(out: &mut Vec<Case>) {
    for &shape in &[
        Shape::TagChain,
        Shape::SplitIndex,
        Shape::Symlinks,
        Shape::Rerere,
        Shape::Octopus,
        Shape::Sparse,
    ] {
        out.push(Case::new("gc", &["gc", "--prune=now", "--quiet"], shape));
    }
    // `--no-prune` is the other half of the same question on the one shape
    // whose unreachable objects are the point.
    out.push(Case::new("gc", &["gc", "--no-prune", "--quiet"], Shape::Rerere));
    for &shape in &[Shape::TagChain, Shape::Rerere, Shape::CommitGraph, Shape::Symlinks] {
        out.push(Case::new("prune", &["prune", "--expire=now", "-v"], shape));
    }
    // `--expire=never` must remove nothing at all, on a shape where something
    // is removable — the pair separates "prunes correctly" from "prunes".
    out.push(Case::new("prune", &["prune", "--expire=never", "-v"], Shape::Rerere));
    for &shape in &[Shape::TagChain, Shape::SplitIndex, Shape::Rerere, Shape::CommitGraph] {
        out.push(Case::new("count-objects", &["count-objects", "-v"], shape));
    }
    // The exact field set, in the two renderings, over the store where the
    // numbers are not all zero: `count-objects -v` prints eight fields and `-H`
    // rewrites the two byte counts among them.
    out.push(Case::new("count-objects", &["count-objects", "-v", "-H"], Shape::Damaged));
}

// ---------------------------------------------------------------------------
// 4. commit-graph over a repository that already has one
// ---------------------------------------------------------------------------

/// `commit-graph` asked of [`Shape::CommitGraph`], the one shape carrying a
/// written graph.
///
/// Every `commit-graph write` case in the corpus runs against a repository with
/// **no** graph on disk: `maintenance.rs` sweeps the healthy read shapes and
/// `object_pack.rs` uses [`Shape::Packed`], and neither has an
/// `objects/info/commit-graph`. So `write` was only ever measured on its
/// create-from-nothing path, and the whole of the merge-an-existing-chain path
/// — which is what `--split`, `--split=replace` and `--split=no-merge` select
/// between — was reached by no case at all. `verify` was in the same position:
/// it ran only where there was nothing to verify, so exit 0 meant "no file"
/// rather than "the file is good".
///
/// The shape is built for this: `cg-late` is committed *after* the graph is
/// written, so the graph on disk is valid and incomplete, and `write` has to
/// decide what to do with a commit the existing file does not cover.
///
/// Two of these diverge, in the place `corpus/fixture_gaps.rs`'s bare `--split`
/// case on this shape already lands. Over an existing graph stock converts
/// `objects/info/commit-graph` into a chain —
/// `objects/info/commit-graphs/commit-graph-chain` plus two `graph-<hash>.graph`
/// files, which `probe_storage` enumerates by their elided names — and exits 0.
/// The port refuses with `unsupported flag "--split" over an existing
/// commit-graph`, exit 1, file unchanged, and `--split=no-merge` and
/// `--split=replace` refuse the same way. The two named strategies are kept
/// separately from the bare flag because each selects a different merge
/// decision and a port may implement them one at a time.
///
/// [`Shape::Shallow`] is the other half. Stock writes **no graph at all** in a
/// shallow repository, silently, exit 0 — `commit-graph` declines rather than
/// recording generation numbers it cannot compute past the graft. The port
/// prints `error: invalid object edfab1b7…` and exits 1, which is the same
/// missing graft handling that shows up in its `gc`. Both are `strict`: with an
/// empty stdout and no file written on either side, stderr is the only place
/// the difference appears.
fn commit_graph_over_a_graph(out: &mut Vec<Case>) {
    for args in [
        &["commit-graph", "verify", "--object-dir=.git/objects"][..],
        &["commit-graph", "write", "--reachable", "--append"],
        &["commit-graph", "write", "--reachable", "--split=no-merge"],
        &["commit-graph", "write", "--reachable", "--split=replace"],
        &["commit-graph", "write", "--reachable", "--no-changed-paths"],
        &["commit-graph", "write", "--reachable", "--changed-paths", "--max-new-filters=1"],
        &["commit-graph", "write", "--reachable", "--append", "--changed-paths"],
    ] {
        out.push(Case::new("commit-graph", args, Shape::CommitGraph));
    }
    // `core.commitGraph=false` must suppress the *read* as well as the write:
    // `verify` over a graph it is told to ignore is a different answer from
    // `verify` over a graph that is not there.
    out.push(
        Case::new("commit-graph", &["commit-graph", "verify"], Shape::CommitGraph)
            .with_config(&[("core.commitGraph", "false")]),
    );

    // A shallow repository: stock writes nothing and says nothing.
    for args in [
        &["commit-graph", "write", "--reachable"][..],
        &["commit-graph", "write", "--reachable", "--changed-paths"],
        &["commit-graph", "verify"],
    ] {
        out.push(Case::strict("commit-graph", args, Shape::Shallow));
    }
    // A partial clone: the commits and trees survived the filter, so the graph
    // is writable and the missing blobs must not be reached for.
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable"],
        Shape::Promisor,
    ));
    out.push(Case::new(
        "commit-graph",
        &["commit-graph", "write", "--reachable", "--changed-paths"],
        Shape::Promisor,
    ));
    out.push(Case::strict("commit-graph", &["commit-graph", "verify"], Shape::Promisor));
}

// ---------------------------------------------------------------------------
// 5. multi-pack-index over stores that have packs
// ---------------------------------------------------------------------------

/// `multi-pack-index`'s real verbs, over the three shapes that have packs.
///
/// `maintenance.rs` says it plainly in its own header: no shape it used had a
/// pack, so its `multi-pack-index` cases are "almost entirely error paths".
/// `object_pack.rs` fixed half of that over [`Shape::Packed`] — `write
/// --bitmap`, `repack`, `--preferred-pack=`, `--stdin-packs` — and left the
/// other half: `verify`, `expire`, a non-zero `--batch-size`, `--no-bitmap`,
/// and `--object-dir` pointed at the store that is actually there rather than
/// at one that is not.
///
/// The two incomplete shapes are the new ground. [`Shape::Promisor`]'s packs
/// each carry a `.promisor` sidecar, and `expire` deciding whether a promisor
/// pack may be dropped is a decision no other fixture can pose;
/// [`Shape::Shallow`] has exactly one pack, which is the degenerate input where
/// `expire` and `repack` must both do nothing.
///
/// What carries the assertion is not the midx file — `probe_pack_headers` reads
/// its header (version, pack count, chunk ids, object count) and nothing
/// deeper — but the object listing around the operation. An `expire` or a
/// `repack` that dropped an object is caught there even though the index itself
/// is compared only by shape.
fn midx_over_packs(out: &mut Vec<Case>) {
    for args in [
        &["multi-pack-index", "expire"][..],
        &["multi-pack-index", "repack", "--batch-size=1"],
        &["multi-pack-index", "write", "--no-bitmap"],
        &["multi-pack-index", "write", "--object-dir=.git/objects"],
        &["multi-pack-index", "verify", "--object-dir=.git/objects"],
        &["multi-pack-index", "expire", "--object-dir=.git/objects"],
    ] {
        out.push(Case::new("multi-pack-index", args, Shape::Packed));
    }
    for &shape in &[Shape::Shallow, Shape::Promisor] {
        for args in [
            &["multi-pack-index", "write"][..],
            &["multi-pack-index", "verify"],
            &["multi-pack-index", "expire"],
            &["multi-pack-index", "repack", "--batch-size=0"],
        ] {
            out.push(Case::new("multi-pack-index", args, shape));
        }
    }
}

// ---------------------------------------------------------------------------
// 6. verify-pack, verify-tag, verify-commit
// ---------------------------------------------------------------------------

/// The reads that only assert.
///
/// `verify-pack` has one job and several spellings of it. `object_pack.rs`
/// pins `-v <pack>` and `--stat-only <idx>` and `corpus/shape_reach.rs` sweeps
/// the `.idx` operand across `-v`/`--verbose`/`-s`/`--stat-only`; what neither
/// reaches is the *`.pack`* operand under the printing flags, and an
/// `--object-format` that **matches** the repository. The existing
/// `--object-format` cases are both mismatches (`sha256` over a `sha1` index,
/// and a bogus name), so a port that rejects every `--object-format` scored
/// exactly the same as one that checks it against the store.
///
/// `verify-tag` and `verify-commit` are owned by `corpus/tag_describe.rs`, over
/// [`Shape::Branched`], where every tag points straight at a commit. The whole
/// answer is on stderr (`error: no signature found`, exit 1) so these are
/// `strict`, and the shape is the one thing they add: [`Shape::TagChain`] has a
/// tag pointing at a tag pointing at a tag, a tag on a blob and a tag on a
/// tree. An implementation that peels to the end before looking for a signature
/// and one that peels once answer identically on `Branched` and differently
/// here, and a tag whose target is not a commit is the input that separates
/// "no signature" from "not a commit".
fn verify_reads(out: &mut Vec<Case>) {
    for args in [
        &["verify-pack", "-s", "packs/sample.pack"][..],
        &["verify-pack", "--verbose", "packs/sample.pack"],
        &["verify-pack", "--object-format=sha1", "packs/sample.pack"],
    ] {
        out.push(Case::new("verify-pack", args, Shape::Packed));
    }

    for args in [
        &["verify-tag", "outermost"][..],
        &["verify-tag", "--raw", "outer", "inner"],
        &["verify-tag", "blobtag"],
        &["verify-tag", "treetag"],
        &["verify-tag", "--format=%(objectname) %(objecttype)", "outermost"],
        &["verify-commit", "outermost"],
    ] {
        out.push(Case::strict(args[0], args, Shape::TagChain));
    }
}
