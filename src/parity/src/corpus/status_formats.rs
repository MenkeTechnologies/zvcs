//! `git status` as the **subject**: the three output formats, the axes that
//! change what each one says, and the `status.*`/`advice.*` keys that change it
//! from configuration.
//!
//! `status` appears in twenty-one modules and is owned by none of them. Every
//! one of those modules runs it as a *probe* of something else — did the sparse
//! cone take, did the clean filter run, did discovery find the right repository
//! — so the thing being measured is the fact status reports and never the
//! rendering it reports it in. That leaves a large, documented, machine-read
//! surface with no owner: `--porcelain=v2` is a parseable contract that editor
//! and shell integrations consume field by field, and a port can get a field
//! wrong there while the long format a human reads stays perfect.
//!
//! # How this divides territory with the modules that already run `status`
//!
//! Read directly before writing a case here (`grep -n '"status"'` over each,
//! plus the surrounding group and its doc comment):
//!
//! * **`index_plumbing.rs`** — the corpus's one systematic `--porcelain=v2`
//!   sweep, and by far the largest owner. It holds `--porcelain=v2` with
//!   `--branch`, `-z`, `-uno`, `-uall`, `--no-ahead-behind`/`--ahead-behind`,
//!   `--show-stash`/`--no-show-stash`, `--ignored=matching|traditional|no`,
//!   `--ignore-submodules=none|all`, `--renames`/`--no-renames`/
//!   `--find-renames=25%`, and the `status.aheadBehind`/`showStash`/`renames`
//!   keys — all of it in the **`--porcelain=v2` spelling**. Nothing here repeats
//!   a v2 argv it already has: this file reaches v2 through the shapes it never
//!   ran on, through the `-b` short spelling, and through the *comparison*
//!   between v2 and the other two formats on one state.
//! * **`fixture_gaps2.rs`** — the reachability sweep for `IntentToAdd`,
//!   `PendingRename`, `Rerere`, `HooksFail`, `WorktreeLocked`, `Shallow`,
//!   `Promisor`. It owns the `--find-renames=30|50|60|70|90` threshold ladder on
//!   `PendingRename` and the `status.renames=false|copies` /
//!   `diff.renameLimit=1` keys there. This file takes the spellings that ladder
//!   never used — `-M`, `-M<n>`, the `<n>%` forms, `--find-renames` bare — and
//!   the flag-beats-config direction the keys were never crossed with.
//! * **`gitignore_precedence.rs`** — `--ignored` as an *ignore-rule* question:
//!   `--ignored=traditional|no|matching` against `core.excludesFile`,
//!   `.gitignore` precedence, pathspec-limited ignore listings, and the
//!   `--ignored=matching -uno` refusal. It owns that refusal and every
//!   `core.excludesFile` case. This file touches `--ignored` only where the
//!   *untracked* mode is the variable (`-unormal`, bare `-u`, `-uall` crossed
//!   with `matching`), which is the one axis it does not vary.
//! * **`config_reads.rs`** — `status.showUntrackedFiles`, `status.showStash`,
//!   `status.aheadBehind`, `status.short` and the `color.status.*` slots, each
//!   delivered from a named scope, with a bare `status` (or `status --short`)
//!   argv. Its own doc comment retires `status.relativePaths`, `status.branch`,
//!   `status.submoduleSummary` and `status.displayCommentPrefix` as unreachable
//!   there. Those four are this file's, and the first of them is its headline.
//! * **`shape_reach.rs`** — one status invocation per newly-added shape so the
//!   shape is not unmeasured; `--porcelain`/`--short` with `--ignored` and
//!   `-uall` on `Attributes`, `Sparse`, `DecomposedPaths`. Breadth of shape, not
//!   depth of format.
//! * **`sequences.rs`** — 235 identical `status --porcelain` steps used to
//!   verify what the *previous* step did. Never the subject, never varied.
//! * **`sparse_family.rs`** — `status` as the instrument that says whether a
//!   sparse-excluded path is reported as deleted. Owns the sparse-config cases.
//! * **`submodule_family.rs`** — `git submodule status` (a different
//!   subcommand) plus three top-level `status --porcelain` cases used as
//!   discovery probes under `-C`/`GIT_DIR`/`GIT_WORK_TREE`.
//! * **`merge_dirty.rs`, `worktree_index.rs`, `add_rm_mv_clean.rs`,
//!   `stash_deep.rs`** — named as likely owners and, read, own **nothing**:
//!   `status` appears in their prose only. `add_rm_mv_clean.rs` says
//!   `status --porcelain=v1 -uall` is its instrument for `clean`, and reaches it
//!   through the state digest rather than through a case.
//!
//! Also read for overlap, since each holds a handful: `fixture_gaps.rs`,
//! `fixture_gaps3.rs`, `env_layer.rs`, `globals_layer.rs`, `pathspec_stdin.rs`,
//! `discovery.rs`, `attributes_filters.rs`, `misc_commands.rs`,
//! `informational.rs`, `exit_codes.rs`.
//!
//! # What is new here, in one list
//!
//! Every one of these is used by no case in the corpus before this file:
//! `-s`, `-b`, `-sb` (the short spellings — only `--short`/`--branch` existed),
//! `-unormal`/`--untracked-files=normal`, bare `-u`, `--no-untracked-files`,
//! `--no-short`, `--no-long`, `--no-porcelain`, `--column` in every style,
//! `-v`/`--verbose`/`-vv`, `-M`/`-M<n>`/`<n>%` rename spellings, bare
//! `--ignore-submodules`, the format-selection *conflicts* (`--long` with `-z`,
//! `--porcelain` with `--long`, two `--porcelain=` in one argv), the invalid
//! porcelain versions, and the `status.relativePaths`, `status.branch`,
//! `status.displayCommentPrefix`, `status.submoduleSummary`,
//! `advice.statusHints`, `core.commentString`/`core.commentChar` keys.
//!
//! # The headline: `--porcelain=v2` is *not* repository-relative
//!
//! Run from a subdirectory, `--porcelain` (v1) prints paths from the repository
//! root and `--porcelain=v2` prints them relative to the current directory.
//! Measured on stock 2.55.0 in `Shape::IntentToAdd`, from `sub/`:
//!
//! ```text
//! $ git -C sub status --porcelain
//! AM both.txt
//!  A sub/ita-nested.txt
//! $ git -C sub status --porcelain=v2
//! 1 AM N... … ../both.txt
//! 1 .A N... … ita-nested.txt
//! ```
//!
//! Both of the harness's oracles agree (2.50.1 prints the same `../both.txt`),
//! so this is git's behaviour and not a version difference: v1 forces
//! `status.relativePaths` off and v2 honours it, default `true`. Any parser that
//! assumes v2 paths are repository-relative is wrong about every invocation from
//! a subdirectory, and any port that "fixes" v2 to match v1 breaks that same
//! parser in the opposite direction. [`relative_paths`] measures the pair
//! together, in all three formats, with the key set to each of its values, from
//! six directories — because "v1 and v2 disagree here" is only a finding if both
//! halves are asked in the same place.
//!
//! # Determinism
//!
//! Everything here was run twice against stock in a scratch copy of the fixture
//! and byte-compared before being written down. Three axes needed the check
//! specifically:
//!
//! * **`--column`** consults the terminal width. Both sides write to a pipe, and
//!   `env::harden` starts from `Command::env_clear`, so `COLUMNS` is absent on
//!   both sides and git falls back to its built-in 80. The cases that *set*
//!   `COLUMNS` set it symmetrically through [`Case::with_env`]; it is not one of
//!   `env::harden`'s pins (`env::pinned_keys`), so setting it adds a fact both
//!   sides see rather than replacing a determinism guarantee.
//! * **`--show-stash`** counts entries, so it needs `Shape::Stashed`'s three
//!   pre-existing ones to print anything but silence. The count is a property of
//!   the fixture, not of when the case runs.
//! * **`-v`/`-vv`** print a diff, which drags in diff's own configuration. That
//!   is the point rather than a hazard — the verbose block is part of status's
//!   output contract — but it is why the verbose cases stay on shapes whose
//!   diffs are small and textual and never on `Shape::Packed` or a binary path.
//!
//! `advice.statusUoption` is the one documented key in this family that is
//! **not** measurable: the advice it gates is emitted only when enumerating
//! untracked files took longer than `status.aheadBehind`-style wall clock
//! threshold (2 seconds), so a case that reached it would be reporting the
//! machine's load. It is deliberately absent, not overlooked.
//!
//! # States no shape can express
//!
//! Surveyed by walking every built template and asking stock git
//! (`ls-files -u`, `symbolic-ref HEAD`, and the presence of `MERGE_HEAD`,
//! `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG`, `rebase-merge/`,
//! `rebase-apply/`, `sequencer/`):
//!
//! * **A rebase in progress**, either backend — no template has `rebase-merge/`
//!   or `rebase-apply/`. `status`'s whole "interactive rebase in progress; onto
//!   …" / "You are currently editing a commit" block is unreachable.
//! * **A cherry-pick or revert in progress** — no template has
//!   `CHERRY_PICK_HEAD`, `REVERT_HEAD` or `sequencer/`.
//! * **A bisect in progress** — no template has `BISECT_LOG`; the
//!   "You are currently bisecting, started from branch …" line cannot be
//!   produced.
//! * **An unborn branch** — every template has at least one commit, so
//!   `# branch.oid (initial)` and "No commits yet on <branch>" are unreachable.
//! * **A branch both ahead and behind its upstream** — `Shape::BehindRemote`
//!   has one (`div`), but it is not `HEAD`, and a case is one argv against a
//!   pristine copy so it cannot check it out. Only `+0 -3` (behind-only) and the
//!   no-upstream case are reachable, and both are measured below.
//! * **Five of the seven unmerged combinations.** `Shape::Conflicted` yields
//!   `AA` and `Shape::Rerere` yields `AA` + `UU` + `UU`; `DD`, `AU`, `UD`, `UA`
//!   and `DU` appear in no template. Reaching them needs a fixture whose merge
//!   conflicts on a delete/modify and a rename/rename, which this file cannot
//!   add.
//! * **A type change**, **an assume-unchanged entry** — no template carries
//!   either, and both are made by a command (`update-index --assume-unchanged`,
//!   replacing a file with a symlink) that a single-argv case cannot run first.
//!   `Shape::Sparse` *does* carry skip-worktree entries, so that third one is
//!   measured (a port that ignores the bit reports the excluded paths deleted).
//! * **A dirty submodule.** Both submodule shapes are clean, so
//!   `--ignore-submodules=<when>` and `status.submoduleSummary` can only be
//!   measured as parsing plus the absence of a summary block. Said again where
//!   those cases are written, so a reader does not over-read them.
//!
//! `--no-optional-locks` is measurable in principle and vacuous in practice
//! here, for the reason `globals_layer.rs` already records: it suppresses an
//! index *refresh write*, and every probe in `runner::probe_state` is logical —
//! it asks stock git what the repository means and re-derives the answer — so a
//! refreshed and an unrefreshed index are indistinguishable to the harness.
//! Confirmed by hand rather than assumed: with `core.checkStat=default` forcing
//! the copied fixture stat-dirty, stock rewrites `.git/index` on a plain
//! `status` and leaves it alone under `--no-optional-locks`, and `write-tree`,
//! `ls-files --stage` and `status` answer identically over both. No case here
//! pretends otherwise.

