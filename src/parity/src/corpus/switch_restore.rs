//! Differential cases for `switch` and `restore` — the two verbs git split
//! `checkout` into, and the two a daily driver actually types.
//!
//! All three verbs are one file in git (`builtin/checkout.c`): `cmd_switch`,
//! `cmd_restore` and `cmd_checkout` each fill a `checkout_opts` and hand it to
//! the same `checkout_main`. The split is entirely in the *gates* each verb
//! sets before that call, so a port that implements the three separately —
//! which is what `src/extensions/src/porcelain/{switch,restore,checkout}.rs`
//! do — has three chances to lose a gate that `checkout` never had:
//!
//! * `switch` sets `opts.accept_pathspec = 0`, so a second operand is not a
//!   path but an error (`only one reference expected`).
//! * `switch` sets `opts.switch_branch_doing_nothing_is_ok = 0` and calls
//!   `die_if_switching_to_a_branch_while_merging()`, so an unresolved merge or
//!   bisect refuses the switch outright — `checkout` happily walks away from
//!   one.
//! * `restore` sets `opts.overlay_mode = 0` (`checkout`'s is 1), which is why
//!   `restore --source=<rev> <path>` *removes* index entries the source does
//!   not carry and `checkout <rev> -- <path>` never does.
//!
//! # What the probes can and cannot see here
//!
//! The runner compares stdout, exit code and the post-command state probe
//! (`status --porcelain=v1 -uall`, `for-each-ref`, `rev-parse`, `ls-files
//! --stage`, `stash list`, `cat-file --batch-all-objects`, `config --list
//! --local`). Three of those carry most of the weight for these verbs:
//!
//! * **stdout** is where the whole "M/D/A <path>" listing goes — git's
//!   `show_local_changes()` writes the carried-over local modifications there,
//!   and *only* those, after the checkout and before `setup_tracking()`'s
//!   `branch '<x>' set up to track '<y>'.`. Both the contents of that list and
//!   its position relative to the tracking line are compared, so a port that
//!   lists paths git does not, or prints the tracking line first, fails on
//!   stdout alone while agreeing on every byte of state.
//! * **`config --list --local`** is the only witness for `--track`: a port that
//!   reports `branch 'x' set up to track 'origin/y'` and writes nothing, or
//!   writes `branch.x.remote=.` where git writes `origin`, is invisible
//!   everywhere else.
//! * **the state probe** is the only witness for the gates: a refusal that
//!   agrees on exit code but leaves `HEAD` somewhere else, or a force that
//!   reports success and discards nothing, differs only there. It carries the
//!   reflog too, which is where `switch` writes three strings a port has to
//!   match exactly — `checkout: moving from <40-hex> to <name>` (the *full*
//!   object id, never an abbreviation), `branch: Reset to <start-point>` for
//!   `-C` against an existing branch as against `branch: Created from` for a
//!   new one, and nothing at all on `HEAD` for `--orphan`.
//!
//! stderr is compared only where a case says [`Case::strict`]. For these verbs
//! the refusal message *is* the behaviour, so every refusal below is strict —
//! except the two whose text embeds an absolute worktree path
//! (`'<branch>' is already used by worktree at '<abs path>'`), which cannot be
//! byte-compared because the two sides run in different fixture directories.
//! Those are pinned by exit code and by `HEAD` not moving.
//!
//! # Fixture constraints this corpus works around
//!
//! * **No remote-only branch exists anywhere.** [`Shape::BehindRemote`] carries
//!   `origin/main` and `origin/div`, and *both* have local counterparts, so
//!   `switch <name>`'s DWIM (`--guess`, `checkout.guess`,
//!   `checkout.defaultRemote`) can never find a unique remote branch to create
//!   from. A case is one argv against a pristine copy and cannot delete a local
//!   branch first, so the DWIM *success* path is unmeasurable by this harness;
//!   what is measured below is the lookup itself running and finding nothing,
//!   which is where the `.lock` defect lived.
//! * **Worktree file content is not probed.** `--conflict={merge,diff3,zdiff3}`
//!   changes only the conflict markers written into the file; the index stages,
//!   the refs and the object set are identical for all three. Those cases pin
//!   that the option is accepted and does not perturb anything else — the
//!   marker text itself is out of this harness's reach.
//! * **No unborn-HEAD shape.** `restore --staged` against a repository with no
//!   `HEAD` (git falls back to the empty tree) has no fixture, so it is not
//!   covered here.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    switch_branch(out);
    switch_create(out);
    switch_tracking(out);
    switch_guess(out);
    switch_gates(out);
    switch_merge_carry(out);
    switch_while_merging(out);
    switch_other_worktree(out);
    switch_recurse_and_sparse(out);
    switch_refusals(out);
    restore_index_worktree(out);
    restore_source(out);
    restore_overlay(out);
    restore_unmerged(out);
    restore_pathspec(out);
    restore_sparse(out);
    restore_refusals(out);
    lock_suffix_dwim(out);
}

