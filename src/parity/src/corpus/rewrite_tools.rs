//! The **bulk** history tools: the verbs that rewrite or summarise a whole
//! history rather than one commit — `filter-branch`, `subtree`, `replace`,
//! `request-pull`, `shortlog` and `whatchanged`.
//!
//! Three of these are shell scripts in stock git (`git-filter-branch`,
//! `git-subtree`, `git-request-pull`), which makes them a different kind of
//! target from a builtin: their control flow is `case`/`die`/`||`, their exit
//! codes come from whichever child spoke last, and a port that reimplements the
//! logic in Rust diverges at the seams — where the script *ignores* a failed
//! child, or catches one and re-words it. **Two of the three defects this module
//! pins are exactly that seam**, and neither is visible from a happy path: one
//! is a `rev-parse` failure `git-subtree` never checks, the other a `rev-parse`
//! failure `git-filter-branch` catches and answers with its own status. The
//! third is not a script at all — `replace --graft` reading *through* an
//! existing replacement — and it is the one that leaves the repository wrong.
//!
//! # How this divides territory with the five modules that already touch these verbs
//!
//! * **`history_rewrite.rs`** is the nearest neighbour and owns the *canonical*
//!   invocation of each verb: `filter-branch` with one filter of each kind on
//!   `Branched`/`Merged`/`Linear`, `subtree split --prefix=src` on
//!   `Branched`/`Merged`/`AwkwardPaths` plus the `ensure_clean` refusals, and
//!   `replace`/`--graft`/`--edit`/`--convert-graft-file` on `Branched`,
//!   `Merged` and `Linear`. Every one of its cases runs against a repository
//!   with **no `refs/replace/*` in it**, which is the axis this file adds.
//! * **`misc_commands.rs`** owns `filter-branch`'s no-`-f` spellings
//!   (`--force`, bare `filter-branch`, `--original refs/parity-backup`) on
//!   `Branched`.
//! * **`stateful_side_files.rs`** owns the `.git`-side-file view: the
//!   `filter-branch` refusal on `Dirty` (stderr-strict), the
//!   `--index-filter`/`--subdirectory-filter` rewrites whose product is
//!   `refs/original/*`, and the largest `replace` group in the corpus —
//!   `--format=`, `-l <pattern>`, `--raw --edit`, and eight `--graft` forms, all
//!   on `Branched`/`Merged`/`Octopus`/`Damaged`.
//! * **`fixture_gaps2.rs`** owns `replace -l` in its five `--format` spellings
//!   on [`Shape::NotesReplace`] — the *listing* of an existing replacement.
//!   Listing is all it does there; nothing in the corpus asked what happens when
//!   a **write** verb runs over a repository that is already replaced, which is
//!   [`replace_over_replaced`] below and where the port's `--graft` defect lives.
//! * **`integrity_gc.rs`** owns `gc`/`prune`/`count-objects` over
//!   `Shape::NotesReplace` — whether a replacement's objects survive
//!   maintenance. It never runs `replace` itself.
//! * **`helpers_credentials.rs`**, **`mail_patch.rs`** and **`mail_series.rs`**
//!   own `request-pull` between them and cover it thoroughly: the three-argument
//!   form, `-p`, tag arguments (annotated, chained, lightweight, blob- and
//!   tree-pointing), an empty range, a missing upstream, and a peer at
//!   `./.remote.git`. What none of them does is hand it a **remote name**
//!   instead of a URL, or point it at a history that is replaced — those two are
//!   [`request_pull_resolution`].
//! * **`diff_family.rs`**, **`history_query.rs`** and
//!   **`history_simplification.rs`** own `shortlog` and `whatchanged` in full on
//!   the ordinary shapes — every `-n`/`-s`/`-e`/`-w`/`--group=`/`--committer`
//!   spelling, stdin payloads, and pathspec-limited walks. Repeating any of that
//!   here would measure the fixture twice, so [`summaries_over_replaced`] takes
//!   only the axis they cannot reach: the same summaries over a history whose
//!   commits and blobs are **substituted** at read time, with and without the
//!   substitution turned off.
//! * **`merge_strategies.rs`**'s twenty-odd "subtree" hits are `merge -s
//!   subtree` / `merge -X subtree=` / `merge-subtree`, which is the merge
//!   strategy and not the `git subtree` script. No overlap.
//! * **`bisect_replay.rs`** owns `git replay`; nothing here touches it.
//!
//! # `filter-branch`'s progress line is not reproducible, and that bounds this file
//!
//! `git-filter-branch` prints one line per rewritten commit to **stdout**:
//!
//! ```text
//! \rRewrite <oid> (i/n) (N seconds passed, remaining M predicted)
//! ```
//!
//! `N` is `$(date +%s)` minus a timestamp taken a few forks earlier
//! (`git-filter-branch` lines 352–363), so it is 0 unless the run happens to
//! straddle a second boundary — and when it does, `next_sample_at` moves too, so
//! the *counter* `(i/n)` desynchronises as well and a later line can repeat an
//! earlier `i`. Measured on stock 2.55.0 in a scratch copy of the fixtures, with
//! `FILTER_BRANCH_SQUELCH_WARNING=1` set on both runs:
//!
//! ```text
//! # 20 runs, Shape::Linear (one commit to rewrite)
//! 19  (0 seconds
//!  1  (1 seconds
//!
//! # two runs, Shape::Branched, `-f --msg-filter "printf 'rewritten\n'" -- --all`
//! run 1: … (1/3) (0 seconds …    … (2/3) (1 seconds …    … (2/3) (1 seconds …
//! run 2: … (1/3) (0 seconds …    … (2/3) (0 seconds …    … (3/3) (0 seconds …
//! ```
//!
//! The **state** is not affected: the two runs above left byte-identical
//! `for-each-ref`/`cat-file --batch-all-objects` digests, so
//! `env::harden`'s pinned `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` do make the
//! rewritten commit ids reproducible and they are a legitimate assertion target
//! — it is only the progress text that is not.
//!
//! Two consequences, both taken rather than papered over:
//!
//!  * **Most of the `filter-branch` group here dies before the rewrite loop
//!    starts.** Every path that fails in `git-filter-branch` lines 112–342 —
//!    an unclean tree, an occupied `tempdir`, an occupied backup namespace, a
//!    rev-list argument selecting nothing, a ref that is not a committish, a
//!    failed `--setup` — prints **no** progress line at all and is therefore
//!    exactly reproducible. Those are [`filter_branch_before_the_loop`], and
//!    both divergences this file found in `filter-branch` are in it.
//!  * **The rewrites that do run are on [`Shape::Linear`]**
//!    ([`filter_branch_one_commit`]), which has a single commit and therefore
//!    prints a single progress line taken microseconds after the start
//!    timestamp. [`filter_branch_structure`] holds the two cases that need more
//!    than one commit because the *structure* is the point; they are expected to
//!    land in `Verdict::Nondeterministic` (excluded, never a false failure) on
//!    some fraction of runs, and are kept to two for that reason.
//!
//! # `FILTER_BRANCH_SQUELCH_WARNING`, and why every case here sets it
//!
//! Without it the script prints a six-line deprecation banner to stdout and then
//! **sleeps ten seconds** (`git-filter-branch` lines 86–98). `history_rewrite.rs`
//! and `misc_commands.rs` both say in their headers that this is why their
//! `filter-branch` case counts are kept low, and both say the corpus cannot set
//! the variable. That was true when `Case` carried only `cmd`/`args`/`shape`; it
//! is not now, and **`stateful_side_files.rs` got there first** — its own
//! `SQUELCH` constant is this same pair, on its four rows, for this same reason.
//! So the variable is already a measured dimension and this file claims no
//! credit for it: a port that ignored it would print the banner on one side of
//! *those* rows. What is new here is only the scale it makes affordable.
//!
//! Measured against the binary under test, `Shape::Linear`,
//! `-f --msg-filter cat HEAD`:
//!
//! ```text
//! without FILTER_BRANCH_SQUELCH_WARNING : 10.99s wall, stdout opens "WARNING: git-filter-branch has a glut of gotchas…"
//! with    FILTER_BRANCH_SQUELCH_WARNING=1:  0.73s wall, stdout opens "Rewrite edfab1b7… (1/1) …"
//! ```
//!
//! Eleven seconds against under one, per invocation, on a group of twenty-eight
//! rows that a failure re-runs up to four times per side. Without it this group
//! would cost more wall clock than every other verb in this file put together,
//! which is exactly why the two modules above stopped at eight rows and four.
//!
//! # What could not be measured, and why
//!
//!  * **`replace --convert-graft-file` against a real graft file.** The
//!    conversion reads `.git/info/grafts`, and no shape in `fixture.rs` writes
//!    one — `graft_partial.rs` is about `.git/shallow` and promisor packs, which
//!    are different files with different semantics. A case cannot create one
//!    (a case is one argv against a pristine copy) and this module may not add a
//!    shape, so only the empty-input path is reachable and that is what
//!    [`replace_over_replaced`] pins.
//!  * **`subtree add` / `merge` / `pull` / `--rejoin` succeeding.**
//!    `Templates::instantiate` copies the fixture file by file, so every inode
//!    moves and the index stat cache is stale; `git-subtree`'s `ensure_clean`
//!    runs `git diff-index HEAD` and always sees modifications. Re-verified on
//!    stock 2.55.0 for this module — `fatal: working tree has modifications.
//!    Cannot add.` — so those verbs are still refusal-only, exactly as
//!    `history_rewrite.rs` recorded. The cases here take the branches that are
//!    decided *before* `ensure_clean` (argument validation) and the ones that do
//!    not need a clean tree at all (`split`, `push`).
//!  * **`filter-branch` in a bare repository.** `require_clean_work_tree` is
//!    skipped when `is_bare_repository` is true, which is a branch no shape can
//!    reach: there is no bare fixture, and `--git-dir` at a bare peer would make
//!    the case name a path.
//!  * **`subtree split -d`'s cache directory.** The debug stream names
//!    `.git/subtree-cache/$$`, so it embeds the process id and can never be
//!    compared. It is on **stderr**, which this module never opts into
//!    comparing, and `probe_storage` reads only `.git/objects`, so the directory
//!    the split leaves behind is invisible to the state digest — verified by
//!    listing it after a stock run. The `-d` case below is therefore measurable
//!    on stdout, exit code and state, and is deliberately not `Case::strict`.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    filter_branch_before_the_loop(out);
    filter_branch_one_commit(out);
    filter_branch_structure(out);
    subtree_split_onto(out);
    subtree_argument_gates(out);
    replace_over_replaced(out);
    request_pull_resolution(out);
    summaries_over_replaced(out);
}

