//! Differential corpus cases for the merge **engine** as `git merge` drives it:
//! the strategy the default backend is reached by (`-s ort`, which 395 existing
//! `merge` cases spell exactly once), the `-X` grammar, the message machinery
//! that turns a finished merge into a commit or a `MERGE_MSG`, and the option
//! table `builtin/merge.c` parses before any of that runs.
//!
//! # How this divides the merge surface with the modules already here
//!
//! Eight modules touch a merge. Each of the sentences below was checked against
//! the module named, and every case in this file sits outside all of them.
//!
//! * [`super::merge_strategies`] — the nearest neighbour, and the one to read
//!   first. It owns *which program the trees are handed to*: `-s recursive`,
//!   `-s resolve`, `-s subtree`, `-s ours`, `-s octopus`, the `-s a -s b` retry
//!   loop, the six backend binaries invoked directly (`merge-recursive`,
//!   `merge-recursive-ours/-theirs`, `merge-subtree`, `merge-resolve`,
//!   `merge-ours`, `merge-octopus`), and the `-X` set as it stands on
//!   [`Shape::CrissCross`]. What it does **not** contain is `-s ort` — the
//!   default since 2.34 and the strategy every one of its cases actually runs
//!   under — anywhere except one bare `merge -s ort cc-right`. The `-s ort`
//!   sweep below completes its five-strategy grid to six on every shape it uses,
//!   which is what makes "does the port alias `ort` to `recursive`, and does it
//!   alias either to `resolve`" answerable rather than assumed.
//! * [`super::merge_family`] owns the *bytes* a three-way text merge produces —
//!   `merge-file`, `merge-index`, `merge-one-file`, `mergetool`, and the ll-merge
//!   driver — plus `merge --abort`/`--continue` on [`Shape::Conflicted`].
//! * [`super::merge_dirty`] owns the dirty-worktree gates on
//!   [`Shape::MergeableDirty`]/[`Shape::MergeableStaged`]: which of index-vs-HEAD
//!   and this-path-on-the-way-past refused, and in whose words.
//! * [`super::patch_equivalence`] owns `merge-tree` in both its modes, and the
//!   patch-identity questions (`cherry`, `patch-id`, `range-diff`) that ask
//!   whether two commits carry the same change. Nothing here runs `merge-tree`:
//!   the split is *engine driven from a worktree* here, *engine driven from bare
//!   trees* there, and the two write to different places (an index and a
//!   worktree versus stdout).
//! * [`super::rebase_engine`] owns the same engine reached through
//!   `rebase`/`cherry-pick`/`revert`; [`super::sequences`] owns everything
//!   needing a second invocation (`--continue`, `--abort`, resolve-then-commit);
//!   [`super::rerere_engine`] owns `rerere.*` over a merge; and
//!   [`super::attributes_filters`] owns `merge.<driver>.driver` and the
//!   `.gitattributes` `merge=`/`conflict-marker-size` rules.
//!
//! # What is here that is in none of them
//!
//! Four surfaces, none of which any existing `merge` case reaches. Measured by
//! listing every `merge` invocation in the corpus (`--list-cases`, 395 of them)
//! and grepping it for each token:
//!
//! 1. **`-s ort` and `--strategy=`** — one occurrence between them, and the long
//!    `--strategy=`/`--strategy-option=` spellings zero.
//! 2. **The merge-message machinery** — `--log`, `--cleanup=`, `--signoff`,
//!    `-F <path>`, `--into-name=`, `-m` twice: zero occurrences each. `merge.log`
//!    was set from config, so the *flag's* own parsing and its `=<n>` form had
//!    never run, and `--cleanup` had never run at all in any mode.
//! 3. **The report knobs** — `-n`, `--stat`, `--no-stat`, `--summary`,
//!    `--compact-summary`, `-e`, `--verbose`, `--progress`, `--autostash`,
//!    `--overwrite-ignore`, `--verify-signatures`: zero occurrences each.
//! 4. **The `--no-` half of the option table.** `parse_options` generates a
//!    negation for all but a handful of `merge`'s options, and a port that
//!    hand-rolls the parser has to enumerate them; the ones it forgets fall
//!    through to `git merge`'s "treat it as a rev" branch and the merge silently
//!    does not happen. Five of them do exactly that in the port under test.
//!
//! Plus two dimensions no `merge` case in the corpus carries at all: a working
//! directory below the root ([`Case::in_dir`], used by four cases here so the
//! conflict paths a merge prints are asked for from a subdirectory), and
//! `merge-recursive`'s own long-option spellings on a **single**-base history,
//! where the result is a plain three-way text merge rather than the virtual base
//! [`super::merge_strategies`] measures them against.
//!
//! # Which conflict types are reachable at all, and which are not
//!
//! The brief for this module named thirteen conflict types. Most of them cannot
//! be produced by any invocation against any fixture in this corpus, and saying
//! which is more useful than writing cases that measure something easier and
//! calling them conflict coverage. Measured over every commit reachable from
//! every ref of all 44 shapes (`ls-tree -r` per commit, `log --all
//! --no-renames --diff-filter=D`):
//!
//! * **Reachable.** *Content* conflict — [`Shape::CrissCross`]'s `clash.txt`,
//!   the only path in the corpus two branches edit differently. *Add/add with no
//!   merge base* — [`Shape::Unrelated`]'s `README.md` across two orphan roots.
//!   A *type change applied cleanly* — [`Shape::Symlinks`]' `dir/target.txt`,
//!   a regular file on `main` and a symlink on `sym-pending`.
//! * **Unreachable, and why.** There are **two deletions in the whole corpus**
//!   (`orig/alpha.txt` and `orig/beta.txt` on [`Shape::Renamed`]), both halves of
//!   a rename, both in a strictly linear history — so *modify/delete*,
//!   *delete/modify*, *rename/delete* and *rename/add* have no second line of
//!   development to fall on. Both renames are the same rename on every branch
//!   that has them, so *rename/rename* has no divergent target. `orig/` keeps two
//!   of its four files, so no *directory rename* is even a candidate. **No tree
//!   in any shape contains a `100755` blob**, so a *mode-only* conflict cannot be
//!   expressed. `dir/target.txt` is the *only* path anywhere whose entry mode
//!   changes between two commits, and only one side changes it, so a
//!   *symlink/file* conflict has no opposing edit; no path is a blob in one
//!   commit and a tree in another, so *file becomes directory* has none either.
//!   The two `160000` gitlinks ([`Shape::Submodule`], [`Shape::NestedSubmodule`])
//!   each exist on one branch of a one-branch repository, so a *gitlink*
//!   conflict has nothing to disagree with. `app/data.bin` has exactly two
//!   revisions on one line of history, so a *binary* conflict would need a third.
//!   Every one of these needs a fixture shape, and a corpus module cannot add
//!   one.
//!
//! So the conflict *types* below are the three reachable ones, and the value is
//! in what is asked **about** them: what the engine leaves behind (`MERGE_MSG`
//! under each `--cleanup` mode, `SQUASH_MSG` when the squashed range contains a
//! merge commit, `AUTO_MERGE`, the stages) rather than which category the
//! conflict falls in.
//!
//! # Determinism
//!
//! Twenty-nine of these cases end in a commit, so their object ids are part of
//! what is compared. Every one was run **twice against stock 2.55.0** in two
//! fresh copies of its shape under [`crate::env::harden`], and the two runs
//! compared on refs, `HEAD`, the full object list, the index, the operation-state
//! files and `log -1 --format=%B%n%T%n%P`; all agreed. The copies were made with
//! `cp -Rp` on purpose — [`crate::fixture::copy_tree`] carries mtimes across and
//! the shapes set `core.checkStat=minimal`, and a copy that drops the timestamps
//! produces a stat-dirty index that makes `builtin/merge.c`'s trivial in-index
//! path fail. Under a timestamp-dropping copy the port and stock disagree on
//! `patches merge -s resolve`; under the harness's own copy they agree, so that
//! difference is an artefact of the copy and is deliberately **not** a case here.
//!
//! `GIT_EDITOR` is pinned to `true` by [`crate::env::harden`], so `-e` commits
//! git's own default message unedited, and `--cleanup=verbatim` is what makes
//! the exact bytes of that message observable — see [`message_machinery`].

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ort_the_default_strategy(out);
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