/// Plain branch movement, and the two reports that come with it.
///
/// git prints nothing on stdout for a switch that carries no local
/// modification: `show_local_changes()` runs a `diff-index` against the *new*
/// `HEAD` with `DIFF_FORMAT_NAME_STATUS` and emits a line only for a path whose
/// worktree content differs from it. Every case here starts from a clean
/// worktree, so stdout must be empty on all of them — a port that echoes the
/// paths it checked out fails the whole group while agreeing on every ref. The
/// same listing is what the sparse case in [`switch_recurse_and_sparse`]
/// checks from the other side: a skip-worktree path is absent from the worktree
/// on purpose and must not be reported as deleted.
///
/// The reflog side is `switch -`: `@{-1}` is resolved from `HEAD`'s reflog, so
/// a port that tracks "previous branch" anywhere else answers a different
/// branch as soon as the reflog and its own bookkeeping disagree.
fn switch_branch(out: &mut Vec<Case>) {
    out.push(Case::new("switch", &["switch", "-q", "feature"], Shape::Branched));
    out.push(Case::new("switch", &["switch", "-"], Shape::Branched));

    // Detaching. `--detach` with no operand detaches in place; with one it
    // takes anything that peels to a commit, which is what makes a tag legal
    // here and illegal without it.
    out.push(Case::new("switch", &["switch", "--detach"], Shape::Branched));
    out.push(Case::new("switch", &["switch", "--detach", "feature"], Shape::Branched));
    out.push(Case::new("switch", &["switch", "--detach", "v0.1.0"], Shape::Branched));
    // Detached advice is an `advice.*` key and a global option, and both have
    // to reach the same place — `--no-advice` is the newer spelling of the
    // per-key config and neither may change what is written.
    out.push(
        Case::new("switch", &["switch", "--detach", "feature"], Shape::Branched)
            .with_config(&[("advice.detachedHead", "false")]),
    );
    out.push(
        Case::new("switch", &["switch", "--detach", "feature"], Shape::Branched)
            .with_globals(&[&["--no-advice"]]),
    );
    // Parallel checkout: a threshold of 1 forces the parallel path on a
    // two-file tree, so the option is not merely parsed.
    out.push(
        Case::new("switch", &["switch", "feature"], Shape::Branched)
            .with_config(&[("checkout.workers", "4"), ("checkout.thresholdForParallelism", "1")]),
    );

    // From a detached HEAD back onto a branch. git prints
    // `Previous HEAD position was <abbrev> <subject>` first, which no other
    // shape can produce.
    out.push(Case::strict("switch", &["switch", "main"], Shape::Detached));
    // Run from a subdirectory: the checkout is repository-wide either way.
    out.push(Case::new("switch", &["switch", "feature"], Shape::Branched).in_dir("src"));
}

/// `-c` / `-C` / `--orphan`: the three ways `switch` writes a ref of its own.
///
/// `-C` is what separates a port that special-cases "branch exists" from one
/// that implements git's `-B` semantics: on an existing branch it *resets* it
/// to the start-point and says `Switched to and reset branch`, where `-c`
/// refuses. `for-each-ref` is what proves the reset happened — `feature` ends
/// up on `main`'s commit rather than on its own.
///
/// `--orphan` leaves `HEAD` pointing at an unborn ref, which is the one state
/// in this corpus where `rev-parse HEAD` legitimately fails on both sides.
fn switch_create(out: &mut Vec<Case>) {
    out.push(Case::new("switch", &["switch", "-c", "topic"], Shape::Branched));
    out.push(Case::new("switch", &["switch", "-c", "topic", "feature"], Shape::Branched));
    // Existing name: `-C` resets it, `-c` refuses (in `switch_refusals`).
    out.push(Case::new("switch", &["switch", "-C", "feature"], Shape::Branched));
    // Creating from a detached HEAD is how `switch -c` rescues a detached
    // commit, and the new branch has to sit on the *detached* commit rather
    // than on the branch the shape's `main` points at.
    out.push(Case::new("switch", &["switch", "-c", "topic"], Shape::Detached));

    out.push(Case::new("switch", &["switch", "--orphan", "fresh"], Shape::Branched));
    // An orphan still runs the worktree gate: the unstaged `README.md` edit is
    // a local change the empty tree would overwrite, so git refuses.
    out.push(Case::strict("switch", &["switch", "--orphan", "fresh"], Shape::Dirty));
}