/// The deprecation banner and its ten-second sleep, squelched.
///
/// Byte-identical to `stateful_side_files.rs`'s constant of the same name, and
/// deliberately a second copy rather than a shared one: the two modules are
/// edited independently, a shared constant would put one file's cases at the
/// mercy of the other's refactor, and the pair is four tokens. What matters is
/// that both deliver the *same* variable to both sides, which is checked by the
/// fact that a divergence in it would show up as a stdout difference on every
/// row of both groups at once.
const SQUELCH: &[(&str, &str)] = &[("FILTER_BRANCH_SQUELCH_WARNING", "1")];

/// One `filter-branch` case, with the banner squelched on both sides.
fn fb(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("filter-branch", args, shape).with_env(SQUELCH));
}

/// One `subtree` case.
fn st(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("subtree", args, shape));
}

/// One `request-pull` case.
fn rpull(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("request-pull", args, shape));
}

/// One `replace` case against the shape that already carries replacements.
fn repl(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::new("replace", args, Shape::NotesReplace));
}

/// One summary case against the shape that already carries replacements. The
/// subcommand is passed separately because `Case::new` wants a `&'static str`
/// and `args[0]` borrowed from a slice parameter is not one.
fn nr(out: &mut Vec<Case>, cmd: &'static str, args: &[&str]) {
    out.push(Case::new(cmd, args, Shape::NotesReplace));
}