/// The default backend, spelled, on every shape [`super::merge_strategies`]
/// runs the other five strategies against.
///
/// The point is not that `-s ort` works — it is the default, so every unadorned
/// `merge` in the corpus already exercises the backend. The point is the
/// **name**: `cmd_merge` looks a strategy up in `builtin/merge.c`'s table, and a
/// port that resolves `ort` to a different entry than the empty default, or that
/// aliases `ort` and `recursive` to two different implementations, is invisible
/// until the name is written down. Pairing each `-s ort` here with the
/// `-s recursive` on the same shape and rev in `merge_strategies` is what turns
/// "are these the same backend" into a comparison rather than a claim.
///
/// Measured on stock 2.55.0, and the reason the grid is worth completing: on
/// [`Shape::CrissCross`] `ort`, `recursive` and `subtree` all leave the same
/// index (`ls-files --stage` digest `34dc9ed69178`, `clash.txt` at stages 1/2/3),
/// `resolve` leaves a *different* one (`ac4bf0d87546` — no stage 1, because
/// `read-tree -m` resolved against one base rather than a virtual one), `ours`
/// leaves `HEAD`'s tree untouched, and `octopus` refuses. So the six names are
/// four distinct behaviours on stock, and a port that collapsed any two of the
/// four would be caught.
///
/// `-s ort` over more than two heads is here strict: `merge-ort` is a two-head
/// engine and refuses with `error: Not handling anything other than two heads
/// merge.` followed by `Merge with strategy ort failed.` — a different refusal
/// from `git-merge-octopus`'s, from a different program, and the corpus had
/// neither for `ort`.
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
            // The retry loop with `ort` on each side of it. The two orders
            // disagree on stdout: `resolve` first fails through
            // `git-merge-one-file` and only then is `ort` tried.
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
/// three apart only by having been asked. Measured: `find-renames` and `subtree`
/// exit 1 with the ordinary conflict, `rename-threshold` exits 128 — so the
/// three are separated by exit code and the last is strict.
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
            // An algorithm name that does not exist: `fatal`, exit 128, before
            // any tree is touched.
            &["merge", "-X", "diff-algorithm=nonsense", "cc-right"],
            // A threshold option with no value at all.
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
/// object id for the same tree and the same parents. That is why this case is
/// here rather than only the strip modes.
///
/// `--cleanup=scissors` on a *conflicted* merge is the other half: git writes
/// the `# ------------------------ >8 ------------------------` block into
/// `MERGE_MSG` above the `# Conflicts:` list, and the block is what tells the
/// editor where the message ends. Both a criss-cross content conflict and a
/// baseless add/add are here because the two write `MERGE_MSG` from different
/// code paths.
///
/// `-F no-such-file` is strict: stock exits **129** with
/// `error: could not read file 'no-such-file'` and the usage block, which is
/// `parse_options`'s own failure rather than a `die()`, and 129-versus-128 is
/// exactly the distinction a hand-rolled parser loses.
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
            // A fast-forwardable merge under scissors, so the block is written
            // into a message that then becomes a commit.
            &["merge", "--cleanup=scissors", "--no-ff", "-m", "x", "cc-a"],
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
/// Every one of these had zero occurrences across the corpus's 395 `merge`
/// invocations. Most cannot change this merge's *result*, and that is the
/// contract being pinned: the option parses, reaches the right field, and
/// leaves the tree alone. `--stat`/`--summary`/`-n` are three names for two
/// settings of one field and `--compact-summary` is a fourth rendering of it;
/// an implementation that maps `--summary` to the wrong one prints a different
/// stdout for the same merge.
///
/// `-e` is worth its line because [`crate::env::harden`] pins `GIT_EDITOR` to
/// `true`: the editor is spawned, exits 0 without touching the file, and the
/// generated message is committed unchanged. So `-e` and `--no-edit` must
/// produce the *same commit*, and a port that only implements one of them —
/// or that skips the spawn and takes a different message path — diverges on the
/// object id rather than on stdout.
///
/// `--verify-signatures` is strict: no commit in any shape is signed, so stock
/// dies at 128 naming the unsigned commit, and the refusal is the whole
/// behaviour of the flag on this corpus.
///
/// [`Shape::Patches`] appears here for one reason: `app/data.bin` is a binary
/// blob, and `Bin 1024 -> 1024 bytes` is a diffstat row no other merge in the
/// corpus can produce.
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
/// One option that is genuinely *not* negatable is here too, and strict, so the
/// set is measured from both sides: `--no-file` exits 129 with the usage block,
/// because `-F` is `PARSE_OPT_NONEG`. (`--no-ff-only`, the other one, is already
/// in [`super::merge_family`] and is not repeated.)
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
/// `merge.verbosity` was measured at 0 and 5, the two ends; the four levels
/// between them each gate a different set of lines in `merge-ort` and
/// `builtin/merge.c` and were unmeasured. `merge.directoryRenames` was measured
/// at `conflict` only, `merge.renames` at `false` only, and
/// `merge.renameLimit` not at all — the last two matter here even though
/// [`Shape::CrissCross`] has no rename for them to find, because what is being
/// pinned is that the key is *read and validated*, not that detection changes.
///
/// **`merge.renameLimit=nonsense` is the case that matters most in this
/// function, and it is strict.** It is not a rename question at all: it is
/// `git_config_int()` refusing a non-numeric value. Stock 2.55.0 and git 2.50.1
/// both die with `fatal: bad numeric config value 'nonsense' for
/// 'merge.renamelimit': invalid unit` at exit 128 **before touching anything**.
/// A port that shrugs the value off does not merely print differently — it
/// performs the merge, and on the clean `alien` merge below it *creates a commit*
/// where both gits created nothing. Both a conflicting and a committing merge
/// are here for exactly that reason: the failure is only visible as a written
/// object on the second.
///
/// `merge.autoStash=true` over a **clean** worktree is the other one worth
/// naming. No stash is created (verified: `stash list` is empty afterwards on
/// both gits), and both gits still print `When finished, apply stashed changes
/// with \`git stash pop\`` when the merge fails — the hint is emitted from the
/// failure path unconditionally, not from the stash. A port that prints it only
/// when it actually stashed omits a line.
///
/// `merge.ff=only` over a merge that cannot fast-forward is strict: the refusal
/// is the whole meaning of the value, and it comes from a different place than
/// `--ff-only`'s.
fn merge_config_values(out: &mut Vec<Case>) {
    for level in ["1", "2", "3", "4"] {
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
    // The width every object id in the diffstat prints at, over a merge.
    out.push(
        Case::new("merge", &["merge", "cc-right"], Shape::CrissCross)
            .with_config(&[("core.abbrev", "12")]),
    );
    out.push(
        Case::new("merge", &["merge", "--stat", "--no-ff", "-m", "x", "cc-a"], Shape::CrissCross)
            .with_config(&[("diff.statGraphWidth", "10")]),
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
/// `criss-cross: cc-right merge`, and the second is a two-parent commit. Stock
/// 2.55.0 writes both into `SQUASH_MSG`; a port that filters merges out of the
/// walk writes one, and the difference is invisible in stdout — `--squash` prints
/// only `Squash commit -- not updating HEAD` and the conflict lines.
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
/// No `merge` case in the corpus carries [`Case::in_dir`]. The engine prints
/// every path it touches — `Auto-merging <path>`, `CONFLICT (content): Merge
/// conflict in <path>`, the diffstat rows, and the `# Conflicts:` list in
/// `MERGE_MSG` — and git prints all of them **relative to the worktree root**
/// regardless of where the command was run. An implementation that renders any
/// of them relative to the current directory produces `../README.md` for a
/// conflict two levels up, and nothing in the corpus could see it because every
/// merge ran from the root.
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
/// [`super::merge_strategies`] runs `merge-recursive` only on
/// [`Shape::CrissCross`], where two explicit bases are given and the backend has
/// to build a virtual one first. That is the harder path and the right one to
/// have, but it means every option there is measured through a recursion, and
/// its own header records that the shape cannot separate rename thresholds at
/// all. [`Shape::Cherry`] is the complement: `topic` and `main` have exactly one
/// merge base (`cherry: seed`), `app.txt` is edited on both sides — the same
/// hunk on one line, different hunks on two others — and the result is an
/// ordinary three-way text merge. So these cases measure the option's effect on
/// `ll_merge` rather than on base construction, and `merge-recursive`'s `<head>`
/// is `topic`, which is what `Shape::Cherry` has checked out (the backend writes
/// through `unpack_trees` and fails the up-to-date check against any other).
///
/// Six of the option names below appear nowhere in the corpus in any spelling:
/// `--ignore-space-change`, `--ignore-all-space`, `--ignore-space-at-eol`,
/// `--ignore-cr-at-eol`, `--renormalize`/`--no-renormalize`, and
/// `--rename-threshold=`/`--subtree=` — `merge_strategies` reaches
/// `--ours`, `--theirs`, `--patience`, `--diff-algorithm=histogram` and
/// `--no-renames` and stops there.
///
/// The one case that is not on `Cherry` is the one that cannot be:
/// `--subtree=<path>` **with two explicit merge bases**. The shift has to be
/// threaded through the recursion that builds the virtual base, and that is a
/// distinct code path from either `-X subtree=` on a two-base merge (which
/// `merge` reaches through its own base computation and which the port handles)
/// or `--subtree=` on a one-base merge. Both gits merge and conflict at exit 1;
/// the port under test refuses at 128 with a sentence naming the recursion it
/// cannot thread the shift through. Strict, because that sentence is the finding.
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