/// `--track` and its three modes, whose only witness is `config --list
/// --local`.
///
/// git's `setup_tracking()` (branch.c) writes `branch.<new>.remote` and
/// `branch.<new>.merge`, and what it writes differs per mode:
///
/// * default / `--track` / `--track=direct` from a remote-tracking start-point
///   writes the *remote's* name and the remote's branch;
/// * `--track=inherit` copies the start-point branch's own upstream, so from a
///   local `main` that tracks `origin/main` the new branch tracks
///   `origin/main` too — not `main`, and not `.`;
/// * `--no-track` writes nothing at all.
///
/// A port that treats `inherit` as "track the start-point" writes
/// `branch.<new>.remote=.` and reports `set up to track 'main'`, which agrees
/// with git on the ref, on `HEAD`, on the object set, and on nothing else.
///
/// `-f` is on the `origin/div` cases because `div`'s tree rewrites `clash.txt`,
/// which the shape holds dirty — without it the checkout gate refuses before
/// the tracking write is ever reached.
fn switch_tracking(out: &mut Vec<Case>) {
    fn br(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("switch", args, Shape::BehindRemote));
    }

    br(out, &["switch", "-c", "mine", "origin/main"]);
    br(out, &["switch", "-c", "mine", "--track=direct", "origin/main"]);
    br(out, &["switch", "-c", "mine", "--no-track", "origin/main"]);
    br(out, &["switch", "-c", "mine", "--track=inherit", "main"]);
    br(out, &["switch", "-f", "-c", "mine", "origin/div"]);
    // A local start-point with no upstream of its own: git's default
    // `branch.autoSetupMerge` is `true`, which means "only from a
    // remote-tracking branch", so this must write nothing.
    br(out, &["switch", "-c", "mine", "main"]);
    // `inherit` from a start-point that has no upstream is a *warning* and a
    // branch with no tracking config, not an error and not a fallback.
    out.push(Case::new("switch", &["switch", "-c", "topic", "--track=inherit", "feature"], Shape::Branched));
    // A remote-tracking ref is not a branch: `switch` refuses it, `--detach`
    // takes it. The pair separates "resolves the ref" from "checks its kind".
    out.push(Case::strict("switch", &["switch", "origin/main"], Shape::BehindRemote));
    br(out, &["switch", "--detach", "origin/main"]);
}

/// `--guess` / `--no-guess` / `checkout.guess` / `checkout.defaultRemote`.
///
/// Every case here resolves to *nothing*: no shape carries a branch that exists
/// on a remote and not locally (see the module header), so the DWIM lookup runs
/// and finds no candidate. That is still the branch of `unique_remote_branch()`
/// that has to survive being handed an arbitrary string — it is where the
/// `.lock` defect lived (see [`lock_suffix_dwim`]) — and it is where the two
/// spellings must agree on the same `invalid reference` refusal.
fn switch_guess(out: &mut Vec<Case>) {
    out.push(Case::strict("switch", &["switch", "--guess", "nosuch"], Shape::BehindRemote));
    out.push(Case::strict("switch", &["switch", "--no-guess", "nosuch"], Shape::BehindRemote));
    out.push(
        Case::strict("switch", &["switch", "nosuch"], Shape::BehindRemote)
            .with_config(&[("checkout.guess", "false")]),
    );
    // `checkout.defaultRemote` only breaks a *tie* between remotes, so with one
    // remote it must change nothing — a port that reads it as "the remote to
    // guess from" starts guessing where git does not.
    out.push(
        Case::strict("switch", &["switch", "nosuch"], Shape::BehindRemote)
            .with_config(&[("checkout.defaultRemote", "origin")]),
    );
}