// ---------------------------------------------------------------------------
// filter-branch
// ---------------------------------------------------------------------------

/// The `filter-branch` paths that die **before** the rewrite loop.
///
/// `git-filter-branch` does all of its validation in lines 112–342 and only then
/// enters the `while read commit parents` loop that prints the wall-clock
/// progress line. Everything in this group therefore has byte-stable stdout
/// (usually empty) on stock, which is what makes it the only part of
/// `filter-branch` that can be asserted on exactly — and it is where both of the
/// port's `filter-branch` defects turned out to be.
///
/// The two divergences, reproduced by hand against stock 2.55.0 and the binary
/// under test in copies of the templates:
///
/// ```text
/// $ git filter-branch -f --original refs/heads --msg-filter cat HEAD    # Shape::Linear
/// stock: exit 1   stderr: You must specify a ref to rewrite.
/// zvcs : exit 128 stderr: fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree.
///
/// $ git filter-branch -f --msg-filter cat blobtag                       # Shape::TagChain
/// stock: exit 1   stderr: WARNING: not rewriting 'refs/tags/blobtag' (not a committish)
///                         You must specify a ref to rewrite.
/// zvcs : exit 128 stderr: fatal: ambiguous argument 'blobtag': unknown revision or path not in the working tree.
/// ```
///
/// Both are the same seam and both arrive at it differently, which is why both
/// are kept. The script runs `git rev-parse --no-flags --revs-only
/// --symbolic-full-name --default HEAD "$@"`, *filters* the result to the refs
/// that peel to a commit, and dies with its own message and status 1 when
/// nothing survives (lines 269–284); the port lets `rev-parse`'s own 128 escape.
/// `--original refs/heads` reaches it by aiming the backup namespace at the
/// branch namespace, so `-f` deletes `refs/heads/main` (line 263) and leaves
/// `HEAD` dangling — **both sides delete the branch, identically**, so the state
/// digests agree and only the exit code moves. `blobtag` reaches it by naming a
/// tag whose target is a blob, so the ref exists and simply does not peel.
///
/// The state agreeing is what makes these exit-code-only findings rather than
/// wrong rewrites: neither side rewrote anything.
fn filter_branch_before_the_loop(out: &mut Vec<Case>) {
    // --- the two divergences ---
    fb(out, Shape::Linear, &["filter-branch", "-f", "--original", "refs/heads", "--msg-filter", "cat", "HEAD"]);
    fb(out, Shape::TagChain, &["filter-branch", "-f", "--msg-filter", "cat", "blobtag"]);

    // The same backup-namespace collision **without** `-f`: the script refuses
    // instead of deleting, dying "Cannot create a new backup." at line 258. The
    // pair is the whole meaning of `-f`, and only one half of it diverges.
    fb(out, Shape::Linear, &["filter-branch", "--original", "refs/heads", "--msg-filter", "cat", "HEAD"]);

    // `-d` at a path that already exists. Without `-f` the script refuses
    // ("<tempdir> already exists, please remove it", line 227); with `-f` it
    // `rm -rf`s the directory first, which for `.git` means deleting the
    // repository out from under itself and then failing to find it. Both sides
    // agree on both, and the second is git's own documented foot-gun rather
    // than a hypothetical.
    fb(out, Shape::Linear, &["filter-branch", "-d", "src", "--msg-filter", "cat", "HEAD"]);
    fb(out, Shape::Linear, &["filter-branch", "-f", "-d", ".git", "--msg-filter", "cat", "HEAD"]);

    // A rev-list argument that selects no commit: `die_with_status 2 "Found
    // nothing to rewrite"` (line 342). Exit **2** is a status no other
    // `filter-branch` path produces, and nothing in the corpus reached it.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--msg-filter", "cat", "HEAD..HEAD"]);

    // `--prune-empty` and `--commit-filter` are mutually exclusive (line 218),
    // decided in the option parser before anything is read.
    fb(
        out,
        Shape::Branched,
        &["filter-branch", "-f", "--prune-empty", "--commit-filter", "git commit-tree \"$@\"", "HEAD"],
    );

    // `--setup` runs once, before the loop, and its failure is its own die
    // (line 386) — distinct from a per-commit filter failing, which is below.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--setup", "false", "--msg-filter", "cat", "HEAD"]);

    // `--state-branch` pointed at a branch that has no `filter.map` blob in it:
    // "Unable to load state from <branch>:filter.map" (line 300).
    fb(out, Shape::Linear, &["filter-branch", "-f", "--state-branch", "main", "--msg-filter", "cat", "HEAD"]);

    // An unmerged index. `Shape::Dirty`'s refusal is `stateful_side_files.rs`'s
    // (and is stderr-strict there); this is the *other* way
    // `require_clean_work_tree` fails, which no case had.
    fb(out, Shape::Conflicted, &["filter-branch", "-f", "--msg-filter", "cat", "HEAD"]);

    // An option the parser does not know falls through to `usage` (line 205).
    fb(out, Shape::Linear, &["filter-branch", "-f", "--bogus-option", "HEAD"]);
}