//! # What the first run of this file found
//!
//! 346 cases; 305 match, 41 do not. Every one of the 41 was re-run against the
//! second oracle and every one is `corroborated-defect`: git 2.50.1 and git
//! 2.55.0 gave the same answer and the port gave another. None is
//! `version-skew`, none is nondeterministic, and both sides reproduced
//! themselves on the repeat. Six classes, each verified by hand in a copy of the
//! fixture before being written down:
//!
//! 1. **Format selection is not last-wins.** `--porcelain=v2 --long`,
//!    `--porcelain=v2 --short` and `--porcelain=v2 --porcelain=v1` all print v2;
//!    stock prints the long format, the short format and v1 respectively. The
//!    port appears to let the *widest* format win rather than the last one
//!    parsed, so the single-flag cases the rest of the corpus asks all pass.
//! 2. **`status.branch=true` leaks into both machine formats.** This is the one
//!    finding here that a human reader could never see. Under
//!    `-c status.branch=true` on `Shape::BehindRemote`:
//!
//!    ```text
//!    $ git -c status.branch=true status --porcelain=v2
//!    stock:  1 .M N... … clash.txt          (two lines, no headers)
//!    zvcs:   # branch.oid 54f11d58…         (four header lines first)
//!            # branch.head main
//!            # branch.upstream origin/main
//!            # branch.ab +0 -3
//!            1 .M N... … clash.txt
//!    ```
//!
//!    `--porcelain` gains a `## main...origin/main [behind 3]` line the same
//!    way. The long format and `-s` are byte-identical to stock under the same
//!    key, so a reader checking by eye sees nothing wrong while every v2 parser
//!    gets four records it did not ask for.
//! 3. **A `status.*` key overrides the command line.**
//!    `-c status.showUntrackedFiles=no status -uall` lists no untracked files;
//!    stock lists them. The configuration is being read after the argv instead
//!    of before it.
//! 4. **The `R<score>` field is not computed.** Every `2` record the port emits
//!    carries `R100`, including the pair stock scores `R60`. The threshold
//!    itself is honoured — at `-M90` both sides agree the pair is not a rename —
//!    so only the printed similarity is wrong, and neither the long nor the
//!    short format prints a similarity at all.
//! 5. **A trailing summary line survives a merge.**
//!    `status --long -- README.md` on `Shape::Rerere` ends with
//!    `nothing to commit, working tree clean`; stock ends after
//!    `(use "git commit" to conclude merge)`.
//! 6. **`-vv` drops intent-to-add paths from the appended diff.** The status
//!    block above lists `new file: ita-new.txt`; the verbose diff below it has
//!    no hunk for that path, so one output contradicts itself.
//!
//! The remaining failures are new spellings landing on the pending-rename
//! defect the corpus already had — the worktree-side rename (`2 .R`) is emitted
//! as an unrelated `1 .A` plus `1 .D` pair — reached now through `-M<n>`, the
//! percentage forms, `-s`, `-z`, `status.renames`, `diff.renames` and a
//! subdirectory.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

