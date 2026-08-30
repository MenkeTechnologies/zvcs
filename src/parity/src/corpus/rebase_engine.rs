//! Differential corpus cases for `rebase`'s **engine**: the backend that
//! replays the commits, the todo list it replays them from, and the flags that
//! rewrite more than one branch at a time.
//!
//! Every case here is one invocation against a pristine fixture, compared
//! against stock git 2.55.0 for stdout, exit code and post-command repository
//! state — which for a rebase means `runner::probe_state`'s `for-each-ref` /
//! `rev-parse HEAD` / `cat-file --batch-all-objects` lines and, above all,
//! `probe_op_state`: the whole of `.git/rebase-merge/` and `.git/rebase-apply/`
//! read back file by file. The fixture pins identity and both dates
//! (`env::FIXED_DATE`), so a rebase's rewritten commit ids are reproducible and
//! are a legitimate assertion target — including the *merge* commits
//! `--rebase-merges` builds, which were checked for byte-stability across two
//! runs before being asserted on (see "What could not be measured" below for the
//! one flag that failed that check).
//!
//! # The territory, and the four modules this one is written around
//!
//! `rebase` already had ~94 cases before this module and they cover the resume
//! verbs and the dirty-tree gates. Read these four headers before adding here;
//! nothing below re-files a case any of them owns.
//!
//! * [`super::sequences`] owns **every path that needs a second invocation** —
//!   `--continue`, `--skip`, `--abort`, resolve-then-continue, autostash
//!   restore-on-abort, and the `Shape::Whitespace` dirty-tree
//!   refuse-then-restore workflow. It also owns
//!   `--reapply-cherry-picks --empty=keep` on [`Shape::Cherry`], which is why
//!   that exact pair is absent here and `--empty=keep` is reached through
//!   `-i` and through `--onto` instead. This module never issues a resume verb:
//!   a case that stops mid-rebase asserts on the *stopped state* and stops
//!   there.
//! * [`crate::nested`] owns the flag *interactions* through two matrices, both
//!   on [`Shape::MergeableDirty`]: `REBASE_DIRTY` (autostash × how the replay
//!   target is named) and `REBASE_STATE` (the five verbs spoken to a rebase
//!   that is not running). [`Shape::MergeableDirty`] therefore does not appear
//!   below at all, and neither does a bare `--continue` / `--abort` / `--skip` /
//!   `--quit` / `--edit-todo`.
//! * [`super::history_rewrite`] owns `rebase` on [`Shape::Branched`],
//!   [`Shape::Detached`] and the plain [`Shape::Merged`] forms, plus the
//!   `sequence.editor`-versus-environment precedence case. [`Shape::Branched`]
//!   is therefore absent here entirely, and the [`Shape::Merged`] cases below
//!   are all backend- or engine-qualified spellings of ranges it reaches
//!   unqualified.
//! * [`super::merge_strategies`] owns *which program* a three-way merge is
//!   handed to and what it does with two merge bases. The `-X` / `--strategy`
//!   cases here ask only whether rebase **passes them through** to its backend
//!   and refuses them on the backend that cannot take them; they name only
//!   strategies git itself ships (`ort`, `resolve`), never a name that would
//!   send either binary looking for a `git-merge-<name>` on `PATH`.
//! * [`super::fixture_gaps`] owns the plain forms on [`Shape::Cherry`]
//!   (`rebase main`, `--reapply-cherry-picks main`, `--keep-base main`,
//!   `--onto main topic~2`, `--no-ff main`), on [`Shape::CrissCross`]
//!   (`rebase cc-right`, `rebase cc-b`) and on [`Shape::Unrelated`].
//!
//! Four shapes carry rebase for the first time here, and they are what makes
//! the engine axes reachable at all: [`Shape::Octopus`] (a four-parent merge
//! with three branch tips inside the rebased range), [`Shape::CommitGraph`] (a
//! two-parent merge with a branch tip inside the range, plus a fork to replay
//! onto), [`Shape::Packed`] (eight commits of linear history past the shared
//! root, so a todo list is long enough for `msgnum`/`end` to run into double
//! digits) and [`Shape::TagChain`].
//!
//! # The axes
//!
//! **The two backends.** `--apply` is `git am` under the hood and `--merge`
//! (the default) is the sequencer. They write *different directories* —
//! `.git/rebase-apply/` versus `.git/rebase-merge/` — print different progress
//! text (`Applying: …` and `Falling back to patching base and 3-way merge…`
//! versus `Rebasing (n/m)`), and reach a different result on the same conflict:
//! `rebase --apply --onto main~1 main` on [`Shape::CrissCross`] stops with
//! `.git/rebase-apply/` holding a patch and a `next`/`last` pair, while the same
//! range under the sequencer stops with `.git/rebase-merge/` holding a todo, a
//! `done`, a `msgnum` and an `end`. A port that has one backend and fakes the
//! other passes nothing here. Twelve flag spellings are rejected outright on the
//! apply backend, in three different wordings; all twelve are pinned with
//! [`Case::strict`],
//! and `-C1` is pinned for the opposite reason — an apply-only option selects
//! the apply backend with no `--apply` on the command line at all.
//!
//! **The todo list.** `env::harden` pins `GIT_SEQUENCE_EDITOR=true`, so `-i`
//! runs the generated list unedited. That is a real, deterministic path and the
//! only way to reach `.git/rebase-merge/git-rebase-todo`, `done`, `msgnum` and
//! `end` — and the generated list is not the same list the non-interactive
//! sequencer runs: `--empty` defaults to `stop` under `-i` and to `drop`
//! without it, so `rebase --onto main topic~2` finishes and
//! `rebase -i --onto main topic~2` stops on the now-empty pick. Both spellings
//! are below.
//!
//! **`--rebase-merges`** generates `label` / `reset` / `merge` commands rather
//! than a flat list of picks, and `=rebase-cousins` versus
//! `=no-rebase-cousins` changes which commits get relaid. It is not cosmetic:
//! on [`Shape::Octopus`], `rebase --rebase-merges main~1` reproduces the
//! existing history byte for byte (`dc580741…`) while
//! `--rebase-merges=rebase-cousins main~1` fast-forwards first and rebuilds the
//! octopus merge, landing on `c31ed12b…`.
//!
//! **`--update-refs`** rewrites every branch pointing into the rebased range.
//! Verified against stock: it moves `oct-a`/`oct-b`/`oct-c` on
//! [`Shape::Octopus`], `cg-side` on [`Shape::CommitGraph`] and `side` on
//! [`Shape::Merged`], and adds `update-ref` lines to the todo list — visible in
//! the stopped [`Shape::CrissCross`] case, whose todo carries
//! `update-ref refs/heads/cc-b` beside its `label`/`merge` commands. This is the
//! flag whose failure silently leaves a branch on an abandoned commit, so both
//! the finished and the stopped forms are here.
//!
//! # `--exec` and the program-naming flags
//!
//! `--exec` runs a program. Every `--exec` below is the closed literal `true` or
//! `false` and nothing else — no substitution, no fixture-derived text, nothing
//! that could block or escape. `--strategy` and `-X` name only `ort`, `resolve`,
//! `ours`, `theirs` and `ignore-space-change`, all of which git resolves
//! internally or ships itself.
//!
//! # What could not be measured, and why it is absent
//!
//! * **`--ignore-date` on any case that creates a commit.** It resets the author
//!   date to the wall clock rather than to the pinned `GIT_AUTHOR_DATE`, so the
//!   resulting commit id moves between runs. Measured on stock 2.55.0: two runs
//!   of `rebase --ignore-date main` on [`Shape::Cherry`] produced
//!   `3eec503e…` and `8f171c78…`, and two runs of
//!   `rebase --apply --ignore-date main` produced `40c619e0…` and `a05694e9…`.
//!   Every other date flag *is* stable — `--committer-date-is-author-date` gave
//!   `0b8cf841…` twice, `--signoff` `24a9ffc0…` twice — and both are used below.
//! * **`reword` / `edit` / `squash` / `fixup` / `drop` / `break`.** They require
//!   an editor to write them into the todo, and `GIT_SEQUENCE_EDITOR` is pinned
//!   to `true`. `--exec` is the one todo verb reachable from argv.
//! * **`--autosquash` doing any squashing.** No fixture shape carries a
//!   `fixup!` or `squash!` commit — checked across all 44 templates — so
//!   `--autosquash` and `--no-autosquash` can only be measured as *todo
//!   generation that must agree*, which is what the cases below assert.
//! * **`--keep-empty` / `--empty=keep` distinguishing themselves.** No shape
//!   carries a commit whose tree equals its parent's — also checked across all
//!   44 templates — so the only empty commit a rebase ever sees is one it makes
//!   itself by replaying an already-upstream patch, which is why every
//!   `--empty` case is on [`Shape::Cherry`].

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    backends(out);
    backend_refusals(out);
    todo_list(out);
    empty_and_cherry_picks(out);
    onto_forms(out);
    rebase_merges(out);
    update_refs(out);
    exec_verb(out);
    base_selection(out);
    metadata_and_strategy(out);
    verbosity(out);
    argument_refusals(out);
}