/// `filter-branch` rewrites that touch exactly **one** commit.
///
/// [`Shape::Linear`] has a single commit, so the loop runs once and the one
/// progress line is emitted a few forks after the start timestamp is taken —
/// measured stable in 19 of 20 stock runs (module header). That is what makes
/// this the only place in the file where a real rewrite's *stdout* is asserted;
/// the state assertion (the rewritten commit id, and what is left in
/// `refs/original/*`) rides along and is exactly reproducible either way.
///
/// The filters chosen are the ones `history_rewrite.rs` does not run at all
/// (`--parent-filter`, `--commit-filter`, `--setup`, `--state-branch`) plus the
/// three flags whose whole effect is *where the output goes* (`--original`,
/// `-d`, `--state-branch`), which nothing measured on a shape small enough to
/// see the result.
///
/// Every command a filter runs is deterministic and clock-free: `cat`, `true`,
/// `false`, `printf`, `git rm --cached`, `git commit-tree`. Nothing here names a
/// temporary file or reads the wall clock, which is the rule that keeps a filter
/// from putting the nondeterminism the progress line already has into the commit
/// ids as well.
fn filter_branch_one_commit(out: &mut Vec<Case>) {
    // The identity filters no case had: a parent list piped through `cat`, and
    // the default commit filter spelled out.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--parent-filter", "cat", "HEAD"]);
    fb(out, Shape::Linear, &["filter-branch", "-f", "--commit-filter", "git commit-tree \"$@\"", "HEAD"]);
    // A parent filter that empties the list: on a root commit this is a no-op,
    // and proving it is a no-op is the point — an implementation that writes
    // the empty list back as a *change* produces a different id.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--parent-filter", "printf ''", "HEAD"]);

    // Where the backup goes. `refs/original/` is the default and
    // `misc_commands.rs` moves it to `refs/parity-backup` on `Branched`; this
    // is the same move on a shape whose whole ref set fits in the digest.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--original", "refs/backup", "--msg-filter", "cat", "HEAD"]);
    // Where the scratch tree goes. `-d` inside `.git` is a path git will not
    // otherwise create, and the trap on exit has to remove it again — a leftover
    // `.git/fbtmp` is a state difference.
    fb(out, Shape::Linear, &["filter-branch", "-f", "-d", ".git/fbtmp", "--msg-filter", "cat", "HEAD"]);
    // `--state-branch` writes a new commit holding `filter.map` on a branch that
    // did not exist, so the digest gains a ref, a tree and a blob.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--state-branch", "fbstate", "--msg-filter", "cat", "HEAD"]);
    fb(
        out,
        Shape::Linear,
        &["filter-branch", "-f", "--msg-filter", "cat", "--state-branch", "refs/heads/fbstate", "--", "--all"],
    );

    // An index filter that empties the tree. Without `--prune-empty` the result
    // is a commit with an empty tree; with it, `git_commit_non_empty_tree`
    // refuses to write one and the rewrite fails — two different outcomes from
    // one filter, which is the pair that shows `--prune-empty` is wired in.
    fb(
        out,
        Shape::Linear,
        &["filter-branch", "-f", "--index-filter", "git rm -r --cached --ignore-unmatch .", "HEAD"],
    );
    fb(
        out,
        Shape::Linear,
        &["filter-branch", "-f", "--prune-empty", "--index-filter", "git rm -r --cached --ignore-unmatch .", "HEAD"],
    );
    // `skip_commit` is the other shell function the script exports to a commit
    // filter; on a root commit it leaves nothing to map to.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--commit-filter", "skip_commit \"$@\"", "HEAD"]);

    // The committer half of `--env-filter`. `history_rewrite.rs` rewrites
    // `GIT_AUTHOR_NAME`; the committer is the half that also has to survive
    // `env::harden`'s pin, which is the only reason the resulting id is stable.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--env-filter", "export GIT_COMMITTER_NAME=other", "HEAD"]);

    // Rev-list arguments that are not a ref name: `--branches` as a selector,
    // and no argument at all (the script defaults to `HEAD`, line 269).
    fb(out, Shape::Linear, &["filter-branch", "-f", "--msg-filter", "cat", "--", "--branches"]);
    fb(out, Shape::Linear, &["filter-branch", "-f", "--msg-filter", "cat"]);

    // A tag-name filter that produces the empty string. On `Linear` there is no
    // tag to rename, so this asserts the filter is not run rather than that it
    // renames — the cheap half of a pair whose expensive half is below.
    fb(out, Shape::Linear, &["filter-branch", "-f", "--tag-name-filter", "printf \"\"", "--", "--all"]);

    // Filters that fail mid-loop. These *do* print the one progress line before
    // dying, which is why they are here rather than in the group above, and each
    // is a different die site: `msg filter failed` (line 472), `index filter
    // failed` (line 443).
    fb(out, Shape::Linear, &["filter-branch", "-f", "--msg-filter", "false", "HEAD"]);
    fb(out, Shape::Linear, &["filter-branch", "-f", "--index-filter", "false", "HEAD"]);

    // A detached HEAD. `Shape::Detached` has two commits rather than one, so
    // this one carries the module header's exclusion risk; it is kept because
    // "there is no branch to update" is a whole branch of the ref-writing code
    // (lines 505–535) that nothing else reaches.
    fb(out, Shape::Detached, &["filter-branch", "-f", "--msg-filter", "cat", "HEAD"]);
}