pub fn cases(out: &mut Vec<Case>) {
    format_selection(out);
    short_spellings(out);
    v2_branch_header(out);
    relative_paths(out);
    column_layout(out);
    verbose_diff(out);
    hints_and_comment_prefix(out);
    untracked_modes(out);
    unmerged_records(out);
    stash_line(out);
    rename_spellings(out);
    submodule_axes(out);
}

/// Shorthand: one status case, no configuration.
fn s(out: &mut Vec<Case>, args: &[&str], shape: Shape) {
    out.push(Case::new("status", args, shape));
}

/// Shorthand: one status case whose stderr is compared too. Used for every
/// refusal, because the message names the offending value and that name is the
/// only place the parser says what it thought it was given.
fn refuse(out: &mut Vec<Case>, args: &[&str], shape: Shape) {
    out.push(Case::strict("status", args, shape));
}

/// Shorthand: one status case under `-c key=value`.
fn c(out: &mut Vec<Case>, args: &[&str], shape: Shape, key: &str, value: &str) {
    out.push(Case::new("status", args, shape).with_config(&[(key, value)]));
}

/// Which format wins, and which combinations git refuses outright.
///
/// `status` has four mutually-exclusive renderings (`--long`, `--short`,
/// `--porcelain[=v1]`, `--porcelain=v2`) selected by a single `status_format`
/// variable that each option **overwrites**, so the answer to "what does
/// `--long --porcelain` print" is decided by argument *order* and not by a
/// precedence rule. Measured on stock 2.55.0: `--long --porcelain` prints
/// porcelain, `--porcelain=v2 --long` prints the long format, and
/// `--no-porcelain` restores the long format rather than clearing the request.
/// A port that resolves the conflict by precedence instead of by last-wins
/// scores perfectly on every single-flag case in the corpus and gets all six of
/// these backwards.
///
/// Two combinations are refusals rather than a last-wins choice, and both are
/// `Case::strict` because the message names the value it rejected:
/// `fatal: options '--long' and '-z' cannot be used together` (either order) and
/// `fatal: unsupported porcelain version '<v>'`.
fn format_selection(out: &mut Vec<Case>) {
    let d = Shape::Dirty;

    // Last-wins, both directions, for each pair of formats.
    s(out, &["status", "--long", "--porcelain"], d);
    s(out, &["status", "--porcelain", "--long"], d);
    s(out, &["status", "--long", "--porcelain=v2"], d);
    s(out, &["status", "--porcelain=v2", "--long"], d);
    s(out, &["status", "--short", "--porcelain"], d);
    s(out, &["status", "--porcelain", "--short"], d);
    s(out, &["status", "--short", "--porcelain=v2"], d);
    s(out, &["status", "--porcelain=v2", "--short"], d);
    s(out, &["status", "--porcelain=v1", "--porcelain=v2"], d);
    s(out, &["status", "--porcelain=v2", "--porcelain=v1"], d);

    // The negations. None of the three clears the format back to "unset"; each
    // selects the long format, which is a different statement from "undo".
    s(out, &["status", "--no-porcelain"], d);
    s(out, &["status", "--porcelain", "--no-porcelain"], d);
    s(out, &["status", "--no-short"], d);
    s(out, &["status", "--short", "--no-short"], d);
    s(out, &["status", "--no-long"], d);
    s(out, &["status", "--porcelain=v2", "--no-long"], d);

    // `-z` with no format selects v1 — the one implicit format selection in the
    // command. On `Conflicted` too, because the `u` line is the record whose
    // NUL-terminated form has the most fields to get wrong.
    s(out, &["status", "-z"], d);
    s(out, &["status", "-z"], Shape::Conflicted);
    s(out, &["status", "-z"], Shape::PendingRename);
    s(out, &["status", "--short", "-z"], d);
    s(out, &["status", "-z", "--short"], d);
    s(out, &["status", "-z", "--porcelain=v2"], d);

    // The version argument is parsed by hand, and the bare digit is a legal
    // spelling: stock 2.55.0 accepts `--porcelain=1` and `--porcelain=2` and
    // renders exactly what `v1`/`v2` render. A port that matches the literal
    // strings `v1`/`v2` rejects both and no case in the corpus would have said
    // so, which is why they are ordinary cases rather than refusals.
    s(out, &["status", "--porcelain=1"], d);
    s(out, &["status", "--porcelain=2"], d);

    // The refusals. Strict, because each message quotes the value it rejected
    // and that quotation is the only place the parser says what it read.
    refuse(out, &["status", "-z", "--long"], d);
    refuse(out, &["status", "--long", "-z"], d);
    refuse(out, &["status", "--porcelain=v3"], d);
    refuse(out, &["status", "--porcelain=v0"], d);
    refuse(out, &["status", "--porcelain="], d);

    // `status.short` against an explicit format on the command line. The key is
    // already measured with a bare argv (`config_reads.rs`, from the global
    // scope); what was never asked is whether a flag overrides it, in either
    // direction.
    c(out, &["status", "--long"], d, "status.short", "true");
    c(out, &["status", "--porcelain=v2"], d, "status.short", "true");
    c(out, &["status", "--no-short"], d, "status.short", "true");
    c(out, &["status", "--short"], d, "status.short", "false");
    c(out, &["status", "-z"], d, "status.short", "true");
}