/// The per-path gates, and what `-f` is allowed to do about them.
///
/// `unpack_trees()`' `twoway_merge` refuses per path, not per repository:
/// `verify_uptodate()` for a tracked path the two trees disagree on and whose
/// worktree copy is dirty, `verify_absent()` for an untracked file sitting
/// where the new tree wants to write. [`Shape::MergeableDirty`] places one of
/// each — `hot.txt` edited and rewritten by `ff-hot`/`div-hot`, `keep.txt`
/// edited and rewritten by nothing, `squat.txt` untracked exactly where
/// `ff-squat` writes — so "refuses when anything is dirty" and "refuses git's
/// subset" score differently.
///
/// `switch -f main` on [`Shape::Dirty`] is the case a port fails by treating
/// "already on that branch" as nothing to do: `--discard-changes` has to reset
/// the worktree and index to `HEAD` even when `HEAD` does not move, so the
/// staged add, the unstaged edit and the deletion all have to be gone
/// afterwards and only the untracked file survives.
fn switch_gates(out: &mut Vec<Case>) {
    fn dirty(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("switch", args, Shape::MergeableDirty));
    }

    // Lands: the branch's footprint and the local edits are disjoint.
    dirty(out, &["switch", "ff-cold"]);
    dirty(out, &["switch", "div-other"]);
    // Refused per path, exit 1, nothing moved.
    out.push(Case::strict("switch", &["switch", "ff-hot"], Shape::MergeableDirty));
    out.push(Case::strict("switch", &["switch", "div-hot"], Shape::MergeableDirty));
    // Refused by `verify_absent()` instead — a different message class.
    out.push(Case::strict("switch", &["switch", "ff-squat"], Shape::MergeableDirty));
    // Forced: `-f`/`--force`/`--discard-changes` are one flag, and it discards
    // the edit the gate refused over. The untracked `squat.txt` survives —
    // nothing on `ff-hot` wants its path.
    dirty(out, &["switch", "-f", "ff-hot"]);

    // A staged change on a path no branch touches: the checkout gate compares
    // the *index* against the new tree, so this is carried through rather than
    // refused, and `status` still reports `M ` afterwards.
    out.push(Case::new("switch", &["switch", "ff-cold"], Shape::MergeableStaged));

    // Same branch, dirty tree: git says `Already on 'main'`, no ref moves, and
    // only `-f` may touch the tree.
    out.push(Case::new("switch", &["switch", "main"], Shape::Dirty));
    out.push(Case::new("switch", &["switch", "-f", "main"], Shape::Dirty));
}

/// `-m` / `--merge`: the local changes are stashed, the branch is switched, and
/// the stash is applied on top.
///
/// git's `merge_working_tree()` falls back to a real three-way when the
/// two-way `unpack_trees()` refuses, and on failure to apply cleanly it leaves
/// the entry in `refs/stash` and says so. That makes this the one `switch` path
/// with an observable object footprint: the stash commit, its tree and the
/// dirty blobs all show up in `cat-file --batch-all-objects`, and `refs/stash`
/// shows up in `for-each-ref` — so a port that "merges" by discarding, or by
/// stashing and never recording the ref, fails on state even when the working
/// tree looks right.
///
/// `--conflict=<style>` only changes the marker text written into the
/// conflicted file, which this harness does not read; the `diff3` case pins
/// that the option is accepted and perturbs neither the index stages nor the
/// object set.
fn switch_merge_carry(out: &mut Vec<Case>) {
    fn m(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("switch", args, Shape::MergeableDirty));
    }

    m(out, &["switch", "-m", "ff-hot"]);
    m(out, &["switch", "--merge", "--conflict=diff3", "ff-hot"]);
    // `-m` onto the branch already checked out: no tree changes, but git still
    // runs the listing, so stdout carries the full `M`/`D`/`A` set — the one
    // shape where the correct answer is *not* empty.
    out.push(Case::new("switch", &["switch", "-m", "main"], Shape::Dirty));
}