/// The two `filter-branch` cases that need more than one commit.
///
/// Both are expected to be excluded as `Verdict::Nondeterministic` on some
/// fraction of runs, for the reason the module header measures, and both are
/// kept anyway because the structure they rewrite exists in no smaller shape.
/// Two, not ten: each one is a coin the harness has to flip, and a group of
/// cases that mostly does not score is a group that mostly wastes a machine.
///
/// Measured on the run that introduced them: **both** landed in that bucket,
/// alongside seven of the eight pre-existing multi-commit rows from
/// `history_rewrite.rs`/`misc_commands.rs`/`stateful_side_files.rs`, while
/// **none** of the fifteen one-commit rows in [`filter_branch_one_commit`] did.
/// That is the whole argument for the split between the two groups, and it is a
/// measurement rather than a prediction.
fn filter_branch_structure(out: &mut Vec<Case>) {
    // A chain of annotated tags. `Shape::TagChain` has `inner` -> commit,
    // `outer` -> `inner`, `outermost` -> `outer`, plus `light-to-tag` pointing
    // at a tag object and `blobtag`/`treetag` pointing at non-commits. With
    // `--tag-name-filter cat` the script has to rewrite the commit, re-create
    // each tag object over the new commit, follow the nesting, and skip the two
    // that do not peel. `history_rewrite.rs` runs the same flag on `Branched`,
    // where the deepest tag is one level and nothing fails to peel.
    fb(out, Shape::TagChain, &["filter-branch", "-f", "--tag-name-filter", "cat", "--", "--all"]);

    // A gitlink through an identity rewrite. `--index-filter true` reads each
    // commit's tree into the index and writes it back unchanged, so the
    // submodule entry has to survive `read-tree`/`write-tree` as mode 160000 —
    // an entry no other `filter-branch` case in the corpus has ever passed
    // through the index.
    fb(out, Shape::Submodule, &["filter-branch", "-f", "--index-filter", "true", "HEAD"]);
}

// ---------------------------------------------------------------------------
// subtree
// ---------------------------------------------------------------------------