/// The short spellings, which no case in the corpus had ever used.
///
/// `-s`, `-b` and the cluster `-sb` are the forms every human and every wrapper
/// script actually types; the corpus only ever wrote `--short` and `--branch`.
/// They are not free aliases in a port: a cluster has to be split by the
/// option parser before either letter is recognised, and `-sb` is the only
/// place in this command where that happens.
///
/// `-b` on its own is deliberately included even though the long format already
/// prints the branch: a port that implements `-b` as "print the branch header"
/// rather than as "set a flag the long format already honours" prints it twice.
fn short_spellings(out: &mut Vec<Case>) {
    for shape in [
        Shape::Dirty,
        Shape::Conflicted,
        Shape::Rerere,
        Shape::IntentToAdd,
        Shape::PendingRename,
        Shape::Stashed,
        Shape::Symlinks,
        Shape::Attributes,
        Shape::BehindRemote,
        Shape::Sparse,
        Shape::MergeableDirty,
        Shape::MergeableStaged,
        Shape::AwkwardPaths,
        Shape::DecomposedPaths,
    ] {
        s(out, &["status", "-s"], shape);
    }

    // `-b` alone, and the cluster, across the head states that make the branch
    // line say different things: a branch with an upstream it is behind, a
    // branch with no upstream at all, a detached HEAD, and a linked worktree on
    // its own branch.
    for shape in [
        Shape::Linear,
        Shape::Branched,
        Shape::Detached,
        Shape::BehindRemote,
        Shape::Cherry,
        Shape::Octopus,
        Shape::Dirty,
    ] {
        s(out, &["status", "-b"], shape);
        s(out, &["status", "-sb"], shape);
    }
    s(out, &["status", "-b"], Shape::Worktree);
    s(out, &["status", "-sb"], Shape::Worktree);
    out.push(Case::new("status", &["status", "-sb"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("status", &["status", "-b"], Shape::Worktree).in_dir("wt"));

    // `-s -b` as two arguments is a different parse from the cluster and a
    // different case id; a splitter that mishandles one handles the other.
    s(out, &["status", "-s", "-b"], Shape::Dirty);
    s(out, &["status", "-b", "-s"], Shape::Dirty);
    s(out, &["status", "-sb"], Shape::Rerere);
    s(out, &["status", "-sb"], Shape::Conflicted);

    // The short format's own ahead/behind rendering: `[behind 3]` against v2's
    // `# branch.ab +0 -3`, and `[different]` against `+? -?`. `index_plumbing`
    // owns the v2 half of this cross; the short half had no case at all.
    s(out, &["status", "-sb", "--no-ahead-behind"], Shape::BehindRemote);
    s(out, &["status", "-sb", "--ahead-behind"], Shape::BehindRemote);
    c(out, &["status", "-sb"], Shape::BehindRemote, "status.aheadBehind", "false");
    c(out, &["status", "-sb"], Shape::BehindRemote, "status.aheadBehind", "true");
    s(out, &["status", "--short", "--no-ahead-behind"], Shape::BehindRemote);

    // `status.branch`, listed by `config_reads.rs` as unreachable there and set
    // by no case anywhere. Both directions, so "config on, flag off" and "config
    // off, flag on" are separate answers.
    c(out, &["status", "--short"], Shape::Dirty, "status.branch", "true");
    c(out, &["status", "-s"], Shape::BehindRemote, "status.branch", "true");
    c(out, &["status", "-sb"], Shape::Dirty, "status.branch", "false");
    c(out, &["status", "--short", "--no-branch"], Shape::Dirty, "status.branch", "true");
    c(out, &["status", "--porcelain=v2"], Shape::BehindRemote, "status.branch", "true");
    c(out, &["status", "--porcelain"], Shape::BehindRemote, "status.branch", "true");
    out.push(
        Case::new("status", &["status", "--short"], Shape::BehindRemote).with_scoped_config(
            vec![ConfigEntry::set(ConfigScope::Repo, "status.branch", "true")],
        ),
    );
}

/// `# branch.oid` / `# branch.head` / `# branch.upstream` / `# branch.ab`, on
/// the head states `index_plumbing.rs`'s v2 sweep never ran on.
///
/// That sweep covers `Dirty`, `Detached`, `Merged`, `Worktree`, `Conflicted`,
/// `BehindRemote`, `Submodule` and (through a config case) `Stashed`; the four
/// header lines are therefore measured against exactly two `branch.head`
/// values, one `branch.upstream` and one `branch.ab`. Everything below adds a
/// head state rather than a flag: a branch that is not `main`
/// (`Cherry` is on `topic`), a merge with four parents, two roots that share no
/// history, a linked worktree read *from inside itself*, and the plain
/// `--branch` on `Stashed` that only ever appeared with `--show-stash` or a
/// configuration key attached.
///
/// The `-b` spelling is used deliberately where the long spelling already
/// exists on that shape, so the case is a new parse and not a second copy.
fn v2_branch_header(out: &mut Vec<Case>) {
    for shape in [
        Shape::Linear,
        Shape::Branched,
        Shape::Cherry,
        Shape::Octopus,
        Shape::CrissCross,
        Shape::Unrelated,
        Shape::MergeableDirty,
        Shape::MergeableStaged,
        Shape::TagChain,
        Shape::AwkwardPaths,
        Shape::Symlinks,
        Shape::Stashed,
        Shape::Attributes,
    ] {
        s(out, &["status", "--porcelain=v2", "--branch"], shape);
    }

    // `-b` where `--branch` is already spoken for.
    for shape in [Shape::Dirty, Shape::Detached, Shape::BehindRemote, Shape::Conflicted] {
        s(out, &["status", "--porcelain=v2", "-b"], shape);
    }

    // The linked worktree read from inside itself. `Shape::Worktree`'s `wt/` is
    // on its own branch (`linked`), so a port that resolves `HEAD` through the
    // common directory prints `main` here and is right everywhere else.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-b"], Shape::Worktree).in_dir("wt"));

    // `branch.ab` is the only header line with a *computed* value, and
    // `--no-ahead-behind` replaces the counts with `+? -?` rather than dropping
    // the line. The v2 half of that is `index_plumbing.rs`'s; what it never
    // asked is what the flag does with no upstream to count against, where the
    // whole `branch.upstream`/`branch.ab` pair is absent and the flag has
    // nothing to suppress.
    s(out, &["status", "--porcelain=v2", "--branch", "--no-ahead-behind"], Shape::Branched);
    s(out, &["status", "--porcelain=v2", "--branch", "--ahead-behind"], Shape::Branched);
    s(out, &["status", "--porcelain=v2", "--branch", "--no-ahead-behind"], Shape::Detached);
    c(out, &["status", "--porcelain=v2", "--branch"], Shape::Branched, "status.aheadBehind", "false");
}

/// **The headline.** `status.relativePaths`, and the fact that `--porcelain`
/// and `--porcelain=v2` answer it differently.
///
/// Run from a subdirectory, v1 prints repository-root-relative paths and v2
/// prints current-directory-relative ones. Measured on stock 2.55.0 in
/// `Shape::IntentToAdd` from `sub/`, and reproduced identically by the second
/// oracle at 2.50.1, so it is git's behaviour rather than a version difference:
///
/// ```text
/// $ git -C sub status --porcelain      | $ git -C sub status --porcelain=v2
/// AM both.txt                          | 1 AM N... … ../both.txt
///  D ita-gone.txt                      | 1 .D N... … ../ita-gone.txt
///  A sub/ita-nested.txt                | 1 .A N... … ita-nested.txt
/// ?? untracked.txt                     | ? ../untracked.txt
/// ```
///
/// `status.relativePaths` defaults to true; v1 forces it off for its own output
/// and v2 does not. So the key is invisible on v1 (setting it either way changes
/// nothing), decisive on v2, and decisive on the long and short formats too.
/// Four renderings, three key states (unset, `true`, `false`), from six
/// directories — because a case that only asked v2 would report a difference
/// with no baseline beside it, and a case that only asked from the root would
/// find no difference at all.
///
/// `status.relativePaths` is set by no other case in the corpus;
/// `config_reads.rs` records it as unreachable from there.
fn relative_paths(out: &mut Vec<Case>) {
    // The full cross, on the shape with the most to say from a subdirectory:
    // `sub/` holds one intent-to-add entry while the root holds four more
    // entries of three other kinds, so every line either gains a `../` or does
    // not and the two groups are visible against each other.
    for args in [
        &["status", "--porcelain=v2"][..],
        &["status", "--porcelain=v1"],
        &["status", "-s"],
        &["status", "--long"],
    ] {
        out.push(Case::new("status", args, Shape::IntentToAdd).in_dir("sub"));
        for value in ["true", "false"] {
            out.push(
                Case::new("status", args, Shape::IntentToAdd)
                    .in_dir("sub")
                    .with_config(&[("status.relativePaths", value)]),
            );
        }
        // The same argv from the root, so the subdirectory cases have a
        // baseline in the corpus rather than in a reader's head.
        out.push(
            Case::new("status", args, Shape::IntentToAdd)
                .with_config(&[("status.relativePaths", "false")]),
        );
    }

    // The other five directories. Reduced to the pair that disagrees plus the
    // long format, because the point here is that the disagreement is a
    // property of the format and not of one fixture.
    for (shape, dir) in [
        (Shape::Dirty, "src"),
        (Shape::Attributes, "sub"),
        (Shape::PendingRename, "pkg"),
        (Shape::Sparse, "inside"),
        (Shape::Rerere, "src"),
    ] {
        out.push(Case::new("status", &["status", "--porcelain=v2"], shape).in_dir(dir));
        out.push(Case::new("status", &["status", "--porcelain=v1"], shape).in_dir(dir));
        out.push(Case::new("status", &["status", "--long"], shape).in_dir(dir));
        out.push(
            Case::new("status", &["status", "--porcelain=v2"], shape)
                .in_dir(dir)
                .with_config(&[("status.relativePaths", "false")]),
        );
        out.push(
            Case::new("status", &["status", "--long"], shape)
                .in_dir(dir)
                .with_config(&[("status.relativePaths", "false")]),
        );
    }

    // A rename pair straddling the boundary is the one place the key has to
    // rewrite *two* paths on one line, and v2's `2` record puts them either side
    // of a tab. `pkg/deep.txt -> pkg/deep-renamed.txt` is inside `pkg/`; the
    // other four pairs are above it.
    out.push(Case::new("status", &["status", "--porcelain=v2", "-z"], Shape::PendingRename).in_dir("pkg"));
    out.push(Case::new("status", &["status", "-s"], Shape::PendingRename).in_dir("pkg"));
    out.push(
        Case::new("status", &["status", "-s"], Shape::PendingRename)
            .in_dir("pkg")
            .with_config(&[("status.relativePaths", "false")]),
    );

    // Delivered from a file scope rather than from `-c`, once. The key is read
    // by `git_status_config` like any other, but a port that only honours the
    // command-line scope for `status.*` passes every case above.
    out.push(
        Case::new("status", &["status", "--porcelain=v2"], Shape::IntentToAdd)
            .in_dir("sub")
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Repo,
                "status.relativePaths",
                "false",
            )]),
    );
    out.push(
        Case::new("status", &["status", "--long"], Shape::IntentToAdd)
            .in_dir("sub")
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Global,
                "status.relativePaths",
                "false",
            )]),
    );
}