/// The merge-in-progress gate, which is `switch`'s alone.
///
/// `cmd_switch` calls `die_if_switching_to_a_branch_while_merging()`
/// (builtin/checkout.c) before anything else: with `MERGE_HEAD` present the
/// verb refuses with exit 128 and `fatal: cannot switch branch while merging`,
/// and **`-f` does not lift it** — force is about overwriting worktree
/// content, not about abandoning a merge. `checkout` has no such gate, so a
/// port that routes `switch` through its `checkout` implementation silently
/// walks away from the conflicted merge instead: the conflicted index is left
/// behind on a different branch, or `-f` throws it away entirely.
///
/// Every form is covered because each takes a different route into
/// `checkout_main` — a branch operand, a forced one, a merging one, a new
/// branch, a detach — and git gates all five at the same place.
fn switch_while_merging(out: &mut Vec<Case>) {
    out.push(Case::strict("switch", &["switch", "theirs"], Shape::Conflicted));
    out.push(Case::strict("switch", &["switch", "-f", "theirs"], Shape::Conflicted));
    out.push(Case::strict("switch", &["switch", "-m", "theirs"], Shape::Conflicted));
    out.push(Case::strict("switch", &["switch", "-c", "topic"], Shape::Conflicted));
    out.push(Case::strict("switch", &["switch", "--detach", "HEAD"], Shape::Conflicted));
}

/// The other-worktree gate, and the flag that lifts it.
///
/// A branch checked out in another worktree may not be checked out again:
/// `die_if_checked_out()` walks `.git/worktrees/*/HEAD` and refuses with exit
/// 128. [`Shape::Worktree`] has `linked` checked out in `wt/` and `main` in the
/// main worktree, so the refusal is reachable from both directions —
/// `switch linked` from the top, `switch main` from inside `wt/`. A port that
/// reads only its own `HEAD` reports success and leaves two worktrees sharing
/// one branch, with the second one's index and worktree now describing a commit
/// its `HEAD` no longer names.
///
/// Not strict: git's message embeds the absolute path of the other worktree,
/// and the two sides run in different fixture directories, so the bytes cannot
/// match by construction. Exit code plus `rev-parse --abbrev-ref HEAD` carry
/// the assertion instead.
fn switch_other_worktree(out: &mut Vec<Case>) {
    out.push(Case::new("switch", &["switch", "linked"], Shape::Worktree));
    out.push(Case::new("switch", &["switch", "main"], Shape::Worktree).in_dir("wt"));
    // The escape hatch: same argv plus the flag, and the switch has to land.
    out.push(Case::new("switch", &["switch", "--ignore-other-worktrees", "linked"], Shape::Worktree));
    // `--detach` peels the branch to a commit, so the gate does not apply:
    // detaching at a branch another worktree holds is legal.
    out.push(Case::new("switch", &["switch", "--detach", "linked"], Shape::Worktree));
}

/// `--recurse-submodules` and the sparse cone.
///
/// The submodule shape has one branch, so what these measure is the *option's*
/// effect on the parent checkout and on `.gitmodules`/`sub` in the index —
/// `submodule.recurse` is the config form of the same switch and has to reach
/// the same place.
///
/// On [`Shape::Sparse`], `unpack_trees()` skips every entry carrying
/// `CE_SKIP_WORKTREE`, so a switch must not materialize `outside/` and must not
/// report those paths as changed either. A port that lists what it wrote
/// without consulting the skip-worktree bit prints `D outside/drop.txt` for a
/// file it never had.
fn switch_recurse_and_sparse(out: &mut Vec<Case>) {
    out.push(Case::new("switch", &["switch", "--recurse-submodules", "main"], Shape::Submodule));
    out.push(Case::new("switch", &["switch", "--no-recurse-submodules", "-c", "topic"], Shape::Submodule));
    out.push(
        Case::new("switch", &["switch", "-c", "topic"], Shape::Submodule)
            .with_config(&[("submodule.recurse", "true")]),
    );

    out.push(Case::new("switch", &["switch", "--detach", "HEAD"], Shape::Sparse));
}

