//! Differential corpus cases for the merge **engine** as `git merge` drives it:
//! the strategy the default backend is reached by (`-s ort`), the `-X` grammar,
//! the message machinery that turns a finished merge into a commit or a
//! `MERGE_MSG`, and the option table `builtin/merge.c` parses before any of that
//! runs.
//!
//! Every count and every behavioural claim below was measured on this machine
//! and is reproducible by the recipe given beside it. Where a claim the first
//! draft of this module made turned out to be wrong, the corrected fact is here
//! and the case that depended on the wrong one was deleted — see
//! [`merge_config_values`] and [`message_machinery`].
//!
//! # How this divides the merge surface with the modules already here
//!
//! * [`super::merge_strategies`] — the nearest neighbour. It owns *which
//!   program the trees are handed to*: `-s recursive`, `-s resolve`,
//!   `-s subtree`, `-s ours`, `-s octopus`, the `-s a -s b` retry loop, the
//!   backend binaries invoked directly, and the `-X` set as it stands on
//!   [`Shape::CrissCross`]. What it does **not** contain is `-s ort`.
//! * [`super::merge_family`] owns the *bytes* a three-way text merge produces —
//!   `merge-file`, `merge-index`, `merge-one-file`, `mergetool`, and the ll-merge
//!   driver — plus `merge --abort`/`--continue` on [`Shape::Conflicted`].
//! * [`super::merge_dirty`] owns the dirty-worktree gates on
//!   [`Shape::MergeableDirty`]/[`Shape::MergeableStaged`].
//! * [`super::patch_equivalence`] owns `merge-tree` in both its modes. Nothing
//!   here runs `merge-tree`: the split is *engine driven from a worktree* here,
//!   *engine driven from bare trees* there.
//! * [`super::rebase_engine`] owns the same engine reached through
//!   `rebase`/`cherry-pick`/`revert`; [`super::sequences`] owns everything
//!   needing a second invocation; [`super::rerere_engine`] owns `rerere.*` over
//!   a merge; [`super::attributes_filters`] owns `merge.<driver>.driver`.
//!
//! # What is here that is in none of them
//!
//! Measured by listing every case id the corpus generates with and without this
//! module (`--list-cases` against a copy of the crate with the
//! `merge_ort::cases` call commented out), reducing each id to the *invocation*
//! it names, and counting. Without this module the corpus holds **395 `merge`
//! case ids**, which reduce to **244 distinct `merge` invocations** (the rest
//! are steps of other commands inside sequences whose entry point is `merge`),
//! and **26 `merge-recursive` ids / 25 invocations**. Across those 244
//! invocations:
//!
//! 1. **`-s ort`** appears exactly **once**, and `--strategy=` /
//!    `--strategy-option` **zero** times.
//! 2. **The merge-message machinery** — `--log`, `--cleanup=`, `--signoff`,
//!    `-F <path>`, `--into-name`, and `-m` given twice: **zero** each.
//! 3. **The report knobs** — `-n`, `--stat`, `--no-stat`, `--summary`,
//!    `--compact-summary`, `-e`, `--verbose`, `--progress`, `--autostash`,
//!    `--overwrite-ignore`, `--verify-signatures`: **zero** each. (Grepping the
//!    raw id listing instead reports 12 for `--stat`; every one of those is a
//!    `diff --cached --stat` or `show --stat` *step* inside a sequence filed
//!    under `merge`, not a `merge --stat`. The invocation-level count is the
//!    one this list is about.)
//! 4. **The `--no-` half of the option table.** `parse_options` generates a
//!    negation for all but a handful of `merge`'s options; the ones a
//!    hand-written parser forgets fall through to `cmd_merge`'s "then it must be
//!    a rev" branch and the merge silently does not happen. **Five of them do
//!    exactly that in the port under test** — `--no-abort`, `--no-quit`,
//!    `--no-continue`, `--no-strategy-option`, `--no-overwrite-ignore` — each
//!    answering `merge: <opt> - not something we can merge` at exit 1 where
//!    stock fast-forwards at exit 0.
//!
//! Two further dimensions are **thin** rather than absent, which is what the
//! first draft got wrong:
//!
//! * A working directory below the root is *not* unused by `merge`: eight
//!   pre-existing `merge` ids and one `merge-recursive` id carry a `cwd`. They
//!   are one bare-repository `cwd[.remote.git]` on [`Shape::BehindRemote`] and
//!   one seven-step [`Shape::Conflicted`] sequence run from `src`. The four
//!   cases in [`from_a_subdirectory`] are on four other shapes.
//! * `merge-recursive` is *not* run only on [`Shape::CrissCross`]: its 25
//!   pre-existing invocations span twelve shapes. What none of them carries is
//!   the option set [`recursive_options_over_one_base`] names — those spellings
//!   appear on **no** `merge-recursive` invocation in the corpus, though most
//!   of them do appear elsewhere (under `merge -X`, `diff`, `merge-file`).
//!
//! # Which conflict types are reachable at all, and which are not
//!
//! The brief for this module named thirteen conflict types. Most cannot be
//! produced by any invocation against any fixture, and saying which is more
//! useful than writing cases that measure something easier. Census run over
//! **every commit reachable from every ref of all 43 shapes**, built by the
//! crate's own [`crate::fixture::build`]: `ls-tree -r` and `ls-tree -r -t` per
//! commit for modes and blob-versus-tree, `log --all` with
//! `--diff-filter=D`/`T`/`R`, and a `merge-base --is-ancestor` sweep over each
//! shape's branch tips to find which shapes can be merged at all.
//!
//! * **Reachable.** *Content* conflict — [`Shape::CrissCross`]'s `clash.txt`.
//!   *Add/add* in both its forms — with a base on [`Shape::Conflicted`]
//!   (`conflict.txt`, added independently on `ours` and `theirs`) and with **no**
//!   base on [`Shape::Unrelated`] (`README.md`, across two orphan roots). A
//!   *type change applied cleanly* — [`Shape::Symlinks`]' `dir/target.txt`.
//! * **Unreachable, and why.**
//!   * *modify/delete*, *delete/modify*, *rename/delete*, *rename/add*,
//!     *rename/rename*, *directory rename*: the census finds **two deletions and
//!     two renames in the entire corpus**, all four on [`Shape::Renamed`]
//!     (`orig/alpha.txt` `R100`, `orig/beta.txt` `R072`), and `renamed` has **no
//!     two divergent branch tips at all** — it is strictly linear, so no merge
//!     can be run on it. `orig/` also keeps two of its four files, so a
//!     directory rename would not be detected even if there were a second line
//!     of development.
//!   * *mode-only*: **no tree in any commit of any shape contains a `100755`
//!     blob.** Every `0o755` in `fixture.rs` is a hook script under `.git`.
//!   * *symlink/file*: `dir/target.txt` on [`Shape::Symlinks`] is the corpus's
//!     **only** typechange, and only `sym-pending` changes it — `main` leaves it
//!     alone, so the merge applies the typechange and there is nothing to
//!     disagree with.
//!   * *file becomes directory*: **no path is a blob in one commit and a tree in
//!     another** anywhere in the corpus.
//!   * *gitlink*: the two `160000` entries ([`Shape::Submodule`]'s `sub`,
//!     [`Shape::NestedSubmodule`]'s `mid`) are on shapes with no divergent
//!     branch tips, so no merge can be run there either.
//!   * *binary*: `app/data.bin` on [`Shape::Patches`] has two revisions, one on
//!     `main` and one on `pending`, so a merge of the two is a fast-forward of
//!     that path rather than a conflict.
//!
//! Every one of those needs a fixture shape, and a corpus module cannot add one.
//! So the conflict *types* below are the reachable ones, and the value is in
//! what is asked **about** them: what the engine leaves behind (`MERGE_MSG` under
//! each `--cleanup` mode, `SQUASH_MSG` when the squashed range contains a merge
//! commit, `AUTO_MERGE`, the stages, the HEAD reflog) rather than which category
//! the conflict falls in.
//!
//! **Defects this corpus therefore cannot see, recorded here rather than papered
//! over.** Outside the fixtures, in a scratch repository where one side
//! renames a directory and the other adds a file into the old one, the port
//! diverges three ways from stock 2.55.0 and git 2.50.1: at the default
//! `merge.directoryRenames=conflict` it exits 0 and **commits** where both gits
//! raise `CONFLICT (file location)` and exit 1; at `=true` it omits the
//! `Path updated: …` line; and at `=false` it moves the file anyway, committing
//! a tree neither git produces. Two further divergences show up on a
//! nine-conflict scratch repository: the port omits stock's
//! `CONFLICT (file/directory): directory in the way of …` line, and for a
//! symlink-versus-file conflict it records stages 1 and 3 at the original path
//! instead of at the `~<branch>` path stock renames the file side to. None of
//! the five is expressible against any shape in this corpus.
//!
//! # What the module currently finds
//!
//! 184 cases, **157 matching (85.3%)**, measured with
//! `--only merge,merge-recursive --verbose` against
//! `target/debug/git` with `/usr/bin/git` (2.50.1) as the second oracle. Every
//! one of the 27 failures was reproduced by hand in a scratch copy of its shape
//! and none is a version difference — the second oracle corroborates all of
//! them. They are eight distinct defects, not 27:
//!
//! | defect | cases |
//! |---|---|
//! | negations fall through to the "must be a rev" branch | 6 |
//! | `SQUASH_MSG` drops merge commits from the `HEAD..MERGE_HEAD` walk | 5 |
//! | `--cleanup=scissors` writes no scissors block into `MERGE_MSG` | 3 |
//! | (`--cleanup=scissors --squash cc-right` is in both rows above, which is why the column sums to 28 over 27 cases) | |
//! | `merge.renameLimit=<non-numeric>` not validated, so the merge runs | 2 |
//! | the state verbs' argument check and its usage block | 4 |
//! | `-s ort` over >2 heads writes no `HEAD` reflog entry | 2 |
//! | `merge.autoStash` hint line omitted from the failure path | 2 |
//! | one each: `--cleanup=verbatim` trailing newline, `-F` missing file exit code, `-s resolve` leaves `REUC` where stock leaves `TREE`, `merge-recursive --subtree=` with two bases | 4 |
//!
//! Only two of the eight can lose data or produce a wrong result rather than a
//! wrong message: `merge.renameLimit` (the port commits where stock refuses)
//! and `-s resolve` on [`Shape::Cherry`] (the port hands stock an index it has
//! to repair). Both are called out where their cases are defined.
//!
//! # Determinism
//!
//! Many of these cases end in a commit, so their object ids are part of what is
//! compared. Two separate pieces of evidence, and they are different strengths:
//!
//! * **The seven cases this pass added, and the one it moved,** were each run
//!   **twice against stock 2.55.0** by hand, in two `cp -Rp` copies of their
//!   shape under [`crate::env::harden`], and the two runs compared on exit code,
//!   stdout,
//!   stderr, `for-each-ref`, `ls-files --stage`, `cat-file --batch-check
//!   --batch-all-objects`, the operation-state files and
//!   `log -1 --format=%B%n%T%n%P`. All eight agreed.
//! * **The rest** rest on the harness's own stock repeat rather than on a hand
//!   run: a `--only merge,merge-recursive` pass reports `NONDETERMINISTIC` = 0
//!   and `zvcs-flaky` = 0 across every id it ran, which is what those verdicts
//!   are for.
//!
//! `cp -Rp` is deliberate —
//! [`crate::fixture::copy_tree`] carries mtimes across and the shapes set
//! `core.checkStat=minimal`, and a copy that drops the timestamps produces a
//! stat-dirty index that makes `builtin/merge.c`'s trivial in-index path fail.
//!
//! `GIT_EDITOR` is pinned to `true` by [`crate::env::harden`], so `-e` commits
//! git's own default message unedited, and `--cleanup=verbatim` is what makes
//! the exact bytes of that message observable — see [`message_machinery`].