/// `--column`, in every style, and the two config keys that reach it.
///
/// Used by no case in the corpus: `informational.rs` has one
/// `-c column.ui=always status --short` filed under the `column` command, and
/// that is the whole of it. The layout engine is real code — it measures every
/// entry, decides a row count against the terminal width, and pads — and it runs
/// only on the long format's untracked list, which is why the shape matters:
/// `Shape::Attributes` has exactly two untracked files that fit side by side at
/// 80 columns and do not at 20.
///
/// Deterministic because both sides write to a pipe and `env::harden` clears the
/// environment, so `COLUMNS` is absent and git falls back to its built-in 80.
/// Verified by running each of these twice against stock in a scratch copy and
/// byte-comparing. The three cases that *set* `COLUMNS` set it through
/// [`Case::with_env`]: it is not one of `env::harden`'s pins, so both sides get
/// the same value and the width becomes a stated fact instead of an inherited
/// one.
fn column_layout(out: &mut Vec<Case>) {
    let a = Shape::Attributes;
    for style in [
        "--column",
        "--no-column",
        "--column=always",
        "--column=never",
        "--column=auto",
        "--column=row",
        "--column=column",
        "--column=plain",
        "--column=always,dense",
        "--column=always,nodense",
    ] {
        s(out, &["status", style], a);
    }
    refuse(out, &["status", "--column=bogus"], a);
    refuse(out, &["status", "--column=always,bogus"], a);

    // Width. 20 columns cannot fit `important.log` and `tracked-looking.txt` on
    // one row and 200 can, so a port that lays out against a hard-coded 80
    // agrees with stock on the third of these and on neither of the first two.
    for width in ["20", "40", "200"] {
        out.push(
            Case::new("status", &["status", "--column"], a).with_env(&[("COLUMNS", width)]),
        );
    }
    // The same widths with no `--column`: the flag is off, so the width must
    // change nothing at all. A port that reads `COLUMNS` unconditionally fails
    // here and passes everything above.
    out.push(Case::new("status", &["status"], a).with_env(&[("COLUMNS", "20")]));

    // `column.status` and the `column.ui` fallback beneath it, both directions.
    c(out, &["status"], a, "column.status", "always");
    c(out, &["status"], a, "column.status", "never");
    c(out, &["status"], a, "column.status", "always,dense");
    c(out, &["status", "--column"], a, "column.status", "never");
    c(out, &["status", "--no-column"], a, "column.status", "always");
    c(out, &["status"], a, "column.ui", "always");
    c(out, &["status", "--no-column"], a, "column.ui", "always");
    out.push(
        Case::new("status", &["status"], a)
            .with_config(&[("column.ui", "always"), ("column.status", "never")]),
    );
    out.push(
        Case::new("status", &["status"], a).with_scoped_config(vec![ConfigEntry::set(
            ConfigScope::Repo,
            "column.status",
            "always",
        )]),
    );

    // The formats the layout must *not* reach. `--column` is accepted beside
    // `-s` and `--porcelain` and has to be inert in both, because a machine
    // reader cannot survive padded columns.
    s(out, &["status", "--column", "-s"], a);
    s(out, &["status", "--column=always", "--porcelain"], a);
    s(out, &["status", "--column=always", "--porcelain=v2"], a);
    c(out, &["status", "--porcelain"], a, "column.ui", "always");

    // A second shape, so the layout is not measured against one pair of names.
    s(out, &["status", "--column=always"], Shape::Symlinks);
    s(out, &["status", "--column=always", "-uall"], Shape::Symlinks);
}