/// The refusals `switch` owes its callers, byte for byte.
///
/// `switch` sets `opts.accept_pathspec = 0`, so it has no pathspec at all: a
/// second operand is `only one reference expected` and an operand after `--` is
/// still parsed as a *reference*, not as a path. That is the single largest
/// behavioural difference from `checkout` and the one most likely to be lost by
/// a port that shares an argument parser between them.
///
/// The `a branch is expected, got <kind> '<name>'` family is a second: git
/// names the kind it found (tag, commit, remote branch) and appends the
/// `--detach` hint, so an implementation that answers `invalid reference` for
/// anything it will not switch to has collapsed four diagnostics into one.
fn switch_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("switch", &["switch", "nosuch"], Shape::Branched));
    out.push(Case::strict("switch", &["switch", "v0.1.0"], Shape::Branched));
    out.push(Case::strict("switch", &["switch", "feature^"], Shape::Branched));
    out.push(Case::strict("switch", &["switch", "-c", "feature"], Shape::Branched));
    out.push(Case::strict("switch", &["switch", "--orphan", "fresh", "feature"], Shape::Branched));
    // The pathspec refusals.
    out.push(Case::strict("switch", &["switch", "main", "--", "README.md"], Shape::Branched));
    out.push(Case::strict("switch", &["switch", "--", "README.md"], Shape::Branched));
}

/// `restore`'s two targets, and the fact that both are opt-in-able separately.
///
/// `--worktree` is the default, `--staged` alone rewrites the index from `HEAD`
/// and leaves the file alone, and both together do each from the same source.
/// [`Shape::Dirty`] carries one of every disagreement — `README.md` modified
/// unstaged, `staged.txt` added to the index only, `src/lib.rs` deleted from
/// the worktree — so each flag combination lands on a different subset and the
/// `status` probe separates them.
///
/// `-SW` is the bundled spelling of `--staged --worktree`. git's
/// `parse-options` folds any run of short flags that take no argument into one
/// token; a hand-rolled parser that matches whole argv tokens accepts
/// `--staged --worktree` and rejects `-SW` with `unknown switch`, which is a
/// divergence in exit code rather than only in wording.
fn restore_index_worktree(out: &mut Vec<Case>) {
    fn d(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("restore", args, Shape::Dirty));
    }

    d(out, &["restore", "--staged", "--worktree", "staged.txt"]);
    d(out, &["restore", "-SW", "README.md"]);
    d(out, &["restore", "."]);
    d(out, &["restore", "--", "README.md"]);
    // A worktree deletion: restoring brings the file back; restoring the index
    // entry alone leaves it deleted, because the entry already matches `HEAD`.
    d(out, &["restore", "src/lib.rs"]);
    // Reporting flags, which must not change what is written.
    // From a subdirectory, naming a path above it.
    out.push(Case::new("restore", &["restore", "../README.md"], Shape::Dirty).in_dir("src"));
    // `-p` cannot be driven interactively here, but its non-interactive
    // half — building the diff and printing the first prompt before stdin
    // reaches EOF — is deterministic and is what a port has to reproduce
    // before any hunk selection matters.
    d(out, &["restore", "--patch", "README.md"]);
}

/// `--source=<tree-ish>`: the half of `restore` that reads from somewhere other
/// than `HEAD`.
///
/// The axis is what the argument is allowed to be — a commit, a relative
/// revision, a branch, an annotated tag, an explicit tree — because
/// `--source` is resolved with `get_oid()` and then `parse_tree()`, so a port
/// that accepts only a commit-ish fails on `HEAD^{tree}` alone.
///
/// With `--staged --worktree` the restored content lands in both, which is what
/// makes the result visible as `M ` (index differs from `HEAD`, worktree
/// matches index) rather than ` M`.
fn restore_source(out: &mut Vec<Case>) {
    fn b(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("restore", args, Shape::Branched));
    }

    b(out, &["restore", "--source=HEAD~1", "src/lib.rs"]);
    b(out, &["restore", "--source=HEAD~1", "--staged", "--worktree", "src/lib.rs"]);
    b(out, &["restore", "--source=feature", "--staged", "--worktree", "."]);
    b(out, &["restore", "-s", "HEAD^{tree}", "--staged", "--worktree", "src/lib.rs"]);
    // `--source=HEAD` over a shape whose index disagrees with `HEAD`: the
    // staged add is removed from the index and the file left untracked.
    out.push(Case::new("restore", &["restore", "--source=HEAD", "--", "."], Shape::Dirty));
}