/// `subtree split --onto=`, where the port refuses and the script shrugs.
///
/// **This is the one case in the file where the port's rewrite produces a
/// different object store than stock's**, and the direction is the safe one —
/// the port writes nothing where stock writes two commits — but it is still a
/// divergence in what the repository contains afterwards. Reproduced by hand
/// against `Shape::Branched`, twice, plus a direct run of the binary under test:
///
/// ```text
/// $ git subtree split -P src --onto=nosuch
/// stock 2.55.0:
///   stdout: 5234dc349585e350808086147aeccff96360bbb3
///   stderr: fatal: ambiguous argument 'nosuch': unknown revision or path not in the working tree.
///           Use '--' to separate paths from revisions, like this:
///           'git <command> [<revision>...] -- [<file>...]'
///           1/       2 (0) [0]2/       2 (1) [0]
///   exit  : 0
///   store : + 5234dc349585e350808086147aeccff96360bbb3 commit 228
///           + 83c09930e05a9f0e62f4c26e8af26eeb9e8d21c3 commit 180
/// zvcs:
///   stdout: (empty)
///   stderr: (empty)
///   exit  : 128
///   store : unchanged
/// ```
///
/// `git-subtree` resolves `--onto` with a bare `git rev-parse` whose failure it
/// never checks, so the fatal goes to stderr, `$onto` ends up empty, and the
/// split proceeds as if `--onto` had not been given. The port treats the same
/// `rev-parse` failure as fatal. Exit code, stdout and post-state all move, so
/// three of the four surfaces the harness compares report it.
///
/// The second case reaches the identical seam from the other side: `blobtag` is
/// a ref that *exists* and does not peel to a commit, so `rev-parse` fails for a
/// different reason and the outcome is the same. Keeping both is what
/// distinguishes "the port validates `--onto` at all" from "the port validates
/// it the way `rev-parse` does".
///
/// The three neighbours pin the boundary: an `--onto` that resolves to a commit
/// on the same history, one that resolves to an annotated tag, and one that
/// resolves into an unrelated root. All three agree today, which is what makes
/// the two above a defect in the *failure* path specifically rather than in
/// `--onto`.
fn subtree_split_onto(out: &mut Vec<Case>) {
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--onto=nosuch"]);
    st(out, Shape::TagChain, &["subtree", "split", "-P", "src", "--onto=blobtag"]);

    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--onto=v0.2.0"]);
    st(out, Shape::Merged, &["subtree", "split", "-P", "src", "--onto=side"]);
    st(out, Shape::Unrelated, &["subtree", "split", "-P", "src", "--onto=alien"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--onto=main", "--branch=srcbr"]);
}

/// `subtree`'s argument gates, and the two subcommands that need no clean tree.
///
/// `ensure_clean` makes `add`/`merge`/`pull`/`--rejoin` unreachable past their
/// first few lines (module header), so the useful question about those verbs is
/// *which check fires first*: a missing operand and a surplus one are decided by
/// the argument parser before the tree is ever inspected, and a port that runs
/// the clean check first answers a different message with a different status.
/// `history_rewrite.rs` pins the clean-tree refusal itself; this pins the order.
///
/// `split` and `push` need no clean tree, so the rest of the group is real
/// behaviour: a prefix with a trailing slash, a prefix that names a file rather
/// than a directory, an empty prefix, `--branch` at a full refname and at a name
/// git will not accept, `--rejoin` combined with `--ignore-joins`, `-q`, and the
/// `-d` debug stream (stderr only, never compared — see the module header).
fn subtree_argument_gates(out: &mut Vec<Case>) {

    // Parser before `ensure_clean`.
    st(out, Shape::Branched, &["subtree", "add", "--prefix=vendor"]);
    st(out, Shape::Branched, &["subtree", "add", "-P", "vendor", "feature", "extra"]);
    // Past the parser, into the clean-tree wall, on the three verbs
    // `history_rewrite.rs` does not spell with `--squash` or a local path.
    st(out, Shape::Branched, &["subtree", "merge", "-P", "src", "--squash", "feature"]);
    st(out, Shape::Branched, &["subtree", "pull", "-P", "src", "--squash", ".", "feature"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--rejoin", "--ignore-joins"]);

    // Prefix spellings.
    st(out, Shape::Branched, &["subtree", "split", "-P", "src/"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "README.md"]);
    st(out, Shape::Branched, &["subtree", "split", "--prefix=", "HEAD"]);

    // Destination spellings.
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--branch=refs/heads/srcbr"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--branch=bad name"]);
    st(out, Shape::Merged, &["subtree", "split", "-P", "src", "--branch=sb"]);

    // A rev argument that does not resolve — the same `rev-parse` the `--onto`
    // group is about, reached as a positional instead of as an option value.
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "nosuch"]);

    // Modes and shapes `history_rewrite.rs` does not run `split` on.
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "--ignore-joins"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "-q"]);
    st(out, Shape::Branched, &["subtree", "split", "-P", "src", "-d"]);
    st(out, Shape::Octopus, &["subtree", "split", "-P", "src"]);
    st(out, Shape::TagChain, &["subtree", "split", "-P", "src"]);
    st(out, Shape::NotesReplace, &["subtree", "split", "-P", "src"]);

    // `push` to a local path, which needs no clean tree and writes a ref in the
    // repository it is pushing to — here, itself.
    st(out, Shape::Branched, &["subtree", "push", "-P", "src", ".", "refs/heads/pushed"]);
}

// ---------------------------------------------------------------------------
// replace
// ---------------------------------------------------------------------------