/// `-v` and `-vv`: the diff status prints below its own output.
///
/// Never passed to `status` by any case. `-v` appends the staged diff and `-vv`
/// appends the unstaged one after it under a `---…---` rule, with the
/// `c/`…`i/`…`w/` prefixes that appear nowhere else in git — `i/README.md` for
/// the index side, `w/README.md` for the worktree side. That prefix scheme is
/// `--mnemonic-prefix` under a different name, and a port that reuses its
/// ordinary `a/`/`b/` diff renderer here is wrong on every line of it while the
/// status block above stays perfect.
///
/// Deliberately kept to shapes whose diffs are small and textual. The verbose
/// block pulls diff's own configuration into status's output, which is the point
/// — it is part of what `status` prints — but it is also why none of these runs
/// on a binary path or on `Shape::Packed`.
fn verbose_diff(out: &mut Vec<Case>) {
    for shape in [
        Shape::Dirty,
        Shape::Whitespace,
        Shape::IntentToAdd,
        Shape::MergeableStaged,
        Shape::Conflicted,
    ] {
        s(out, &["status", "-v"], shape);
        s(out, &["status", "-vv"], shape);
    }
    s(out, &["status", "--verbose"], Shape::Dirty);
    s(out, &["status", "-v", "-v"], Shape::Dirty);
    s(out, &["status", "--verbose", "--verbose"], Shape::Dirty);
    s(out, &["status", "--no-verbose"], Shape::Dirty);
    s(out, &["status", "-v", "--no-verbose"], Shape::Dirty);
    s(out, &["status", "-vv"], Shape::PendingRename);
    s(out, &["status", "-vv"], Shape::Symlinks);

    // The verbose block is a diff, so diff's configuration reaches it. Each of
    // these changes the appended hunk and none of them changes the status block
    // above it, which is the separation being measured.
    c(out, &["status", "-v"], Shape::Dirty, "diff.noprefix", "true");
    c(out, &["status", "-vv"], Shape::Dirty, "diff.mnemonicPrefix", "false");
    c(out, &["status", "-v"], Shape::Whitespace, "diff.context", "0");
    c(out, &["status", "-vv"], Shape::Whitespace, "core.whitespace", "trailing-space");
    c(out, &["status", "-v"], Shape::PendingRename, "diff.renames", "false");

    // Verbose against each of the other three formats. `-v` is silently inert in
    // all three; a port that appends the diff to porcelain output breaks every
    // parser at once, and no case in the corpus would have caught it.
    s(out, &["status", "-v", "--porcelain"], Shape::Dirty);
    s(out, &["status", "-vv", "--porcelain=v2"], Shape::Dirty);
    s(out, &["status", "-v", "-s"], Shape::Dirty);

    // Verbose from a subdirectory: the diff's own paths are repository-relative
    // whatever the status block above them does.
    out.push(Case::new("status", &["status", "-v"], Shape::Dirty).in_dir("src"));
    out.push(
        Case::new("status", &["status", "-v"], Shape::Dirty)
            .in_dir("src")
            .with_config(&[("status.relativePaths", "false")]),
    );
}

/// `advice.statusHints` and `status.displayCommentPrefix`: the two keys that
/// rewrite every line of the long format without changing a single fact in it.
///
/// Neither is set by any case. The corpus reaches advice suppression twice —
/// `env_layer.rs` with `GIT_ADVICE=0` and `globals_layer.rs` with
/// `--no-advice`, both of which turn off *all* advice — and never through the
/// per-message key, so a port with one global advice switch and no per-key
/// lookup scores full marks. `status.displayCommentPrefix` is listed by
/// `config_reads.rs` as unreachable there.
///
/// The comment prefix is `core.commentChar`/`core.commentString`, so the pair
/// crosses: stock 2.55.0 renders `REM On branch main` under
/// `core.commentString=REM`, and a bare `#` line for the blank separators.
///
/// `advice.statusUoption` is the one key in this family left out on purpose:
/// the advice it gates fires only when the untracked walk took more than two
/// seconds, so a case that reached it would be reporting the machine's load
/// rather than the port's behaviour.
fn hints_and_comment_prefix(out: &mut Vec<Case>) {
    for shape in [
        Shape::Dirty,
        Shape::Conflicted,
        Shape::Sparse,
        Shape::Stashed,
        Shape::IntentToAdd,
        Shape::Rerere,
        Shape::Attributes,
    ] {
        c(out, &["status"], shape, "advice.statusHints", "false");
    }
    c(out, &["status"], Shape::Conflicted, "advice.statusHints", "true");
    // The `-uno` hint — "Untracked files not listed (use -u option to show
    // untracked files)" — is its own line, gated by the same key, and reachable
    // only with untracked display off.
    c(out, &["status", "-uno"], Shape::Dirty, "advice.statusHints", "false");
    c(out, &["status", "-uno"], Shape::Dirty, "advice.statusHints", "true");
    // Inert on the machine formats, which carry no hints to suppress.
    c(out, &["status", "--porcelain"], Shape::Conflicted, "advice.statusHints", "false");
    c(out, &["status", "-s"], Shape::Conflicted, "advice.statusHints", "false");

    for shape in [Shape::Dirty, Shape::Conflicted, Shape::Sparse, Shape::BehindRemote] {
        c(out, &["status"], shape, "status.displayCommentPrefix", "true");
    }
    c(out, &["status"], Shape::Dirty, "status.displayCommentPrefix", "false");
    c(out, &["status", "--short"], Shape::Dirty, "status.displayCommentPrefix", "true");
    c(out, &["status", "--porcelain=v2"], Shape::Dirty, "status.displayCommentPrefix", "true");
    out.push(
        Case::new("status", &["status"], Shape::Dirty)
            .with_config(&[("status.displayCommentPrefix", "true"), ("core.commentString", "REM")]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Dirty)
            .with_config(&[("status.displayCommentPrefix", "true"), ("core.commentChar", ";")]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Dirty)
            .with_config(&[("status.displayCommentPrefix", "true"), ("advice.statusHints", "false")]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Conflicted).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Global, "status.displayCommentPrefix", "true"),
            ConfigEntry::set(ConfigScope::Repo, "advice.statusHints", "false"),
        ]),
    );
    // `-v` under the comment prefix: the appended diff is *not* prefixed, so the
    // one output that mixes prefixed and unprefixed lines is worth its own case.
    out.push(
        Case::new("status", &["status", "-v"], Shape::Dirty)
            .with_config(&[("status.displayCommentPrefix", "true")]),
    );
}