/// Overlay mode, which is the difference between `restore` and `checkout <rev>
/// -- <path>`.
///
/// `cmd_restore` sets `opts.overlay_mode = 0`; `cmd_checkout` leaves it 1.
/// In overlay mode a path the source does not carry is *left alone*; with
/// overlay off it is removed from the index and from the worktree. `--overlay`
/// puts `restore` back into `checkout`'s behaviour, and it is the flag most
/// easily implemented as a no-op, because with the default already being
/// no-overlay the difference only shows on a source that is *missing* paths the
/// index has.
///
/// [`Shape::AwkwardPaths`] is the one shape whose `HEAD~1` is missing files
/// `HEAD` has — four of them, each with a byte pattern that also exercises the
/// quoting path in the probe's `status` output. With `--staged --worktree` the
/// two modes differ in every one of those paths, and the third case in the
/// group — the same argv with neither flag — is what pins that the *default*
/// is `--no-overlay` rather than whatever the port's `checkout` does.
fn restore_overlay(out: &mut Vec<Case>) {
    fn a(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("restore", args, Shape::AwkwardPaths));
    }

    a(out, &["restore", "--source=HEAD~1", "--staged", "--worktree", "--overlay", "."]);
    a(out, &["restore", "--source=HEAD~1", "--staged", "--worktree", "--no-overlay", "."]);
    a(out, &["restore", "--source=HEAD~1", "--staged", "--worktree", "."]);
    // Single awkward paths, so a quoting failure is attributable to one name.
    a(out, &["restore", "--source=HEAD", "--staged", "--worktree", "with space.txt"]);
    // A decomposed name, which macOS hands out of `readdir()` and which git
    // composes before matching (`compat/precompose_utf8.c`).
    out.push(Case::new("restore", &["restore", "e\u{301}.txt"], Shape::DecomposedPaths));
}

/// Unmerged entries: `--ours`, `--theirs`, `--merge`, `--ignore-unmerged`.
///
/// With a conflicted index, plain `restore <path>` cannot decide which stage to
/// write and refuses with `error: path '<p>' is unmerged` (exit 1);
/// `--ignore-unmerged` downgrades exactly that to a warning and exits 0.
/// `--ours`/`--theirs` write stage 2/3 to the worktree and leave the index
/// unmerged — `ls-files --stage` still showing three stages afterwards is the
/// assertion — while `--staged` collapses the entry back to `HEAD` and
/// `--staged --worktree` resolves the path entirely.
///
/// `--merge` re-runs the three-way and writes a fresh conflicted blob, which is
/// visible as one extra object in `cat-file --batch-all-objects` and nowhere
/// else; the marker style `--conflict=` selects is not visible to this harness.
fn restore_unmerged(out: &mut Vec<Case>) {
    fn c(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("restore", args, Shape::Conflicted));
    }

    c(out, &["restore", "--ours", "conflict.txt"]);
    c(out, &["restore", "--theirs", "conflict.txt"]);
    c(out, &["restore", "--merge", "conflict.txt"]);
    c(out, &["restore", "--conflict=zdiff3", "conflict.txt"]);
    c(out, &["restore", "--staged", "conflict.txt"]);
    c(out, &["restore", "--staged", "--worktree", "conflict.txt"]);
    out.push(Case::strict("restore", &["restore", "--ignore-unmerged", "."], Shape::Conflicted));
}