use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ort_the_default_strategy(out);
    strategies_over_one_base(out);
    strategy_option_grammar(out);
    message_machinery(out);
    report_knobs(out);
    option_table_negations(out);
    state_verbs_reject_arguments(out);
    merge_config_values(out);
    squash_over_a_merge(out);
    from_a_subdirectory(out);
    recursive_options_over_one_base(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// Push one strict case per argv: the refusal or the diagnostic *is* the
/// contract, so stderr is compared byte for byte too.
fn each_strict(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::strict(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// `-s ort`: the strategy every merge runs under and almost nothing names
// ---------------------------------------------------------------------------

/// The default backend, spelled, on the shapes the corpus can merge.
///
/// The point is not that `-s ort` works — it is the default, so every unadorned
/// `merge` in the corpus already exercises the backend. The point is the
/// **name**: `cmd_merge` looks a strategy up in `builtin/merge.c`'s table, and a
/// port that resolves `ort` to a different entry than the empty default, or that
/// aliases `ort` and `recursive` to two different implementations, is invisible
/// until the name is written down. Where [`super::merge_strategies`] already has
/// `-s recursive` on the same shape and rev — [`Shape::Branched`]'s `feature`
/// and [`Shape::CrissCross`]'s `cc-right` — the pair turns "are these the same
/// backend" into a comparison rather than a claim.
///
/// Measured on stock 2.55.0 by running `merge -s <name> cc-right` on
/// [`Shape::CrissCross`] six times and reading `ls-files --stage` after each.
/// The six names are **four** distinct index outcomes:
///
/// * `ort`, `recursive`, `subtree` — `clash.txt` at stages **1/2/3**, `cc.txt`
///   merged to `b6f80f6368e7`, exit 1.
/// * `resolve` — `clash.txt` at stages **2 and 3 only**, no stage 1, because
///   `read-tree -m` resolved against one base rather than a virtual one; same
///   merged `cc.txt`, exit 1.
/// * `ours` — index untouched (`cc.txt` still `530844c6079c`, `clash.txt` at
///   stage 0), exit 0, commits.
/// * `octopus` — index untouched, exit 2.
///
/// A port that collapsed any two of the four would be caught. **The port under
/// test does not collapse any of them**: run side by side on this shape it
/// reproduces all four outcomes, and on [`Shape::Cherry`] (see
/// [`strategies_over_one_base`]) it reproduces the three that shape separates.
/// `ort` and `recursive` are *not* two engines on stock either — they produce
/// byte-identical stdout, index and tree on every input tried, including a
/// scratch repository carrying nine simultaneous conflicts — so a port that
/// answers identically to both is matching stock, not aliasing past a test.
///
/// `-s ort` over more than two heads is here strict: `merge-ort` is a two-head
/// engine and refuses with `error: Not handling anything other than two heads
/// merge.` followed by `Merge with strategy ort failed.` — a different refusal
/// from `git-merge-octopus`'s, from a different program, and the corpus had
/// neither for `ort`. Both those cases fail against the port, and **not on the
/// message**: the two sides agree on stdout, stderr and exit code, and diverge
/// on the `HEAD` reflog. Stock writes a no-op entry
/// `<oid> <oid> ... merge div-cold div-other: updating HEAD` even though the
/// head-count check refused before any tree was touched; the port writes none.
fn ort_the_default_strategy(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "merge",
        &[&["merge", "-s", "ort", "feature"], &["merge", "--no-ff", "-s", "ort", "feature"]],
        out,
    );
    each(Shape::Symlinks, "merge", &[&["merge", "-s", "ort", "-m", "sym", "sym-pending"]], out);
    each(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "-s", "ort", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-s", "ort", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            // `-s ort` reaching the squash path over a baseless add/add.
            &["merge", "-s", "ort", "--allow-unrelated-histories", "--squash", "alien-clash"],
        ],
        out,
    );
    each(Shape::MergeableDirty, "merge", &[&["merge", "-s", "ort", "div-cold"]], out);
    each(Shape::Octopus, "merge", &[&["merge", "-s", "ort", "--no-ff", "-m", "x", "oct-side"]], out);
    each(Shape::Cherry, "merge", &[&["merge", "-s", "ort", "--no-ff", "-m", "x", "main"]], out);
    each(
        Shape::CrissCross,
        "merge",
        &[
            // The long spelling of the same option, which nothing in the corpus
            // used for any strategy.
            &["merge", "--strategy=ort", "cc-right"],
            // The retry loop with `ort` on each side of it, and the two orders
            // print different things. `-s resolve -s ort` runs `resolve`
            // (which dies inside `git-merge-one-file`), rewinds, runs `ort`
            // (which conflicts), rewinds again, and then runs `resolve` a
            // *third* time under `Using the resolve strategy to prepare
            // resolving by hand.` — the best-of-the-failures replay.
            // `-s ort -s resolve` runs `ort`, rewinds, runs `resolve`, and
            // stops: `resolve` was last, so it is already the prepared tree.
            // Both exit 1.
            &["merge", "-s", "resolve", "-s", "ort", "cc-right"],
            &["merge", "-s", "ort", "-s", "resolve", "cc-right"],
            // A strategy option delivered to the named strategy rather than to
            // the default one.
            &["merge", "-s", "ort", "-X", "ours", "cc-right"],
            &["merge", "-s", "ort", "cc-right", "cc-a"],
        ],
        out,
    );
    each_strict(
        Shape::MergeableStaged,
        "merge",
        &[
            // Which gate fires first when the strategy is named: `ort`'s own
            // index-vs-HEAD check, worded differently from `git-merge-resolve`'s
            // and from `git-merge-octopus`'s.
            &["merge", "-s", "ort", "div-cold"],
            // Two extra heads plus a staged change: the head count is checked
            // first, so the staged file is never mentioned.
            &["merge", "-s", "ort", "div-cold", "div-other"],
            &["merge", "-s", "ort", "div-squat", "ff-squat"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge",
        &[
            // An option the strategy does not know, rejected by the *named*
            // strategy rather than by the default one.
            &["merge", "-s", "ort", "-X", "no-such-option", "cc-right"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// The six strategy names over one merge base, on the shape that separates them
// ---------------------------------------------------------------------------

/// The whole strategy table run against **one** merge and compared on the
/// resulting tree — the grid that answers "does this port implement these
/// distinctly, or alias them".
///
/// [`super::merge_strategies`] already has the grid on [`Shape::Branched`] (a
/// fast-forwardable merge) and on [`Shape::CrissCross`] (two merge bases). What
/// neither of those is, is an ordinary three-way *text* merge over a single
/// base — and that is the case where `resolve` and `ort` are most likely to
/// agree and a port could get away with running one for the other.
/// [`Shape::Cherry`] is exactly that shape: `topic` (checked out) and `main`
/// have one merge base, `cherry: seed`, and `app.txt` is edited on both sides —
/// the same hunk on one line, different hunks on two others.
///
/// Measured on stock 2.55.0 and corroborated on git 2.50.1, `merge -s <name>
/// --no-ff -m x main` here is **three** distinct outcomes, not six:
///
/// * `ort`, `recursive`, `resolve`, `subtree` — exit 0, all four committing the
///   *same* tree `4c77efd4c6f6`.
/// * `ours` — exit 0, committing `HEAD`'s tree `69389d709760`.
/// * `octopus` — exit 2, `error: Merge requires file-level merging` /
///   `Merge with strategy octopus failed.`, nothing committed.
///
/// So this grid does not separate `ort` from `resolve` on the *tree*;
/// [`Shape::CrissCross`] does that. What it separates is what they leave in the
/// **index**, and that is where it found something no existing case could see.
///
/// **`merge -s resolve --no-ff -m x main` is a real divergence.** Refs, `HEAD`,
/// the commit, the committed tree, stdout, stderr and exit code all agree
/// between the port and both gits; the index extensions do not. Stock writes a
/// `TREE` cache-tree extension (`<root>=4/1:4c77efd4c6f6…`, 397 bytes); the
/// port writes a `REUC` resolve-undo record for `app.txt` instead and no `TREE`
/// at all (433 bytes), so stock has to rebuild the cache tree on the next
/// command that needs one — the harness's `probe_interop` reports
/// `index-repaired: yes` and the index growing to 494 bytes on the port's side
/// and `no` on stock's. This is the [`crate::runner::Verdict::InteropDiff`]
/// class of defect reached through a strategy name, and it is why this grid
/// compares the index and not only the tree. The other five strategies match
/// stock exactly, index extensions included.
///
/// [`Shape::Unrelated`] deliberately gets nothing here: its grid over a
/// **baseless** add/add is already complete — `octopus`, `ours`, `recursive`,
/// `resolve` and `subtree` on `alien` from [`super::merge_strategies`], plus
/// `-s ort` from [`ort_the_default_strategy`] above. Adding to it produced two
/// duplicate ids, which `no_case_id_appears_twice_in_the_corpus` caught.
fn strategies_over_one_base(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "merge",
        &[
            &["merge", "-s", "recursive", "--no-ff", "-m", "x", "main"],
            &["merge", "-s", "resolve", "--no-ff", "-m", "x", "main"],
            &["merge", "-s", "subtree", "--no-ff", "-m", "x", "main"],
            &["merge", "-s", "ours", "--no-ff", "-m", "x", "main"],
        ],
        out,
    );
    each_strict(
        Shape::Cherry,
        "merge",
        // The one refusal in the grid, so the sentence is compared too.
        &[&["merge", "-s", "octopus", "--no-ff", "-m", "x", "main"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// The `-X` grammar
// ---------------------------------------------------------------------------

/// The `-X` spellings and combinations [`super::merge_strategies`] does not
/// have: the long form, the valueless forms, the percentage forms, and pairs
/// that combine a *resolution* option with an *algorithm* option.
///
/// `-X ours -X theirs` is already there, and it is the easy pair — two options
/// that contradict, where last-wins is the whole answer. The pairs here do not
/// contradict: `-X ours -X patience` has to apply the favour-ours resolution
/// *and* run the patience diff underneath it, and an implementation that lets
/// the second `-X` replace the first rather than accumulate resolves
/// `clash.txt` by conflict instead of by `ours`. Measured on stock 2.55.0: both
/// orders exit 0 and commit, so accumulation is observable in the exit code
/// alone, and in the tree through the worktree probe.
///
/// The valueless forms are the other half. `merge-ort` accepts
/// `-X find-renames` (no `=`) and `-X subtree` (no `=<path>`) and rejects
/// `-X rename-threshold` with no value; a hand-written option parser gets the
/// three apart only by having been asked. Measured on stock 2.55.0:
/// `find-renames` and `subtree` exit 1 with the ordinary
/// `CONFLICT (content): Merge conflict in clash.txt`, `rename-threshold` exits
/// **128** with `fatal: unknown strategy option: -Xrename-threshold` — so the
/// three are separated by exit code and the last is strict. The port matches
/// stock on all three.
///
/// `-s resolve -X ours` and `-s octopus -X ours` are strict for the reason
/// [`super::merge_strategies`] gives for the backend refusals: the two shell
/// strategies take no options at all and the message *is* the behaviour.
/// (`-s ours -X theirs` is the third of the set and is not a refusal — `-s ours`
/// ignores the option and commits `HEAD`'s tree, which the state probe checks.)
fn strategy_option_grammar(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge",
        &[
            // The long spellings, attached and detached.
            &["merge", "--strategy-option=theirs", "cc-right"],
            &["merge", "--strategy-option", "ours", "cc-right"],
            // Resolution plus algorithm, in both orders: both must accumulate.
            &["merge", "-X", "ours", "-X", "patience", "cc-right"],
            &["merge", "-X", "patience", "-X", "ours", "cc-right"],
            &["merge", "-X", "theirs", "-X", "diff-algorithm=histogram", "cc-right"],
            &["merge", "-X", "ignore-space-change", "-X", "ours", "cc-right"],
            &["merge", "-X", "ours", "-X", "ignore-cr-at-eol", "cc-right"],
            // Two whitespace/normalization options together, neither of which
            // may change this merge's outcome.
            &["merge", "-X", "renormalize", "-X", "ignore-all-space", "cc-right"],
            // The value forms the corpus never wrote: no `=`, and `%`.
            &["merge", "-X", "find-renames", "cc-right"],
            &["merge", "-X", "find-renames=100%", "cc-right"],
            &["merge", "-X", "rename-threshold=100%", "cc-right"],
            &["merge", "-X", "subtree", "cc-right"],
            &["merge", "-X", "subtree=nosuch", "cc-right"],
            // `-X diff-algorithm=patience` is not the same option as
            // `-X patience`: the first goes through the algorithm name table,
            // the second is its own flag.
            &["merge", "-X", "diff-algorithm=patience", "cc-right"],
            // The option and the strategy naming the same shift.
            &["merge", "-s", "subtree", "-X", "subtree=cc", "cc-right"],
            // `-s ours` ignores every `-X` and must still produce HEAD's tree.
            &["merge", "-s", "ours", "-X", "theirs", "cc-right"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge",
        &[
            // An algorithm name that does not exist. Rejected by
            // `parse_merge_opt` rather than by the algorithm-name table, so the
            // sentence names the whole option:
            // `fatal: unknown strategy option: -Xdiff-algorithm=nonsense`,
            // exit 128, before any tree is touched.
            &["merge", "-X", "diff-algorithm=nonsense", "cc-right"],
            // A threshold option with no value at all —
            // `fatal: unknown strategy option: -Xrename-threshold`, also 128,
            // which is what separates it from the valueless forms above that
            // *are* accepted and reach the ordinary conflict at exit 1.
            &["merge", "-X", "rename-threshold", "cc-right"],
            // The two strategies that accept no options.
            &["merge", "-s", "resolve", "-X", "ours", "cc-right"],
            &["merge", "-s", "octopus", "-X", "ours", "cc-right"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "merge",
        &[
            // The same options over an add/add with **no merge base**, where
            // `-X ours`/`-X theirs` have only two sides to choose between and
            // the diff algorithm has nothing to diff against.
            &["merge", "-X", "diff-algorithm=patience", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            &["merge", "-s", "ort", "-X", "ours", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            &["merge", "-s", "ort", "-X", "theirs", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            &["merge", "-X", "subtree=alien.txt", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
        ],
        out,
    );
    each(
        Shape::Cherry,
        "merge",
        &[
            // A real three-way text merge of one file whose two sides edit
            // different hunks and share a third — the algorithm options have
            // something to be an algorithm about, which `clash.txt`'s single
            // line does not.
            &["merge", "-X", "patience", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "diff-algorithm=histogram", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "diff-algorithm=minimal", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "ignore-all-space", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "ours", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "theirs", "--no-ff", "-m", "x", "main"],
            &["merge", "-X", "subtree=app.txt", "--no-ff", "-m", "x", "main"],
        ],
        out,
    );
    each(Shape::Symlinks, "merge", &[&["merge", "-X", "subtree=dir", "-m", "sym", "sym-pending"]], out);
}

// ---------------------------------------------------------------------------
// The merge message: `--log`, `--cleanup`, `--signoff`, `-F`, `--into-name`
// ---------------------------------------------------------------------------

/// Everything between "the trees merged" and "this is the commit object", none
/// of which any `merge` case reached: the shortlog appendix, the four cleanup
/// modes, the trailer, the message read from a file, and the name the generated
/// subject merges *into*.
///
/// This is where a merge stops being a tree operation. `--log[=<n>]` appends
/// entries from `git shortlog HEAD..MERGE_HEAD`; `--cleanup=<mode>` decides
/// whether comment lines and trailing blanks survive; `--signoff` appends a
/// trailer; `-F <path>` replaces the whole message with a file's contents; and
/// `--into-name=<name>` changes the generated subject from `Merge branch 'x'
/// into y` to `Merge branch 'x'`. All five land in the commit object on a clean
/// merge and in `.git/MERGE_MSG` on a conflicted one, and both are compared —
/// the first through `for-each-ref`/`cat-file --batch-all-objects`, the second
/// through `probe_op_state`.
///
/// **`--cleanup=verbatim` is the one that changes the bytes of an otherwise
/// ordinary merge.** Every other mode strips, and stripping a message that
/// needs no stripping is a no-op; `verbatim` is the mode under which git's own
/// generated `Merge branch 'feature'` is committed exactly as generated. Stock
/// 2.55.0 and git 2.50.1 both write a 290-byte commit whose message has **no**
/// trailing newline; a port that appends one writes 291 bytes and a different
/// object id for the same tree and the same parents. **The port under test
/// appends one** — verified by hexdump: stock's commit object ends
/// `…Merge branch 'feature'` at 290 bytes, the port's ends
/// `…Merge branch 'feature'\n` at 291, and the two commit ids differ. That is
/// why this case is here rather than only the strip modes.
///
/// `--cleanup=scissors` is only observable on a **conflicted** merge, which is
/// the second thing the first draft got wrong. On a clean merge git commits
/// `Merge branch 'feature'` and the scissors block never appears — verified with
/// and without `-e`. On a conflicted one git writes the
/// `# ------------------------ >8 ------------------------` block into
/// `MERGE_MSG` above the `# Conflicts:` list, and that is what tells the editor
/// where the message ends. Both a criss-cross content conflict and a baseless
/// add/add are here because the two write `MERGE_MSG` from different code paths,
/// and **the port omits the whole block on both**, writing
/// `Merge branch 'cc-right' into cc-left\n\n# Conflicts:\n#\tclash.txt\n`
/// where stock writes the four scissors lines in between.
///
/// `-F no-such-file` is strict, and the first draft of this comment overstated
/// it: stock 2.55.0 and git 2.50.1 exit **129** with exactly one line,
/// `error: could not read file 'no-such-file'`, and **no usage block** — that
/// is `parse_options`'s own failure path rather than a `die()`. The port
/// answers `fatal: could not open 'no-such-file' for reading: No such file or
/// directory` at **128**. 129-versus-128 is the distinction a hand-rolled
/// parser loses.
fn message_machinery(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "merge",
        &[
            // The shortlog appendix, in each of its forms.
            &["merge", "--log", "--no-ff", "feature"],
            &["merge", "--log=1", "--no-ff", "feature"],
            &["merge", "--log=0", "--no-ff", "feature"],
            &["merge", "--no-log", "--no-ff", "feature"],
            // The trailer.
            &["merge", "--signoff", "--no-ff", "feature"],
            &["merge", "--no-signoff", "--no-ff", "feature"],
            // The four cleanup modes. `verbatim` is the one that can move bytes.
            &["merge", "--cleanup=verbatim", "--no-ff", "feature"],
            &["merge", "--cleanup=whitespace", "--no-ff", "feature"],
            &["merge", "--cleanup=strip", "--no-ff", "feature"],
            &["merge", "--cleanup=scissors", "--no-ff", "feature"],
            // The generated subject's "into" half.
            &["merge", "--into-name=trunk", "--no-ff", "feature"],
            // The whole message from a tracked file that exists in every shape.
            &["merge", "-F", "README.md", "--no-ff", "feature"],
            // Message and appendix together, over a squash rather than a merge.
            &["merge", "--squash", "--log", "feature"],
            &["merge", "--squash", "--signoff", "feature"],
            &["merge", "--squash", "--cleanup=verbatim", "feature"],
        ],
        out,
    );
    each_strict(
        Shape::Branched,
        "merge",
        &[
            // A cleanup mode that does not exist, and a message file that does
            // not: two different parse failures with two different exit codes.
            &["merge", "--cleanup=nonsense", "--no-ff", "feature"],
            &["merge", "-F", "no-such-file", "--no-ff", "feature"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "merge",
        &[
            // The same knobs where the message goes to `MERGE_MSG` instead of to
            // a commit, and the `# Conflicts:` list is already in it.
            &["merge", "--cleanup=verbatim", "cc-right"],
            &["merge", "--cleanup=whitespace", "cc-right"],
            &["merge", "--cleanup=strip", "cc-right"],
            &["merge", "--cleanup=default", "cc-right"],
            &["merge", "--cleanup=scissors", "cc-right"],
            &["merge", "--into-name=trunk", "cc-right"],
            &["merge", "--no-log", "cc-right"],
            &["merge", "--no-signoff", "cc-right"],
            &["merge", "--signoff", "cc-right"],
            &["merge", "--log", "--no-ff", "cc-right"],
            // `-m` twice: `builtin/merge.c` joins the two with a blank line.
            &["merge", "-m", "one", "-m", "two", "cc-right"],
            // Scissors over a squash: the block goes into `MERGE_MSG` while
            // `SQUASH_MSG` is written from the other code path, so one case
            // reads both files. Replaces a
            // `--cleanup=scissors --no-ff -m x cc-a` case the first draft had,
            // which measured nothing: `cc-a` is an ancestor of `cc-left`, so
            // stock answers `Already up to date.` and writes no message at all.
            &["merge", "--cleanup=scissors", "--squash", "cc-right"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "--cleanup=scissors", "--allow-unrelated-histories", "alien-clash"],
            &["merge", "--into-name=trunk", "--allow-unrelated-histories", "alien-clash"],
            &["merge", "--log", "--no-ff", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "--signoff", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "-F", "README.md", "--allow-unrelated-histories", "alien"],
        ],
        out,
    );
    each(
        Shape::Octopus,
        "merge",
        &[
            // `--log` over a range that is more than one commit, so the
            // shortlog has something to summarise and `=<n>` has something to
            // truncate.
            &["merge", "--log", "--no-ff", "-m", "x", "oct-side"],
            &["merge", "--log=1", "--no-ff", "-m", "x", "oct-side"],
        ],
        out,
    );
    each(Shape::Cherry, "merge", &[&["merge", "--log", "--no-ff", "-m", "x", "main"]], out);
}

// ---------------------------------------------------------------------------
// What a merge reports, and the flags that decide it
// ---------------------------------------------------------------------------

/// The end-of-merge report and the run-time flags around it: the diffstat in
/// its four spellings, the editor flag, verbosity, progress, autostash, the
/// ignored-file gate and signature verification.
///
/// Every one of these had zero occurrences across the corpus's 244 pre-existing
/// `merge` invocations. Most cannot change this merge's *result*, and that is
/// the contract being pinned: the option parses, reaches the right field, and
/// leaves the tree alone. `--stat`/`--summary`/`-n` are three names for two
/// settings of one field and `--compact-summary` is a fourth rendering of it;
/// an implementation that maps `--summary` to the wrong one prints a different
/// stdout for the same merge. `--compact-summary` is only *distinguishable*
/// from `--stat` where the merge creates, deletes or chmods a path: on
/// [`Shape::Branched`] it prints `feature.txt (new) | 1 +` and drops the
/// `create mode` line, while on [`Shape::Patches`], where nothing is created,
/// the two are byte-identical. Both are kept — the Patches pair is there for
/// the binary row, not for the rendering.
///
/// `-e` is worth its line because [`crate::env::harden`] pins `GIT_EDITOR` to
/// `true`: the editor is spawned, exits 0 without touching the file, and the
/// generated message is committed unchanged. So `-e` and `--no-edit` must
/// produce the *same commit*, and a port that only implements one of them —
/// or that skips the spawn and takes a different message path — diverges on the
/// object id rather than on stdout.
///
/// `--verify-signatures` is strict: no commit in any shape is signed, so stock
/// dies at 128 with `fatal: Commit 07e86d1 does not have a GPG signature.`, and
/// the refusal is the whole behaviour of the flag on this corpus. The port
/// matches, abbreviation included.
///
/// [`Shape::Patches`] appears here for one reason: `app/data.bin` is a binary
/// blob, and ` app/data.bin | Bin 1024 -> 1024 bytes` — verified verbatim on
/// stock — is a diffstat row no other merge in the corpus can produce.
fn report_knobs(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "merge",
        &[
            &["merge", "-n", "--no-ff", "feature"],
            &["merge", "--stat", "--no-ff", "feature"],
            &["merge", "--no-stat", "--no-ff", "feature"],
            &["merge", "--summary", "--no-ff", "feature"],
            &["merge", "--no-summary", "--no-ff", "feature"],
            &["merge", "--compact-summary", "--no-ff", "feature"],
            // The editor is `true`, so both of these commit the generated
            // message and must agree on the resulting object id.
            &["merge", "-e", "--no-ff", "feature"],
            &["merge", "--no-edit", "--no-ff", "feature"],
            &["merge", "--verbose", "--no-ff", "feature"],
            &["merge", "--progress", "--no-ff", "feature"],
            &["merge", "--no-progress", "--no-ff", "feature"],
            // Autostash over a clean worktree: nothing is stashed, and the
            // question is whether anything is *said*.
            &["merge", "--autostash", "--no-ff", "feature"],
            &["merge", "--no-autostash", "--no-ff", "feature"],
            &["merge", "--overwrite-ignore", "--no-ff", "feature"],
            &["merge", "--no-overwrite-ignore", "--no-ff", "feature"],
            &["merge", "--no-verify-signatures", "--no-ff", "feature"],
        ],
        out,
    );
    each_strict(
        Shape::Branched,
        "merge",
        &[
            // Nothing in this corpus is signed; the refusal names the commit.
            &["merge", "--verify-signatures", "--no-ff", "feature"],
        ],
        out,
    );
    each(
        Shape::Patches,
        "merge",
        &[
            // A diffstat with a binary row in it, in two renderings.
            &["merge", "--stat", "--no-ff", "-m", "x", "pending"],
            &["merge", "--compact-summary", "--no-ff", "-m", "x", "pending"],
        ],
        out,
    );
    each(
        Shape::CrissCross,
        "merge",
        &[
            // The stat flags on a merge that *fails*: there is no diffstat to
            // print, so the flag has to be accepted and then not act.
            &["merge", "-n", "cc-right"],
            &["merge", "--stat", "cc-right"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "merge",
        &[&["merge", "--no-overwrite-ignore", "--allow-unrelated-histories", "-m", "join", "alien"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// The `--no-` half of the option table
// ---------------------------------------------------------------------------

/// Every `--no-<opt>` `builtin/merge.c`'s option table generates, asked for by
/// name.
///
/// `parse_options` gives a negation to every option not marked `PARSE_OPT_NONEG`
/// — including the ones for which a negation is meaningless, like `--no-abort`
/// and `--no-strategy`. They are meaningless in effect and *not* meaningless in
/// parsing: `git merge --no-abort feature` is a perfectly ordinary merge of
/// `feature`. A port that hand-writes the parser enumerates the negations it
/// thought of, and the ones it did not fall through to `cmd_merge`'s
/// "then it must be a rev" branch, where they become
/// `merge: --no-abort - not something we can merge` and the merge silently does
/// not happen. That failure mode is invisible to every other case in the corpus,
/// because no other case writes a negation down.
///
/// **Five of the thirteen negations below do exactly that in the port under
/// test**, verified by hand against both gits: `--no-abort`, `--no-quit`,
/// `--no-continue`, `--no-strategy-option` and `--no-overwrite-ignore` each
/// answer `merge: <opt> - not something we can merge` at exit 1 and leave
/// `main` where it was, while stock fast-forwards to `feature` at exit 0. The
/// other eight — `--no-into-name`, `--no-cleanup`, `--no-compact-summary`,
/// `--no-strategy`, `--no-message`, `--no-verify`,
/// `--no-allow-unrelated-histories`, and `-s ort --no-strategy` — pass, which
/// is what makes the five a parser gap rather than "negations are unported".
///
/// One option that is genuinely *not* negatable is here too, and strict, so the
/// set is measured from both sides: `--no-file` exits 129 with
/// `error: unknown option `no-file'` followed by the usage block, because `-F`
/// is `PARSE_OPT_NONEG`. The port matches. (`--no-ff-only`, the other one, is
/// already in [`super::merge_family`] and is not repeated.)
fn option_table_negations(out: &mut Vec<Case>) {
    each(
        Shape::Branched,
        "merge",
        &[
            &["merge", "--no-into-name", "--no-ff", "feature"],
            &["merge", "--no-cleanup", "--no-ff", "feature"],
            &["merge", "--no-compact-summary", "--no-ff", "feature"],
            &["merge", "--no-strategy", "--no-ff", "feature"],
            &["merge", "--no-strategy-option", "--no-ff", "feature"],
            &["merge", "--no-message", "--no-ff", "feature"],
            &["merge", "--no-verify", "--no-ff", "feature"],
            &["merge", "--no-allow-unrelated-histories", "feature"],
            // The three state verbs, negated: no state verb runs, and the merge
            // proceeds normally.
            &["merge", "--no-abort", "feature"],
            &["merge", "--no-quit", "feature"],
            &["merge", "--no-continue", "feature"],
            // A negation after the positive form of the same option: the last
            // one wins and the merge runs under the default strategy.
            &["merge", "-s", "ort", "--no-strategy", "feature"],
        ],
        out,
    );
    each_strict(
        Shape::Branched,
        "merge",
        &[
            // The two options `parse_options` marks non-negatable.
            &["merge", "--no-file", "--no-ff", "feature"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// `--abort` / `--quit` / `--continue` and their argument check
// ---------------------------------------------------------------------------

/// The three state verbs asked to run with something else on the command line.
///
/// `cmd_merge` checks these before it looks at anything else: each of the three
/// `die(_("--abort expects no arguments"))` and friends fires when *any* other
/// argument survived option parsing, and the die is followed by the usage block
/// and exit **129**. Two distinct mistakes are separated here, and the corpus
/// had neither:
///
/// * a stray **rev** (`merge --abort cc-right`) — the port under test produces
///   the same sentence but not the usage block that follows it, which is why
///   these are strict;
/// * a stray **option** (`merge --abort -s ort`) — the port's check counts
///   positional arguments only, so `-s ort` is consumed, the check passes, and
///   the abort runs and fails for an unrelated reason at a different exit code.
///
/// The shape is [`Shape::CrissCross`], where no merge is in progress, so the
/// argument check is reached and the "there is no merge to abort" path is not.
///
/// Verified by hand on all three binaries. `merge --abort cc-right`: stock and
/// the port both say `fatal: --abort expects no arguments` at exit 129, and the
/// port stops there while stock prints the usage block after it.
/// `merge --abort -s ort`: stock still refuses at 129, the port consumes
/// `-s ort`, passes its own argument check, and runs the abort, failing at
/// **128** with `fatal: There is no merge to abort (MERGE_HEAD missing).`;
/// `--continue -s ort` is the same shape of mistake with
/// `fatal: There is no merge in progress (MERGE_HEAD missing).`
///
/// All four land in the harness's `gits-disagree` bucket, and the reason is
/// worth knowing before reading that bucket: the *content* of git's usage block
/// changed between 2.50.1 and 2.55.0 (`--[no-]compact-summary` is new, and the
/// option order moved), so the two oracles differ on these bytes. The finding
/// being pinned — the block is absent, and the argument check is passed by an
/// option — is the same against both.
fn state_verbs_reject_arguments(out: &mut Vec<Case>) {
    each_strict(
        Shape::CrissCross,
        "merge",
        &[
            &["merge", "--abort", "cc-right"],
            &["merge", "--quit", "cc-right"],
            &["merge", "--abort", "-s", "ort"],
            &["merge", "--continue", "-s", "ort"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// `merge.*` values the corpus never set
// ---------------------------------------------------------------------------

/// The `merge.*` keys and values no case delivered, including the ones that make
/// git **refuse to start**.
///
/// **What most of these keys can and cannot show, measured rather than
/// assumed.** Each key below was run against `merge cc-right` on
/// [`Shape::CrissCross`] twice — once with the key set and once without — and
/// the two runs compared on exit code, stdout, stderr, refs, `ls-files --stage`
/// and the operation-state files. Only five changed anything: `merge.log=true`,
/// `merge.suppressDest=cc-left`, `merge.ff=false`, `merge.autoStash=true` and
/// `merge.conflictStyle` at `diff3`/`zdiff3`. `merge.verbosity` at **1, 2, 3
/// and 4 changes nothing** (on a clean [`Shape::Branched`] merge either — only
/// 5 does, and that case is already in the corpus), and neither do
/// `merge.directoryRenames`, `merge.renames`, `merge.renameLimit=0`,
/// `merge.stat=false`, `merge.branchdesc` or `merge.tool` on a shape with no
/// rename, no diffstat, no branch description and no mergetool invocation.
///
/// They are kept anyway, and the claim is the smaller one: the key is **read
/// and accepted** rather than rejected or ignored into a `die`. `verbosity=2`
/// was dropped, because 2 is the default and that case was a byte-for-byte
/// duplicate of the corpus's plain `merge cc-right`; the id-uniqueness test
/// cannot see a duplicate that differs only in a config segment.
///
/// **`merge.renameLimit=nonsense` is the case that matters most in this
/// function, and it is strict.** It is not a rename question at all: it is
/// `git_config_int()` refusing a non-numeric value. Stock 2.55.0 and git 2.50.1
/// both die with `fatal: bad numeric config value 'nonsense' for
/// 'merge.renamelimit': invalid unit` at exit 128 **before touching anything**.
/// **The port shrugs the value off, and this is the worst failure in the
/// module** — verified by hand on all three binaries. It does not merely print
/// differently: it performs the merge. On `merge cc-right` it conflicts at exit
/// 1 and leaves `clash.txt` at stages 1/2/3 where both gits left the index
/// untouched at 128; on the clean `alien` merge below it exits **0 and creates
/// a commit** (`refs/heads/main` moves) where both gits created nothing. Both a
/// conflicting and a committing merge are here for exactly that reason: the
/// failure is only visible as a written object on the second.
///
/// `merge.autoStash=true` over a **clean** worktree is the other one worth
/// naming. No stash is created (verified: `stash list` is empty afterwards on
/// both gits), and both gits still print `When finished, apply stashed changes
/// with \`git stash pop\`` when the merge fails — the hint is emitted from the
/// failure path unconditionally, not from the stash. **The port omits exactly
/// that line**, on both cases here — the criss-cross content conflict and the
/// baseless add/add — and agrees on everything else.
///
/// `merge.ff=only` over a merge that cannot fast-forward is strict: the refusal
/// is the whole meaning of the value, and it comes from a different place than
/// `--ff-only`'s.
fn merge_config_values(out: &mut Vec<Case>) {
    // 2 is the default, so a case setting it would duplicate the corpus's
    // plain `merge cc-right`; 1, 3 and 4 pin that the key is accepted at each
    // end of the non-debug range. None of them changes stock's output.
    for level in ["1", "3", "4"] {
        out.push(
            Case::new("merge", &["merge", "cc-right"], Shape::CrissCross)
                .with_config(&[("merge.verbosity", level)]),
        );
    }
    for (key, value) in [
        ("merge.directoryRenames", "true"),
        ("merge.directoryRenames", "false"),
        ("merge.renames", "true"),
        ("merge.renames", "copies"),
        ("merge.renameLimit", "0"),
        ("merge.log", "true"),
        ("merge.stat", "false"),
        ("merge.branchdesc", "true"),
        ("merge.suppressDest", "cc-left"),
        ("merge.tool", "nonsense"),
        ("merge.ff", "false"),
    ] {
        out.push(
            Case::new("merge", &["merge", "cc-right"], Shape::CrissCross)
                .with_config(&[(key, value)]),
        );
    }
    // The hint printed from the failure path, with nothing actually stashed.
    out.push(
        Case::new("merge", &["merge", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.autoStash", "true")]),
    );
    out.push(
        Case::new(
            "merge",
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            Shape::Unrelated,
        )
        .with_config(&[("merge.autoStash", "true")]),
    );
    // A value git refuses to parse. Strict, and on both a merge that would have
    // conflicted and one that would have committed.
    out.push(
        Case::strict("merge", &["merge", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.renameLimit", "nonsense")]),
    );
    out.push(
        Case::strict(
            "merge",
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien"],
            Shape::Unrelated,
        )
        .with_config(&[("merge.renameLimit", "nonsense")]),
    );
    // The config half of `--ff-only`, over a merge that cannot fast-forward.
    out.push(
        Case::strict("merge", &["merge", "cc-right"], Shape::CrissCross)
            .with_config(&[("merge.ff", "only")]),
    );
    // The flag half, which no case had either.
    out.push(Case::strict("merge", &["merge", "--ff-only", "cc-right"], Shape::CrissCross));
    // Two conflict styles over the **baseless** add/add, where the base section
    // a diff3 marker names is empty. `merge_strategies` measures the three
    // styles over a criss-cross, which always has a base to show.
    for style in ["diff3", "zdiff3", "merge"] {
        out.push(
            Case::new(
                "merge",
                &["merge", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
                Shape::Unrelated,
            )
            .with_config(&[("merge.conflictStyle", style)]),
        );
    }
    // The width every object id `merge` prints at. Attached to a
    // **fast-forward**, which is the only merge that prints an id at all:
    // `Updating 5915d79..07e86d1` becomes `Updating 5915d79de18d..07e86d1fedb7`
    // under `core.abbrev=12`. The first draft attached this to
    // `merge cc-right`, which prints no object id anywhere, so the setting had
    // nothing to act on.
    //
    // A `diff.statGraphWidth=10` case sat beside it and is deleted rather than
    // moved: it was written against `merge --stat --no-ff -m x cc-a`, where
    // `cc-a` is an ancestor of `cc-left` and stock answers `Already up to
    // date.` with no diffstat at all — and no merge anywhere in this corpus
    // produces a stat bar wider than the ten columns the key would cap, so
    // there is no invocation to move it to.
    out.push(
        Case::new("merge", &["merge", "feature"], Shape::Branched)
            .with_config(&[("core.abbrev", "12")]),
    );
}

// ---------------------------------------------------------------------------
// `--squash` over a range that contains a merge commit
// ---------------------------------------------------------------------------

/// What `--squash` writes into `.git/SQUASH_MSG`, on a range that contains a
/// merge commit and on three ranges that do not.
///
/// `merge --squash` builds `SQUASH_MSG` by walking `HEAD..MERGE_HEAD` and
/// pasting each commit in — merge commits included, with their `Merge:` line.
/// [`Shape::CrissCross`] is the only shape whose `HEAD..<other tip>` range
/// contains one: `cc-right` is `criss-cross: cc-right tip` on top of
/// `criss-cross: cc-right merge`, and the second is a two-parent commit.
///
/// **This is a finding, verified by hand on all three binaries.** Stock 2.55.0
/// and git 2.50.1 write both commits into `SQUASH_MSG`, the merge one carrying
/// its `Merge: 27e7a99 0a24ba3` line. The port writes only
/// `criss-cross: cc-right tip` — it filters merge commits out of the
/// `HEAD..MERGE_HEAD` walk. The difference is invisible in stdout (`--squash`
/// prints only the conflict lines and
/// `Squash commit -- not updating HEAD`) and invisible in the refs, so nothing
/// short of reading `SQUASH_MSG` catches it. All four cases on
/// [`Shape::CrissCross`] below fail this way, including `-s ours --squash`,
/// which reaches the same file down a different path at exit 0.
///
/// The three controls are the point of the group as much as the finding is:
/// [`Shape::Unrelated`]'s `alien` (two commits, no merge), [`Shape::Octopus`]'s
/// `oct-side` and [`Shape::Cherry`]'s `main` all squash ranges with no merge
/// commit in them, so a failure here is specifically about the merge commit and
/// not about squash in general.
///
/// `-s ours --squash` is included because it takes a different path to the same
/// file: the strategy resolves without touching the tree and the squash message
/// is still written.
fn squash_over_a_merge(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge",
        &[
            &["merge", "--squash", "cc-right"],
            &["merge", "--squash", "--no-commit", "cc-right"],
            &["merge", "-s", "ours", "--squash", "cc-right"],
            &["merge", "--squash", "--cleanup=verbatim", "cc-right"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "--squash", "--allow-unrelated-histories", "alien"],
            &["merge", "--squash", "-s", "ours", "--allow-unrelated-histories", "alien"],
            &["merge", "--squash", "--allow-unrelated-histories", "alien-clash"],
        ],
        out,
    );
    each(Shape::Octopus, "merge", &[&["merge", "--squash", "oct-side"]], out);
    each(
        Shape::Cherry,
        "merge",
        &[&["merge", "--squash", "--no-commit", "main"], &["merge", "--squash", "main"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// A merge run from a subdirectory
// ---------------------------------------------------------------------------

/// The same merges, run from a directory below the worktree root.
///
/// Nine pre-existing ids under `merge`/`merge-recursive` carry a `cwd`, and
/// they are two things: `merge main` in a bare repository on
/// [`Shape::BehindRemote`], and a seven-step [`Shape::Conflicted`] sequence run
/// from `src`. The first is not a worktree merge and the second is one shape.
///
/// The engine prints every path it touches — `Auto-merging <path>`,
/// `CONFLICT (content): Merge conflict in <path>`, the diffstat rows, and the
/// `# Conflicts:` list in `MERGE_MSG` — and git prints all of them **relative to
/// the worktree root** regardless of where the command was run. An
/// implementation that renders any of them relative to the current directory
/// produces `../README.md` for a conflict two levels up. These four cases put
/// that question on four shapes the existing `cwd` cases do not touch.
///
/// Four directories that exist in their shapes: `src/` and `app/` hold files the
/// merge writes, `dir/` holds the path whose *type* changes.
fn from_a_subdirectory(out: &mut Vec<Case>) {
    out.push(
        Case::new(
            "merge",
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            Shape::Unrelated,
        )
        .in_dir("src"),
    );
    out.push(
        Case::new(
            "merge",
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien"],
            Shape::Unrelated,
        )
        .in_dir("src"),
    );
    out.push(
        Case::new("merge", &["merge", "--no-ff", "-m", "x", "pending"], Shape::Patches)
            .in_dir("app"),
    );
    out.push(
        Case::new("merge", &["merge", "-m", "sym", "sym-pending"], Shape::Symlinks).in_dir("dir"),
    );
}

// ---------------------------------------------------------------------------
// `merge-recursive`'s own option spellings, over a single merge base
// ---------------------------------------------------------------------------

/// The backend invoked directly, with the long options `-X` feeds, over a
/// history with **one** merge base.
///
/// The corpus's 25 pre-existing `merge-recursive` invocations span twelve
/// shapes, but eleven of them are on [`Shape::CrissCross`], where two explicit
/// bases are given and the backend has to build a virtual one first — and every
/// one of the *option*-carrying invocations is there. That is the harder path
/// and the right one to have, but it means every option is measured through a
/// recursion. [`Shape::Cherry`] is the complement: `topic` and `main` have exactly one
/// merge base (`cherry: seed`), `app.txt` is edited on both sides — the same
/// hunk on one line, different hunks on two others — and the result is an
/// ordinary three-way text merge. So these cases measure the option's effect on
/// `ll_merge` rather than on base construction, and `merge-recursive`'s `<head>`
/// is `topic`, which is what `Shape::Cherry` has checked out (the backend writes
/// through `unpack_trees` and fails the up-to-date check against any other).
///
/// Seven of the option names below appear on **no `merge-recursive`
/// invocation** in the corpus: `--ignore-space-change`, `--ignore-all-space`,
/// `--ignore-space-at-eol`, `--ignore-cr-at-eol`,
/// `--renormalize`/`--no-renormalize`, `--rename-threshold=` and `--subtree=`.
/// The whole pre-existing option set on that command is `--ours`, `--theirs`,
/// `--patience`, `--diff-algorithm=histogram`, `--no-renames` and one
/// `--find-renames=90`. (Most of the seven do appear elsewhere in the corpus —
/// under `merge -X`, `diff`, `merge-file` — so this is a claim about the
/// backend's own parser, not about the tokens being unseen.)
///
/// The one case that is not on `Cherry` is the one that cannot be:
/// `--subtree=<path>` **with two explicit merge bases**. The shift has to be
/// threaded through the recursion that builds the virtual base, and that is a
/// distinct code path from either `-X subtree=` on a two-base merge (which
/// `merge` reaches through its own base computation and which the port handles)
/// or `--subtree=` on a one-base merge. Verified by hand: both gits merge
/// (`Auto-merging cc.txt` / `Auto-merging clash.txt` /
/// `CONFLICT (content): Merge conflict in clash.txt`), conflict at exit 1, and
/// leave `clash.txt` at stages 1/2/3 with `AUTO_MERGE` written. The port
/// refuses at **128** with `fatal: merge-recursive --subtree cannot be
/// performed: 2 explicit merge bases require a virtual merge base built by
/// recursively merging them with the subtree shift applied at each level;
/// Repository::virtual_merge_base cannot thread the shift through its
/// recursion`, leaving a stage-0 index and no `AUTO_MERGE`. Strict, because
/// that sentence is the finding.
fn recursive_options_over_one_base(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "merge-recursive",
        &[
            // The baseline this group is read against.
            &["merge-recursive", "main~2", "--", "topic", "main"],
            // Algorithm selection, through the name table.
            &["merge-recursive", "--diff-algorithm=patience", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--diff-algorithm=histogram", "main~2", "--", "topic", "main"],
            // The four whitespace options, none of which appears anywhere in the
            // corpus in this spelling.
            &["merge-recursive", "--ignore-space-change", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--ignore-all-space", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--ignore-space-at-eol", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--ignore-cr-at-eol", "main~2", "--", "topic", "main"],
            // Renormalization, both ways.
            &["merge-recursive", "--renormalize", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--no-renormalize", "main~2", "--", "topic", "main"],
            // Rename detection: a threshold, and the valueless form.
            &["merge-recursive", "--rename-threshold=25", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--find-renames", "main~2", "--", "topic", "main"],
            // The subtree shift with an empty operand and with a real one, over
            // a single base.
            &["merge-recursive", "--subtree=", "main~2", "--", "topic", "main"],
            &["merge-recursive", "--subtree=app.txt", "main~2", "--", "topic", "main"],
        ],
        out,
    );
    each_strict(
        Shape::Cherry,
        "merge-recursive",
        &[
            // An option `parse_merge_opt` does not know, from the direct
            // invocation rather than from `-X`.
            &["merge-recursive", "--no-such-opt", "main~2", "--", "topic", "main"],
        ],
        out,
    );
    each_strict(
        Shape::CrissCross,
        "merge-recursive",
        &[
            // The subtree shift threaded through a virtual base.
            &["merge-recursive", "--subtree=cc", "cc-a", "cc-b", "--", "cc-left", "cc-right"],
        ],
        out,
    );
}