/// One case, on the shape named.
fn c(shape: Shape, args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::new("rebase", args, shape));
}

/// One case whose stderr is compared byte for byte, because the refusal *is*
/// the contract.
fn s(shape: Shape, args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::strict("rebase", args, shape));
}

// ---------------------------------------------------------------------------
// The two backends over identical work
// ---------------------------------------------------------------------------

/// `--apply` versus `--merge` on the same range, so the two state directories,
/// the two progress vocabularies and the two conflict outcomes are all compared.
///
/// Paired where the pairing pays: three ranges are given to `--apply` and to an
/// explicit `--merge` (explicit because the default spelling is owned
/// elsewhere), so a port that routes both into one implementation shows up as
/// the backend-specific half of the state and the stdout being wrong rather than
/// as a single unattributable diff. The rest of the sequencer side of each pair
/// is reached under `-i` in `todo_list`, which runs the same backend and writes
/// the same directory.
///
/// The two stopped cases are the point of the section. `--apply` on
/// [`Shape::CrissCross`] leaves `.git/rebase-apply/` — the `am` state, with its
/// own `patch`, `next` and `last` — and `--merge` on [`Shape::Renamed`] leaves
/// `.git/rebase-merge/`; neither directory is reachable from the other backend,
/// and `probe_op_state` walks both in full.
fn backends(out: &mut Vec<Case>) {
    // Linear replay with a three-way fallback: the apply backend prints
    // `Using index info to reconstruct a base tree...` and
    // `Falling back to patching base and 3-way merge...`, which the sequencer
    // never says.
    c(Shape::Cherry, &["rebase", "--apply", "main"], out);
    c(Shape::Cherry, &["rebase", "--merge", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "main", "topic"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--onto", "main", "topic~2"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--onto", "main~2", "topic~1"], out);

    // A merge in the replayed range, flattened: without `--rebase-merges` both
    // backends drop the octopus merge and replay its three side commits.
    c(Shape::Octopus, &["rebase", "--apply", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--apply", "--onto", "oct-side", "main~1", "main"], out);
    c(Shape::CommitGraph, &["rebase", "--apply", "cg-loose"], out);
    c(Shape::Merged, &["rebase", "--apply", "--onto", "main~2", "main~1"], out);
    c(Shape::Merged, &["rebase", "--merge", "--onto", "main~2", "main~1"], out);

    // Seven commits of linear history: the longest todo list any shape can
    // produce, so `msgnum`/`end` count past single digits.
    c(Shape::Packed, &["rebase", "--apply", "--onto", "main~6", "main~5"], out);

    // The two stopped states, one per backend.
    c(Shape::CrissCross, &["rebase", "--apply", "cc-b"], out);
    c(Shape::CrissCross, &["rebase", "--apply", "--onto", "main~1", "main"], out);
    c(Shape::Renamed, &["rebase", "--apply", "--onto", "main~4", "main~3"], out);
    c(Shape::Renamed, &["rebase", "--merge", "--onto", "main~4", "main~3"], out);

    // The apply backend's own options, which *select* it: `-C1` and
    // `--whitespace` are `git am` options and reach the am path with no
    // `--apply` on the command line.
    c(Shape::Cherry, &["rebase", "-C1", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "-C1", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--whitespace=fix", "main"], out);

    // The apply backend's *dirty-tree* gate and its autostash, which the
    // sequences on this shape reach only through the merge backend.
    c(Shape::Whitespace, &["rebase", "--apply", "--autostash", "--onto", "main~4", "main~3"], out);
    c(Shape::Whitespace, &["rebase", "--apply", "--no-autostash", "--onto", "main~4", "main~3"], out);

    // `--root` with an explicit `--onto` is the one root rebase the apply
    // backend accepts; without `--onto` it refuses (see `backend_refusals`).
    c(Shape::Linear, &["rebase", "--apply", "--onto", "HEAD", "--root"], out);
}

/// The twelve flags that refuse on the apply backend, each with its own message.
///
/// Every one of these is a *pre-flight* refusal: git decides the flag needs the
/// sequencer and dies at 128 before touching a ref, so the contract is the
/// stderr line and the fact that nothing moved. Three distinct messages hide in
/// here — `--<flag> requires the merge backend`,
/// `apply options and merge options cannot be used together` (for `-i` and for
/// `--merge` itself, where the conflict is between option *classes* rather than
/// with one named flag), and `--root without --onto requires the merge backend`.
/// A port that emits one wording for all twelve still exits 128 on every one of
/// them, which is why the whole set is strict: the exit code alone cannot tell
/// the three apart.
fn backend_refusals(out: &mut Vec<Case>) {
    for flag in [
        &["--rebase-merges"][..],
        &["--update-refs"],
        &["--empty=keep"],
        &["--reapply-cherry-picks"],
        &["--keep-empty"],
        &["--exec", "true"],
        &["--autosquash"],
        &["-X", "ours"],
        &["--strategy=ort"],
        &["-i"],
        &["--merge"],
    ] {
        let mut args = vec!["rebase", "--apply"];
        args.extend_from_slice(flag);
        args.push("main");
        s(Shape::Cherry, &args, out);
    }
    s(Shape::Cherry, &["rebase", "--apply", "--root"], out);
}

// ---------------------------------------------------------------------------
// The generated todo list
// ---------------------------------------------------------------------------

/// `-i` with the pinned `true` sequence editor: the generated list, run
/// unedited.
///
/// Two things separate this from the same argv without `-i`, and both are
/// asserted below. The list is *written to disk* — `.git/rebase-merge/` exists
/// for the whole run and survives a stop, so a case that stops has its
/// `git-rebase-todo`, `done`, `msgnum`, `end` and `git-rebase-todo.backup`
/// compared line for line. And the list is *generated differently*: `--empty`
/// defaults to `stop` under `-i` and to `drop` without it, which is why
/// `--onto main topic~2` finishes here and stops there.
fn todo_list(out: &mut Vec<Case>) {
    c(Shape::Renamed, &["rebase", "-i", "--onto", "main~4", "main~3"], out);
    c(Shape::Packed, &["rebase", "-i", "--onto", "main~6", "main~5"], out);
    c(Shape::Packed, &["rebase", "-i", "--exec", "true", "--onto", "main~6", "main~5"], out);
    c(Shape::Octopus, &["rebase", "-i", "main~1"], out);
    c(Shape::CommitGraph, &["rebase", "-i", "cg-loose"], out);
    c(Shape::Merged, &["rebase", "-i", "--onto", "main~2", "main~1"], out);
    c(Shape::Cherry, &["rebase", "-i", "main", "topic"], out);
    c(Shape::Cherry, &["rebase", "-i", "--exec", "true", "--exec", "true", "main"], out);
    c(Shape::Cherry, &["rebase", "-i", "--keep-base", "main"], out);

    // The `-i`-versus-not `--empty` default, on the one shape that can produce
    // an empty pick. The first stops; `rebase --onto main topic~2` (owned by
    // `fixture_gaps`) does not.
    c(Shape::Cherry, &["rebase", "-i", "--onto", "main", "topic~2"], out);
    c(Shape::Cherry, &["rebase", "-i", "--empty=keep", "main"], out);
    c(Shape::Cherry, &["rebase", "-i", "--reapply-cherry-picks", "--empty=drop", "main"], out);
    c(Shape::Cherry, &["rebase", "-i", "--reapply-cherry-picks", "--empty=stop", "main"], out);

    // Autosquash generates the todo whether or not there is anything to
    // reorder, and no shape carries a `fixup!`/`squash!` subject — so what is
    // measured is that both spellings produce the *same* list git would.
    c(Shape::Cherry, &["rebase", "-i", "--autosquash", "--onto", "main", "topic~2"], out);
    c(Shape::Cherry, &["rebase", "-i", "--no-autosquash", "--onto", "main", "topic~2"], out);

    // A stopped interactive rebase over a criss-cross, where the todo has
    // already been half consumed: `done` holds the picks that ran and
    // `git-rebase-todo` the ones that did not.
    c(Shape::CrissCross, &["rebase", "-i", "--onto", "main~1", "main"], out);

    // Root rebases: the list opens with the root commit rather than with a
    // `reset onto`.
    c(Shape::Renamed, &["rebase", "-i", "--root"], out);
}

// ---------------------------------------------------------------------------
// Empty commits and already-upstream patches
// ---------------------------------------------------------------------------

/// `--empty` / `--keep-empty` / `--reapply-cherry-picks` over
/// [`Shape::Cherry`], the one shape whose topic branch carries a patch that is
/// already on the upstream.
///
/// The two decisions are independent and are crossed here. *Whether the
/// already-applied commit is picked at all* is `--reapply-cherry-picks` —
/// without it git skips the commit before the replay and says so on stderr
/// (`warning: skipped previously applied commit 7a4b88a`), with it the commit
/// is picked and becomes empty. *What happens to the commit that came out
/// empty* is `--empty`: `drop` prints
/// `dropping 7a4b88a… -- patch contents already upstream` on stdout, `keep`
/// commits it, and `stop` halts with a live `.git/rebase-merge/` and the
/// `git commit --allow-empty` advice.
///
/// `--reapply-cherry-picks --empty=keep` is deliberately absent: `sequences`
/// owns that pair.
fn empty_and_cherry_picks(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--empty=drop", "main"], out);
    c(Shape::Cherry, &["rebase", "--empty=keep", "main"], out);
    c(Shape::Cherry, &["rebase", "--empty=stop", "main"], out);
    c(Shape::Cherry, &["rebase", "--reapply-cherry-picks", "--empty=drop", "main"], out);
    c(Shape::Cherry, &["rebase", "--reapply-cherry-picks", "--empty=stop", "main"], out);
    c(Shape::Cherry, &["rebase", "--no-reapply-cherry-picks", "main"], out);
    c(Shape::Cherry, &["rebase", "--keep-empty", "main"], out);
    c(Shape::Cherry, &["rebase", "--empty=keep", "--onto", "main", "topic~2"], out);
    c(Shape::Cherry, &["rebase", "--reapply-cherry-picks", "--onto", "main", "topic~2"], out);
    // Both of topic's own commits are already upstream when the roles are
    // reversed, so the whole replay drops and the branch lands on `topic`.
    c(Shape::Cherry, &["rebase", "--onto", "topic", "main"], out);
}

// ---------------------------------------------------------------------------
// Naming what to replay, and where
// ---------------------------------------------------------------------------

/// Every form of `--onto` and of the two-argument spelling.
///
/// The two-argument `rebase <upstream> <branch>` checks `<branch>` out first,
/// so it moves `HEAD` as well as the branch ref — a port that resolves the
/// range correctly and forgets the checkout leaves `HEAD` where it was and is
/// caught by the `rev-parse --abbrev-ref HEAD` probe rather than by stdout.
///
/// `--onto` is given a branch, a raw object id, a `^{commit}` peel and a `~n`
/// walk, because they enter git through different revision-parsing paths and
/// only the first is the one a port is likely to have implemented.
fn onto_forms(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--onto", "main~2", "topic~1"], out);
    c(Shape::Cherry, &["rebase", "--onto", "main", "--root", "topic"], out);
    c(Shape::Cherry, &["rebase", "--root", "--onto", "main"], out);
    // `main` on the cherry shape, spelled as the raw id the fixture pins it to
    // and as a peel of the name.
    c(Shape::Cherry, &["rebase", "--onto", "b0db3a776e19ea70ad34d49ac80c3ba6a9d7b492", "topic~2"], out);
    c(Shape::Cherry, &["rebase", "--onto", "main^{commit}", "topic~2"], out);
    c(Shape::Octopus, &["rebase", "--onto", "oct-side", "main~1", "main"], out);
    c(Shape::Octopus, &["rebase", "main", "oct-side"], out);
    c(Shape::CommitGraph, &["rebase", "--onto", "cg-loose", "main~5"], out);
    c(Shape::Merged, &["rebase", "main~2", "side"], out);
    c(Shape::TagChain, &["rebase", "--onto", "main~3", "main~2"], out);
    c(Shape::Linear, &["rebase", "--root"], out);
}

// ---------------------------------------------------------------------------
// --rebase-merges: a todo list with topology in it
// ---------------------------------------------------------------------------

/// `--rebase-merges` and its two cousin modes, over the three shapes that have
/// a merge to preserve.
///
/// The generated list stops being flat: it carries `label`, `reset` and `merge`
/// commands, and the merge command re-runs the merge rather than replaying its
/// first-parent side. Two things are asserted. The finished cases assert the
/// *result* — that the rebuilt merge still has all of its parents, which on
/// [`Shape::Octopus`] means four, and which stock reaches by re-running the
/// octopus strategy (`Merge made by the 'octopus' strategy.`). The stopped
/// [`Shape::CrissCross`] case asserts the *list*: its `git-rebase-todo` holds
/// `label cc-b`, `reset onto`, `merge -C 5b52389… cc-b` and a `pick`, its
/// `done` holds the three commands already run, and `end` reads 8.
///
/// `=rebase-cousins` is not cosmetic on [`Shape::Octopus`]: stock takes a
/// different route (`Fast-forwarding to: 1ac6fa1a…` before the octopus merge)
/// and lands on a different commit than `=no-rebase-cousins`.
fn rebase_merges(out: &mut Vec<Case>) {
    c(Shape::Merged, &["rebase", "--rebase-merges", "--onto", "main~2", "main~1"], out);
    c(Shape::Merged, &["rebase", "--rebase-merges=rebase-cousins", "--onto", "main~2", "main~1"], out);
    c(Shape::Merged, &["rebase", "--rebase-merges", "--root"], out);

    c(Shape::Octopus, &["rebase", "--rebase-merges", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges=rebase-cousins", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges=no-rebase-cousins", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "--onto", "oct-side", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "oct-side"], out);

    c(Shape::CrissCross, &["rebase", "--rebase-merges", "main"], out);
    c(Shape::CrissCross, &["rebase", "--rebase-merges=rebase-cousins", "main"], out);
    // Stopped: the one case where the `label`/`merge` todo is read back off
    // disk rather than inferred from the result.
    c(Shape::CrissCross, &["rebase", "--rebase-merges", "--onto", "main~1", "main"], out);

    c(Shape::CommitGraph, &["rebase", "--rebase-merges", "cg-loose"], out);
    c(Shape::CommitGraph, &["rebase", "--rebase-merges=rebase-cousins", "cg-loose"], out);
    c(Shape::CommitGraph, &["rebase", "--rebase-merges", "--onto", "cg-loose", "main~5"], out);

    // A root rebase onto an unrelated root, with the merge topology preserved.
    c(Shape::Unrelated, &["rebase", "--rebase-merges", "--onto", "alien", "--root", "main"], out);

    // The negation, which must produce the flat list.
    c(Shape::Cherry, &["rebase", "--no-rebase-merges", "main"], out);
}

// ---------------------------------------------------------------------------
// --update-refs: the branches that move with the rebase
// ---------------------------------------------------------------------------

/// `--update-refs`, the flag whose failure is silent.
///
/// A rebase abandons every commit it replays. Any branch that pointed at one of
/// those commits is left behind on an object nothing reaches any more, and
/// nothing in the rebase's own output says so — which is why this section is
/// separated out. Stock's contract, verified per case: after
/// `rebase --update-refs main~1` on [`Shape::Octopus`], `oct-a`, `oct-b` and
/// `oct-c` all point at replayed commits and stderr lists them under
/// `Updated the following refs with --update-refs:`; after
/// `rebase --update-refs --onto main~2 main~1` on [`Shape::Merged`], `side`
/// does. The `for-each-ref` probe is what catches the failure, so a port that
/// prints the list and moves nothing fails on state rather than on stdout.
///
/// The flag also changes the *todo*: `update-ref refs/heads/<name>` lines are
/// inserted after the pick that the branch pointed at, and `end` grows by one
/// per moved ref (8 → 10 on the stopped [`Shape::CrissCross`] case). Both the
/// finished and the stopped forms are here for that reason.
fn update_refs(out: &mut Vec<Case>) {
    c(Shape::Octopus, &["rebase", "--update-refs", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--no-update-refs", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--update-refs", "--onto", "oct-side", "main~1"], out);
    c(Shape::Octopus, &["rebase", "-i", "--update-refs", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "--update-refs", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "--update-refs", "oct-side"], out);
    c(Shape::Octopus, &["rebase", "--update-refs", "--rebase-merges", "--keep-base", "main~1"], out);

    c(Shape::CommitGraph, &["rebase", "--update-refs", "cg-loose"], out);
    c(Shape::CommitGraph, &["rebase", "--rebase-merges", "--update-refs", "--onto", "cg-loose", "main~5"], out);

    c(Shape::Merged, &["rebase", "--update-refs", "--onto", "main~2", "main~1"], out);
    c(Shape::Merged, &["rebase", "--rebase-merges", "--update-refs", "--onto", "main~2", "main~1"], out);

    // Stopped, with `update-ref` lines still ahead of the cursor in the todo and
    // a `.git/rebase-merge/update-refs` file recording what is owed.
    c(Shape::CrissCross, &["rebase", "--update-refs", "main"], out);
    c(Shape::CrissCross, &["rebase", "--rebase-merges", "--update-refs", "--onto", "main~1", "main"], out);

    c(Shape::TagChain, &["rebase", "--update-refs", "--onto", "main~3", "main~2"], out);
}

// ---------------------------------------------------------------------------
// --exec
// ---------------------------------------------------------------------------

/// `--exec`, the one todo verb reachable without an editor.
///
/// Both commands are closed literals. `true` inserts an `exec true` after every
/// pick and the rebase finishes; `false` stops the rebase at the first exec with
/// `warning: execution failed: false` and a live `.git/rebase-merge/` whose
/// `done` ends with the failed `exec` line — the one way a stopped rebase is
/// reached without a content conflict, and therefore the one whose stopped state
/// has a *clean* index. Crossed with `--rebase-merges`, which changes where in
/// the list the execs land.
fn exec_verb(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--exec", "true", "main"], out);
    c(Shape::Cherry, &["rebase", "--exec", "false", "main"], out);
    c(Shape::Cherry, &["rebase", "--autosquash", "--exec", "true", "main"], out);
    c(Shape::Octopus, &["rebase", "--exec", "true", "main~1"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "--exec", "true", "main~1"], out);
    c(Shape::Packed, &["rebase", "--exec", "false", "--onto", "main~6", "main~5"], out);
    c(Shape::CommitGraph, &["rebase", "-i", "--exec", "true", "cg-loose"], out);
}

// ---------------------------------------------------------------------------
// Which base the range is computed from
// ---------------------------------------------------------------------------

/// `--fork-point`, `--keep-base` and the forced replay, which change *which
/// commits* end up in the todo rather than what is done with them.
///
/// `--fork-point` consults the upstream's reflog rather than the commit graph,
/// so [`Shape::BehindRemote`] — the one shape with a real remote-tracking ref
/// and an upstream configured — is where it means anything; the
/// `--autostash` is there because that shape's worktree is dirty and every
/// rebase on it otherwise stops at the gate `nested::REBASE_DIRTY` owns.
/// `--keep-base` rebases onto the *merge base* rather than onto the upstream
/// tip, which on [`Shape::Cherry`] turns the whole thing into
/// `Current branch topic is up to date.` and on [`Shape::CommitGraph`] replays
/// five commits onto a base neither branch is at.
fn base_selection(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--fork-point", "main"], out);
    c(Shape::Cherry, &["rebase", "--no-fork-point", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--keep-base", "main"], out);
    c(Shape::Cherry, &["rebase", "--force-rebase", "--keep-base", "main"], out);
    c(Shape::Octopus, &["rebase", "--keep-base", "main~1"], out);
    c(Shape::CommitGraph, &["rebase", "--keep-base", "cg-loose"], out);
    c(Shape::BehindRemote, &["rebase", "--autostash", "--fork-point", "origin/main"], out);
    c(Shape::BehindRemote, &["rebase", "--autostash", "--no-fork-point", "origin/main"], out);
    // No upstream named: the branch's own `branch.main.merge` supplies it, and
    // `--fork-point` is the default in that spelling.
    c(Shape::BehindRemote, &["rebase", "--autostash", "--fork-point"], out);
    c(Shape::BehindRemote, &["rebase", "--autostash", "--keep-base", "origin/main"], out);
}

// ---------------------------------------------------------------------------
// What the replayed commits say, and who merges them
// ---------------------------------------------------------------------------

/// The flags that change the replayed commit's metadata, and the pass-through
/// of `--strategy` / `-X` to whichever backend is running.
///
/// Both date flags below were checked for byte-stability across two runs before
/// being asserted on — `--committer-date-is-author-date` and `--signoff` are
/// stable, `--ignore-date` is not and is therefore absent (see the module
/// header). `--signoff` produced the same commit id on both backends, which is
/// itself the assertion: a trailer appended by two different code paths must
/// come out identical.
///
/// `--strategy=resolve` makes the sequencer run git's own `git-merge-resolve`,
/// whose stdout (`Trying simple merge.`, `Simple merge failed, trying Automatic
/// merge.`) is nothing the default `ort` ever prints — the clearest evidence
/// available from stdout alone that the strategy was actually passed through
/// rather than accepted and ignored.
fn metadata_and_strategy(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--signoff", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--signoff", "main"], out);
    c(Shape::Cherry, &["rebase", "--committer-date-is-author-date", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--committer-date-is-author-date", "main"], out);
    c(Shape::Cherry, &["rebase", "-X", "ours", "main"], out);
    c(Shape::Cherry, &["rebase", "-X", "theirs", "main"], out);
    c(Shape::Cherry, &["rebase", "--strategy=resolve", "main"], out);
    c(Shape::Cherry, &["rebase", "--strategy=ort", "-X", "ignore-space-change", "main"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "-X", "ours", "main~1"], out);
}

// ---------------------------------------------------------------------------
// How much the rebase says about itself
// ---------------------------------------------------------------------------

/// `--stat` / `--quiet` / `--verbose`, on both backends.
///
/// `--stat` puts a diffstat of the *whole* range on stdout before the replay
/// starts and `--verbose` puts one there plus one per replayed commit, so these
/// are among the few rebase invocations whose contract is mostly stdout rather
/// than mostly state. `--quiet` on the apply backend prints nothing at all,
/// which is a stronger assertion than it looks: a port that leaks one progress
/// line fails it.
fn verbosity(out: &mut Vec<Case>) {
    c(Shape::Cherry, &["rebase", "--stat", "main"], out);
    c(Shape::Cherry, &["rebase", "--quiet", "main"], out);
    c(Shape::Cherry, &["rebase", "--verbose", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--stat", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--quiet", "main"], out);
    c(Shape::Cherry, &["rebase", "--apply", "--verbose", "main"], out);
    c(Shape::Octopus, &["rebase", "--rebase-merges", "--verbose", "main~1"], out);
}

// ---------------------------------------------------------------------------
// Refusals that are the contract
// ---------------------------------------------------------------------------

/// The argument-level refusals, all strict.
///
/// Three different exit codes and three different *layers* refuse here: 128 from
/// rebase's own validation of an enumerated value, 129 from `parse-options`
/// when a flag's argument is missing, and 1 from the branch-has-no-upstream path
/// — which is the only one of the three whose message names a ref from the
/// fixture, so a port that hard-codes the advice text fails it while passing the
/// other two.
///
/// The five ways to speak to a rebase that is not running are absent
/// deliberately: `nested::REBASE_STATE` owns all five.
fn argument_refusals(out: &mut Vec<Case>) {
    s(Shape::Cherry, &["rebase", "--empty=bogus", "main"], out);
    s(Shape::Cherry, &["rebase", "--rebase-merges=bogus", "main"], out);
    s(Shape::Cherry, &["rebase", "--exec"], out);
    s(Shape::Cherry, &["rebase", "--onto"], out);
    s(Shape::Cherry, &["rebase", "--onto", "no-such-ref", "main"], out);
    s(Shape::Cherry, &["rebase", "--onto", "main"], out);
}