/// The untracked-files axis, which the corpus only ever set to `all` or `no`.
///
/// `-u` takes three modes and every case before this file used two of them.
/// `normal` is the default, so the interesting question is not what it prints
/// but whether it is *reachable*: a port that parses `-u<mode>` by matching
/// `all` and `no` and treating anything else as an error, or as `all`, is
/// invisible to the whole corpus. Bare `-u` (which means `all`, not `normal`)
/// and `--no-untracked-files` (which restores the default rather than turning
/// listing off) are the two spellings most likely to be got wrong.
///
/// The `--ignored` cross here varies the *untracked* mode only.
/// `gitignore_precedence.rs` owns `--ignored` as an ignore-rule question — the
/// `core.excludesFile` cases, the pathspec-limited listings, and the
/// `--ignored=matching -uno` refusal — and never moves this axis.
fn untracked_modes(out: &mut Vec<Case>) {
    for shape in [Shape::Dirty, Shape::Attributes, Shape::IntentToAdd, Shape::Sparse] {
        s(out, &["status", "-unormal"], shape);
        s(out, &["status", "--untracked-files=normal"], shape);
    }
    s(out, &["status", "-u"], Shape::Dirty);
    s(out, &["status", "--untracked-files"], Shape::Dirty);
    s(out, &["status", "--no-untracked-files"], Shape::Dirty);
    s(out, &["status", "-uall", "--no-untracked-files"], Shape::Attributes);
    s(out, &["status", "-uno", "-unormal"], Shape::Dirty);
    s(out, &["status", "-u", "--porcelain=v2"], Shape::Attributes);
    s(out, &["status", "-unormal", "--porcelain=v2"], Shape::Attributes);
    s(out, &["status", "-unormal", "-s"], Shape::Attributes);
    // On `Attributes` rather than `Dirty`: `corpus.rs` already sweeps this key's
    // three values with `--porcelain` on `Dirty`, and `Attributes` is where
    // `normal` and `all` differ by more than nothing.
    c(out, &["status", "--porcelain"], Shape::Attributes, "status.showUntrackedFiles", "normal");
    c(out, &["status", "-uall"], Shape::Dirty, "status.showUntrackedFiles", "no");

    // The cross with `--ignored`. `matching` and `traditional` differ in whether
    // an ignored *directory* is listed whole or expanded, and — measured on
    // stock 2.55.0 — the untracked mode flips which of them expands:
    //
    //   status --ignored=traditional -uall  ->  !! build/output.o
    //   status --ignored=matching    -uall  ->  !! build/
    //
    // That inversion is the one behaviour in this family a reader is most likely
    // to assume backwards, and no case had both halves.
    let a = Shape::Attributes;
    s(out, &["status", "--porcelain", "--ignored=matching", "-uall"], a);
    s(out, &["status", "--porcelain", "--ignored=matching", "-unormal"], a);
    s(out, &["status", "--porcelain", "--ignored=traditional", "-unormal"], a);
    s(out, &["status", "--porcelain", "--ignored", "-unormal"], a);
    s(out, &["status", "--porcelain", "--ignored", "-u"], a);
    s(out, &["status", "--porcelain=v2", "--ignored=matching", "-uall"], a);
    s(out, &["status", "--porcelain=v2", "--ignored=traditional", "-uall"], a);
    s(out, &["status", "-s", "--ignored=matching", "-uall"], a);
    s(out, &["status", "--porcelain", "--ignored=no", "-unormal"], a);

    // Invalid values. Each message quotes the value, and the three of them are
    // worded differently from each other in git — `Invalid untracked files mode`,
    // `Invalid ignored mode`, `bad --ignore-submodules argument` — which is
    // exactly the kind of detail a port normalises into one sentence.
    refuse(out, &["status", "--untracked-files=bogus"], Shape::Dirty);
    refuse(out, &["status", "-ubogus"], Shape::Dirty);
    refuse(out, &["status", "--ignored=bogus"], Shape::Dirty);
    refuse(out, &["status", "--ignore-submodules=bogus"], Shape::Dirty);
}

/// The `u` record and the long format's "Unmerged paths" block, in the
/// spellings `index_plumbing.rs` and `fixture_gaps2.rs` did not use.
///
/// Two of the seven stage combinations exist in the fixtures and five do not.
/// `Shape::Conflicted` is a single `AA`; `Shape::Rerere` is `AA` plus two `UU`,
/// which is the only shape where the `u` record appears more than once and so
/// the only place the *ordering* of unmerged entries against ordinary ones can
/// be seen. `DD`, `AU`, `UD`, `UA` and `DU` need a merge that conflicts on a
/// delete/modify or a rename/rename, which no fixture builds and which a case —
/// one argv against a pristine copy — cannot build for itself. They are named
/// here rather than approximated, because a case that pretended to measure `DU`
/// by running `status` on a `UU` fixture would be worse than no case.
///
/// `index_plumbing.rs` holds v2 on `Conflicted` (bare, `--branch`, `-z`) and
/// `fixture_gaps2.rs` holds v2/v1/`--short`/`--long` on `Rerere`. Everything
/// below is a spelling or an axis neither of them reached.
fn unmerged_records(out: &mut Vec<Case>) {
    let r = Shape::Rerere;
    s(out, &["status", "--porcelain=v2", "--branch"], r);
    s(out, &["status", "--porcelain=v2", "-z"], r);
    s(out, &["status", "--porcelain=v1", "-z"], r);
    s(out, &["status", "--porcelain=v2", "-uall"], r);
    s(out, &["status", "--porcelain=v2", "--no-renames"], r);
    s(out, &["status", "--porcelain=v2", "--find-renames=50"], r);
    s(out, &["status", "--porcelain=v1", "--ignored"], r);
    s(out, &["status", "-sb"], Shape::MergeableDirty);
    s(out, &["status", "--long"], Shape::Conflicted);
    s(out, &["status", "--porcelain=v1"], Shape::Conflicted);
    s(out, &["status", "--porcelain=v1", "-uall"], Shape::Conflicted);
    s(out, &["status", "-v"], Shape::Rerere);

    // The `u` record's mode columns. `AA` has no stage-1 entry, so its first
    // mode column is `000000` while `UU`'s is `100644`; both are on `Rerere` and
    // nothing else in the corpus prints them side by side.
    out.push(Case::new("status", &["status", "-s"], r).in_dir("src"));

    // Unmerged entries under a pathspec, which decides whether the merge-state
    // header above them is still printed when nothing matches.
    s(out, &["status", "--porcelain=v2", "--", "rr.txt"], r);
    s(out, &["status", "--long", "--", "README.md"], r);
    s(out, &["status", "--porcelain=v2", "--", "nosuch"], r);
}