/// `replace` run against a repository that is **already replaced**.
///
/// [`Shape::NotesReplace`] carries two substitutions — a commit replaced by a
/// commit with the same tree and a different message, and a blob replaced by
/// another blob. `fixture_gaps2.rs` lists them; `stateful_side_files.rs` and
/// `history_rewrite.rs` write replacements but only ever into repositories that
/// have none. The write-over-a-replacement cross is what this group is, and it
/// is where the port's one wrong rewrite lives.
///
/// **The divergence: `replace --graft` reads through the replacement.**
/// `builtin/replace.c` disables replacement lookup for the whole of
/// `cmd_replace`, so `--graft` copies the **original** object. The port copies
/// the object the replacement points at. Hand-verified on `Shape::NotesReplace`
/// with `git replace -f --graft HEAD~2`, where `HEAD~2` is
/// `0dc1e64f34767c0cd0f35ad39a53bb0ad697ae04`:
///
/// ```text
/// stock 2.55.0 -> refs/replace/0dc1e64… = c3ab96d6c2d176dac8e31fc446fd3b5e5d4b1a33
///   tree 66cc02ca0c1a09871f1405751c7132992da5c715
///   author zvcs parity <parity@example.invalid> 1700000000 +0000
///   committer zvcs parity <parity@example.invalid> 1700000000 +0000
///
///   notes: commit 1
///
/// zvcs         -> refs/replace/0dc1e64… = 199a4423094231e95d265ec9c675f247208af0d5
///   tree 66cc02ca0c1a09871f1405751c7132992da5c715
///   author zvcs parity <parity@example.invalid> 1700000000 +0000
///   committer zvcs parity <parity@example.invalid> 1700000000 +0000
///
///   notes: replacement for commit 1
/// ```
///
/// stdout, stderr and exit code all agree; only the state digest sees it (`commit
/// 188` against `commit 204` in the `cat-file --batch-all-objects` listing).
/// That is the worst shape a rewrite defect can take — the command reports
/// success and the repository is wrong — and it compounds: each re-graft would
/// fold the previous replacement's message in again.
///
/// The `--edit` neighbour is here to show the defect is `--graft`-specific
/// rather than a whole-command replacement-lookup bug: with `GIT_EDITOR=true`
/// the editor writes nothing back, and both sides refuse with "new object is the
/// same as the old one", which they could not both do if one had opened the
/// original and the other the replacement.
fn replace_over_replaced(out: &mut Vec<Case>) {

    // The divergence, and the same divergence with the parent spelled out.
    repl(out, &["replace", "-f", "--graft", "HEAD~2"]);
    repl(out, &["replace", "-f", "--graft", "HEAD~2", "HEAD~2"]);

    // The neighbour that agrees, which is what makes the pair a finding.
    repl(out, &["replace", "-f", "--edit", "HEAD~2"]);
    repl(out, &["replace", "--edit", "HEAD"]);

    // Deleting an existing replacement by naming the *replaced* object through a
    // revision rather than a literal id. Nothing had removed a replacement the
    // fixture shipped; `stateful_side_files.rs`'s `-d` cases name refs that do
    // not exist.
    repl(out, &["replace", "-d", "HEAD~2"]);

    // Listing with a pattern, against a namespace that is not empty. The
    // existing pattern case (`replace -l main`) runs where the answer is empty
    // either way, so a port that ignores the pattern scores the same.
    repl(out, &["replace", "-l", "refs/replace/*"]);
    repl(out, &["replace", "-l", "HEAD~2"]);

    // `--raw` outside `--edit` is rejected by the parser (exit 129), which is a
    // different refusal from `--format=bogus`.
    repl(out, &["replace", "--raw", "--list"]);

    // Conversion with no `.git/info/grafts` present — the only reachable half,
    // for the reason the module header records.
    repl(out, &["replace", "--convert-graft-file"]);

    // Turning the substitution off, in both spellings, while asking the command
    // that manages it. `env_layer.rs` moves `GIT_REPLACE_REF_BASE` for `log`;
    // neither the global flag nor the config key had ever been aimed at
    // `replace` itself.
    out.push(
        Case::new("replace", &["replace", "-l"], Shape::NotesReplace)
            .with_globals(&[&["--no-replace-objects"]]),
    );
    out.push(
        Case::new("replace", &["replace", "-l"], Shape::NotesReplace)
            .with_config(&[("core.useReplaceRefs", "false")]),
    );

    // Replacing an object with one of a different type. `blobtag` peels to a
    // blob and `treetag` to a tree, so these are commit->blob and commit->tree;
    // git accepts a type change only under `-f`, and the pair is the two answers.
    out.push(Case::new("replace", &["replace", "HEAD", "blobtag"], Shape::TagChain));
    out.push(Case::new("replace", &["replace", "-f", "HEAD", "treetag"], Shape::TagChain));
}

// ---------------------------------------------------------------------------
// request-pull
// ---------------------------------------------------------------------------