/// Pathspec handling: magic prefixes, exclusions, and `--pathspec-from-file`.
///
/// `restore` runs its operands through `parse_pathspec()` with
/// `PATHSPEC_PREFER_FULL`, so every magic form git supports is legal here.
/// Two of them change *what is written* rather than only what is matched:
/// `:!<path>` removes a path from the set, and `:(icase)` matches a name whose
/// case differs from the index's. A port that treats an operand as a plain
/// string quietly restores the path the caller excluded.
///
/// `--pathspec-from-file=-` reads the set from stdin; with
/// `--pathspec-file-nul` the separator is NUL and quoting is off, which is the
/// only form that can carry a path containing a newline. Both are fed the same
/// two paths so the two parsers can be compared against one outcome.
fn restore_pathspec(out: &mut Vec<Case>) {
    out.push(Case::new(
        "restore",
        &["restore", "--source=HEAD~1", "--staged", ":(glob)**/*.txt"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "restore",
        &["restore", "--source=HEAD~1", "--staged", "--worktree", ":(icase)README.MD"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "restore",
        &["restore", "--source=HEAD~1", "--staged", ".", ":!nested/deep/path.txt"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new("restore", &["restore", ":(top)README.md"], Shape::Dirty).in_dir("src"));

    out.push(Case::with_stdin(
        "restore",
        &["restore", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"README.md\nsrc/lib.rs\n",
    ));
    out.push(Case::with_stdin(
        "restore",
        &["restore", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::Dirty,
        b"README.md\0src/lib.rs\0",
    ));
}

/// Sparse checkouts, where a pathspec is limited to the cone unless told
/// otherwise.
///
/// `restore` calls `pathspec_needs_expanded_index()`/`ce_skip_worktree()` and
/// silently drops entries outside the cone, so naming an excluded path is
/// `error: pathspec '<p>' did not match any file(s) known to git` even though
/// the entry is right there in the index. `--ignore-skip-worktree-bits` is the
/// flag that lifts the limit — it makes the same argv exit 0 — and a port that
/// parses it without wiring it into the pathspec match answers the refusal
/// either way, which is a divergence in exit code, not only in wording.
fn restore_sparse(out: &mut Vec<Case>) {
    fn s(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("restore", args, Shape::Sparse));
    }

    out.push(Case::strict("restore", &["restore", "outside/drop.txt"], Shape::Sparse));
    s(out, &["restore", "--ignore-skip-worktree-bits", "outside/drop.txt"]);
    s(out, &["restore", "--ignore-skip-worktree-bits", "--staged", "--source=HEAD", "outside/drop.txt"]);
    // Submodule paths: a gitlink entry restored through both targets.
    out.push(Case::new(
        "restore",
        &["restore", "--recurse-submodules", "--source=HEAD", "--staged", "--worktree", "sub"],
        Shape::Submodule,
    ));
}

/// `restore`'s refusals, byte for byte.
///
/// Three distinct exits share one verb and must not be collapsed: a pathspec
/// that matches nothing is `error: … did not match any file(s) known to git`
/// at exit 1, an unresolvable `--source` is `fatal: could not resolve '<rev>'`
/// at 128, and no pathspec at all is `fatal: you must specify path(s) to
/// restore` at 128. An untracked file is the same refusal as a nonexistent one,
/// because `restore` matches against the index, not the filesystem — a port
/// that stats the path first accepts it and writes nothing.
fn restore_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("restore", &["restore"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "nosuch.txt"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "untracked.txt"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "nosuch/*"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "--source=nosuchrev", "README.md"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "."], Shape::Conflicted));
}

/// An argument that is a legal *path* and an illegal *ref name*.
///
/// Every one of the three verbs hands its first operand to a
/// remote-tracking-branch lookup before deciding it is a path: `switch` and
/// `checkout` for DWIM (`unique_remote_branch()` in
/// `src/extensions/src/porcelain/{switch,checkout}.rs`), building
/// `refs/remotes/<remote>/<arg>` for each configured remote. A name ending in
/// `.lock` — `Cargo.lock`, the single most common argument to
/// `git restore` in a Rust tree — is one git could never have created a ref
/// under, and `gix::validate::reference::name` reports that as an `Err` rather
/// than as "no such ref". Propagating it kills the command on an argument that
/// should simply have fallen through to the pathspec path.
///
/// [`Shape::BehindRemote`] is the only shape with a remote configured, and the
/// loop body never runs without one — so on any other shape these cases would
/// pass no matter what the lookup does.
fn lock_suffix_dwim(out: &mut Vec<Case>) {
    out.push(Case::strict("restore", &["restore", "Cargo.lock"], Shape::BehindRemote));
    out.push(Case::strict(
        "restore",
        &["restore", "--source=HEAD", "--staged", "--worktree", "Cargo.lock"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict("switch", &["switch", "Cargo.lock"], Shape::BehindRemote));
    out.push(Case::strict("switch", &["switch", "--guess", "Cargo.lock"], Shape::BehindRemote));
    // `checkout` reaches the same lookup from its own copy of the DWIM, and is
    // the verb where a `.lock` argument is a *pathspec* rather than a ref.
    out.push(Case::strict("checkout", &["checkout", "Cargo.lock"], Shape::BehindRemote));
    out.push(Case::strict("checkout", &["checkout", "--", "Cargo.lock"], Shape::BehindRemote));
}