/// The stash line, in the two formats `index_plumbing.rs`'s v2 cases could not
/// reach, and the config key crossed with the flag.
///
/// `--show-stash` prints `Your stash currently has 3 entries` — a *count*, which
/// is why it needs `Shape::Stashed`'s three pre-existing entries and why the
/// count is a property of the fixture rather than of when the case runs. Stock
/// 2.55.0 prints the line in the long format only: with `-s`, `--porcelain` or
/// `--porcelain=v2` the flag is accepted and silent, which is a claim worth
/// pinning because a port that emits a `# stash <n>` header into v2 breaks every
/// parser that does not expect one.
fn stash_line(out: &mut Vec<Case>) {
    let st = Shape::Stashed;
    s(out, &["status", "--show-stash"], st);
    s(out, &["status", "--no-show-stash"], st);
    s(out, &["status", "--show-stash", "--no-show-stash"], st);
    s(out, &["status", "--show-stash", "-s"], st);
    s(out, &["status", "-sb", "--show-stash"], st);
    s(out, &["status", "--show-stash", "--porcelain=v1"], st);
    s(out, &["status", "--show-stash", "-v"], st);
    c(out, &["status", "--show-stash"], st, "status.showStash", "false");
    c(out, &["status", "--no-show-stash"], st, "status.showStash", "true");
    c(out, &["status", "-s"], st, "status.showStash", "true");
    c(out, &["status", "--porcelain=v1"], st, "status.showStash", "true");
    out.push(
        Case::new("status", &["status"], st).with_scoped_config(vec![ConfigEntry::set(
            ConfigScope::Repo,
            "status.showStash",
            "true",
        )]),
    );
    // An empty stash, where the line is suppressed rather than printed as zero.
    s(out, &["status", "--show-stash"], Shape::Linear);
    c(out, &["status"], Shape::Dirty, "status.showStash", "true");
}

/// The rename spellings `fixture_gaps2.rs`'s threshold ladder did not use, and
/// the flag-beats-config direction it did not cross.
///
/// That ladder is `--find-renames=30|50|60|70|90` with `--porcelain=v2` on
/// `Shape::PendingRename`, plus `status.renames=false|copies` and
/// `diff.renameLimit=1`. It never spells the option `-M`, never uses the
/// percentage form, never asks what a bare `--find-renames` means, and never
/// puts a flag and a key in conflict. All four are here, on the same shape, so
/// the two files read as one measurement of the same fixture.
///
/// The `R<score>` field is v2's only computed column, and the fixture's pairs
/// are placed to separate thresholds rather than to move together: `R100` for
/// three pairs, `R060` for one, `R039` for one.
fn rename_spellings(out: &mut Vec<Case>) {
    let p = Shape::PendingRename;
    for spelling in ["-M", "-M30", "-M60", "-M90", "-M60%", "-M39%"] {
        s(out, &["status", "--porcelain=v2", spelling], p);
    }
    s(out, &["status", "--find-renames", "--porcelain=v2"], p);
    s(out, &["status", "--find-renames=60%", "--porcelain=v2"], p);
    s(out, &["status", "--find-renames=39%", "--porcelain=v2"], p);
    s(out, &["status", "-s", "-M90"], p);
    s(out, &["status", "-s", "--find-renames=30"], p);
    s(out, &["status", "--long", "--no-renames"], p);
    s(out, &["status", "--long", "-M90"], p);
    s(out, &["status", "-sb", "--no-renames"], p);
    s(out, &["status", "--porcelain=v1", "--no-renames"], p);

    // Flag against key, both directions, and the `diff.renames` fallback beneath
    // `status.renames`.
    out.push(
        Case::new("status", &["status", "--renames", "--porcelain=v2"], p)
            .with_config(&[("status.renames", "false")]),
    );
    out.push(
        Case::new("status", &["status", "--no-renames", "--porcelain=v2"], p)
            .with_config(&[("status.renames", "true")]),
    );
    out.push(
        Case::new("status", &["status", "-M30", "--porcelain=v2"], p)
            .with_config(&[("status.renames", "false")]),
    );
    c(out, &["status", "--porcelain=v2"], p, "status.renames", "true");
    c(out, &["status", "--long"], p, "status.renames", "copies");
    c(out, &["status", "--porcelain=v2"], p, "diff.renames", "copies");
    c(out, &["status", "--porcelain=v2"], p, "diff.renames", "false");
    out.push(
        Case::new("status", &["status", "--porcelain=v2"], p)
            .with_config(&[("diff.renames", "false"), ("status.renames", "true")]),
    );
    out.push(
        Case::new("status", &["status", "--porcelain=v2"], p).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "status.renames", "copies"),
        ]),
    );
}

/// `--ignore-submodules` in the spelling with no value, and
/// `status.submoduleSummary`.
///
/// **Read narrowly.** Both submodule shapes are clean — `git status` in
/// `Shape::Submodule` prints `nothing to commit, working tree clean` and its
/// porcelain output is empty — so nothing here can measure summary *rendering*
/// or the difference between `dirty` and `untracked`. What it does measure is
/// that the option is accepted in each spelling, that the bare form (which
/// defaults to `all`) parses at all, and that a clean submodule produces no
/// summary block under a key that would otherwise add one. A port that emitted
/// a spurious empty `Submodule changes to be committed:` header would be caught;
/// one that renders a real summary wrongly would not, and that is a fixture gap
/// this file cannot close.
fn submodule_axes(out: &mut Vec<Case>) {
    for shape in [Shape::Submodule, Shape::NestedSubmodule] {
        s(out, &["status", "--ignore-submodules"], shape);
        s(out, &["status", "--ignore-submodules=dirty"], shape);
        s(out, &["status", "--ignore-submodules=untracked"], shape);
        c(out, &["status"], shape, "status.submoduleSummary", "true");
    }
    s(out, &["status", "--porcelain=v2", "--ignore-submodules"], Shape::Submodule);
    s(out, &["status", "-s", "--ignore-submodules=dirty"], Shape::Submodule);
    c(out, &["status"], Shape::Submodule, "status.submoduleSummary", "10");
    c(out, &["status"], Shape::Submodule, "status.submoduleSummary", "false");
    c(out, &["status", "--porcelain=v1"], Shape::Submodule, "diff.ignoreSubmodules", "all");
    c(out, &["status"], Shape::Submodule, "diff.ignoreSubmodules", "dirty");
    out.push(
        Case::new("status", &["status"], Shape::Submodule).with_config(&[
            ("status.submoduleSummary", "true"),
            ("diff.ignoreSubmodules", "all"),
        ]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Submodule).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "status.submoduleSummary", "true"),
        ]),
    );
}