/// `request-pull`, on the two inputs its existing corpus never hands it: a
/// **remote name** where a URL goes, and a **replaced** history.
///
/// `git-request-pull` resolves its second argument with `git ls-remote
/// --get-url "$url"`, so a configured remote name and a path are two different
/// code paths that produce the same string, and every existing case (thirty-odd
/// across three modules) passes a path — `.`, `./.remote.git`, or a name that
/// resolves to neither. [`Shape::BehindRemote`] has `remote.origin.url` set to
/// the relative `./.remote.git`, so naming `origin` exercises the resolution
/// without putting an absolute path into a case.
///
/// The output is checked for wall-clock and absolute-path content and has
/// neither: the dates it prints are the commits' own, which `env::harden` pins
/// to `1700000000 +0000`, and the URL it echoes is the relative string the case
/// supplied. Verified on `Shape::Packed`, whose nine-commit history is the
/// deepest range any `request-pull` case walks:
///
/// ```text
/// The following changes since commit 342199d662e564993f93c075543367b420aa4353:
///
///   packed: revision 4 (2023-11-14 22:13:20 +0000)
///
/// are available in the Git repository at:
///
///   .
/// ```
fn request_pull_resolution(out: &mut Vec<Case>) {

    // A remote name rather than a URL, in both the two- and three-argument
    // forms, and once with the patch.
    rpull(out, Shape::BehindRemote, &["request-pull", "HEAD~1", "origin"]);
    rpull(out, Shape::BehindRemote, &["request-pull", "HEAD~1", "origin", "main"]);
    rpull(out, Shape::BehindRemote, &["request-pull", "-p", "HEAD~1", "origin", "main"]);

    // A replaced history. The summary it prints comes from a `log` walk, so a
    // port that does not consult `refs/replace/*` prints a different subject for
    // the replaced commit — and the `--no-replace-objects` twin is the control.
    rpull(out, Shape::NotesReplace, &["request-pull", "HEAD~2", "."]);
    rpull(out, Shape::NotesReplace, &["request-pull", "HEAD~2", ".", "refs/heads/main"]);
    out.push(
        Case::new("request-pull", &["request-pull", "HEAD~2", "."], Shape::NotesReplace)
            .with_globals(&[&["--no-replace-objects"]]),
    );

    // Two shapes no `request-pull` case has used: a nine-commit linear history,
    // and one where the two branches share a patch but not a commit id.
    rpull(out, Shape::Packed, &["request-pull", "HEAD~3", "."]);
    rpull(out, Shape::Cherry, &["request-pull", "main", ".", "topic"]);
}

// ---------------------------------------------------------------------------
// shortlog / whatchanged
// ---------------------------------------------------------------------------

/// `shortlog` and `whatchanged` summarising a history that is **substituted** as
/// it is read.
///
/// Both verbs are covered exhaustively elsewhere on the ordinary shapes
/// (`diff_family.rs`, `history_query.rs`, `history_simplification.rs`), so the
/// only thing worth adding is the axis those files cannot reach: on
/// [`Shape::NotesReplace`] a commit and a blob are replaced at read time, which
/// changes the author tally `shortlog` groups, the subject it prints under
/// `--format=`, and the diff `whatchanged` renders — while every object id in
/// the repository stays the same. A port that writes `refs/replace/*` correctly
/// and never consults it when walking (the defect `Shape::NotesReplace` was
/// built for) answers these differently from stock and identically to stock's
/// `--no-replace-objects`, which is exactly why each is paired with its
/// substitution-off twin.
///
/// `whatchanged` is spelled with `--i-still-use-this` throughout: stock 2.55.0
/// refuses without it (`fatal: refusing to run without --i-still-use-this`), and
/// `diff_family.rs` already owns the refusal itself.
fn summaries_over_replaced(out: &mut Vec<Case>) {

    nr(out, "shortlog", &["shortlog", "-s", "-n", "HEAD"]);
    nr(out, "shortlog", &["shortlog", "-s", "-e", "--group=committer", "HEAD"]);
    nr(out, "shortlog", &["shortlog", "--group=format:%H", "-s", "HEAD"]);
    nr(out, "shortlog", &["shortlog", "--group=trailer:Signed-off-by", "-s", "HEAD"]);
    nr(out, "shortlog", &["shortlog", "-n", "-w40,3,5", "HEAD"]);
    // `--all` reaches `refs/notes/*` and `refs/replace/*` as well as the
    // branches, so the tally includes the notes commits git wrote itself; the
    // explicit triple is the same walk with those two namespaces excluded, and
    // the difference between the two answers is the whole point of the pair.
    nr(out, "shortlog", &["shortlog", "-s", "--all"]);
    nr(out, "shortlog", &["shortlog", "-s", "--branches", "--tags", "--remotes"]);

    nr(out, "whatchanged", &["whatchanged", "--i-still-use-this", "--oneline", "HEAD"]);
    nr(out, "whatchanged", &["whatchanged", "--i-still-use-this", "-p", "HEAD"]);
    nr(out, "whatchanged", &["whatchanged", "--i-still-use-this", "--raw", "--all"]);

    // The substitution-off twins, in both spellings.
    for args in [
        &["shortlog", "-s", "HEAD"][..],
        &["whatchanged", "--i-still-use-this", "--oneline", "HEAD"],
    ] {
        out.push(
            Case::new(args[0], args, Shape::NotesReplace)
                .with_globals(&[&["--no-replace-objects"]]),
        );
    }
    out.push(
        Case::new("shortlog", &["shortlog", "-s", "HEAD"], Shape::NotesReplace)
            .with_config(&[("core.useReplaceRefs", "false")]),
    );

    // The nested-tag shape, whose `--all` reaches five tag objects and two that
    // do not peel to a commit — a ref set `shortlog --all` has never walked.
    out.push(Case::new("shortlog", &["shortlog", "-s", "--all"], Shape::TagChain));
}
