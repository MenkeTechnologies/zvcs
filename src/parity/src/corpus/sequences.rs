//! The multi-step corpus: workflows, run one step at a time against one
//! repository per side and compared after every step.
//!
//! Every other case in this harness is a single invocation against a pristine
//! fixture. Git's stateful operations are not single invocations, and the state
//! that makes `--continue`, `--skip` and `--abort` work — `.git/sequencer/todo`,
//! `.git/rebase-merge/`, `.git/rebase-apply/`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`,
//! `REBASE_HEAD`, `BISECT_*`, `ORIG_HEAD`, the reflog trail — is written *by* one
//! invocation and read *by* the next. A single case can only meet that state
//! pre-built by a fixture, which pins one moment of one operation and leaves
//! every transition unmeasured. See [`crate::runner::Sequence`] for the
//! mechanism, what composes per step versus per case, and the cost.
//!
//! # What these sequences are written to reach
//!
//! Two properties, chosen because they are the ones that historically broke:
//!
//!  * **The stopped state itself.** After the step that conflicts, the state
//!    probe compares `.git/sequencer/todo` verb by verb, the whole of
//!    `.git/rebase-merge/`, and every root state file. A `cherry-pick A B C`
//!    that stops without recording what is left to do scores a state difference
//!    at that step, named, rather than surfacing three commands later as a
//!    mystery.
//!  * **The abort.** An aborted operation must leave the repository *exactly* as
//!    stock leaves it: same HEAD, same index, same worktree, same reflog, and
//!    none of the state files left behind. That is the single most valuable
//!    property here — a `--abort` that half-cleans looks like success and is
//!    only discovered later by whatever trips over the residue — so most
//!    workflows below exist in an abort variant beside their continue variant.
//!
//! # Why the steps do their own setup
//!
//! Several sequences start by putting the fixture into the state they need
//! (`merge --abort` on [`Shape::Conflicted`], `restore .` on
//! [`Shape::Whitespace`], `checkout -b side …`) rather than a shape being added
//! for each. Two reasons. A shape costs every run — it is built once per sweep
//! and copied twice per case that uses it — while a setup step costs only the
//! sequence that needs it. And a setup step is *itself compared*: the sequence
//! stops at the first divergence, so by the time the interesting step runs, the
//! premise it runs on has been proven identical on both sides rather than
//! assumed. A fixture-built premise gives no such proof.
//!
//! # Reproducing one by hand
//!
//! A step id carries its whole script (`::script[…]`), and the `--verbose`
//! failure block prints it one line per step with the failing step marked. Run
//! those argvs in order against a copy of the named shape under
//! `crate::env::harden`'s environment and the failure reproduces.

use crate::fixture::Shape;
use crate::runner::Sequence;

/// The whole curated sequence corpus.
pub fn sequences() -> Vec<Sequence> {
    let mut s = Vec::new();
    cherry_pick(&mut s);
    revert(&mut s);
    rebase(&mut s);
    am(&mut s);
    merge(&mut s);
    bisect(&mut s);
    stash(&mut s);
    sparse_checkout(&mut s);
    worktree(&mut s);
    apply(&mut s);
    notes(&mut s);
    replace(&mut s);
    refs_and_storage(&mut s);
    remote_side(&mut s);
    submodule(&mut s);
    unwind(&mut s);
    s
}

/// A one-hunk diff against [`Shape::Linear`]'s `README.md`, with the pre-image
/// blob id in its `index` line.
///
/// The id is what makes it usable by `apply --3way`: without a resolvable
/// pre-image, the three-way fallback cannot reconstruct a base and the patch
/// either applies verbatim or fails, which is the one path that needs no
/// sequence to reach. `9741694` is the blob for `# fixture\n`, which every shape
/// in this harness starts from (`fixture.rs:build` writes it before the match),
/// so the same payload is a clean apply on `Linear` and a conflicting three-way
/// merge on a shape whose `README.md` has moved on.
const README_DIFF: &[u8] = b"diff --git a/README.md b/README.md\n\
index 9741694..2a1b3c4 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n\
 # fixture\n\
+added line\n";

/// A `update-ref --stdin` transaction that commits: two ref creations inside an
/// explicit `start`/`prepare`/`commit` envelope.
///
/// Written with the envelope spelled out rather than left implicit, because the
/// three keywords each print their own `ok` line and a port that treats them as
/// no-ops produces the right refs with the wrong stdout.
const UPDATE_REF_TXN: &[u8] = b"start\n\
create refs/heads/txn-a HEAD\n\
create refs/heads/txn-b HEAD~1\n\
prepare\n\
commit\n";

/// A transaction that must fail *whole*: a valid delete followed by a create
/// naming a ref that does not exist.
///
/// The delete is first on purpose. It is individually legal, so a port that
/// applies commands as it reads them leaves `refs/heads/txn-a` gone and the
/// repository half-updated — which the `for-each-ref` step after it is what
/// catches.
const UPDATE_REF_BAD_TXN: &[u8] = b"start\n\
delete refs/heads/txn-a\n\
create refs/heads/nope refs/heads/does-not-exist\n\
commit\n";

// ---------------------------------------------------------------------------
// cherry-pick
// ---------------------------------------------------------------------------
//
// [`Shape::Conflicted`] is a repository parked in the middle of a conflicted
// *merge*, so every sequence here starts by clearing it: `merge --abort` leaves
// `main` (`conflict.txt` = "ours") and `theirs` (`conflict.txt` = "theirs") as
// two branches that add the same path with different content, which is an
// add/add conflict for anything that tries to replay one onto the other.
//
// The multi-commit sequences use [`Shape::Whitespace`] instead, because
// `.git/sequencer/todo` is only non-empty while picks remain: a two-commit
// history can only stop on its last pick, and the todo list — the part of the
// sequencer a port is most likely to forget entirely — would be empty every
// time. `Whitespace` has six commits, four of which rewrite `ws/indent.c` —
// `initial` predates the file, `whitespace: seed` creates it, and `whitespace:
// crlf to lf` rewrites `ws/eol.txt` instead (`fixture.rs:595-618`). Picking a
// run of the four onto an older one conflicts at the *first* pick with two more
// still queued, which is the property these sequences need.

fn cherry_pick(out: &mut Vec<Sequence>) {
    // Conflict, resolve with git alone, continue. `checkout --theirs` +`add` is
    // the resolution path a human takes and the only one available to a case,
    // which cannot write a file.
    out.push(
        Sequence::new("cherry-pick", "conflict-resolve-continue", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["cherry-pick", "--continue"])
            .step(&["log", "--oneline"]),
    );

    // The abort. Step 4's post-state must be step 1's post-state plus nothing —
    // no `CHERRY_PICK_HEAD`, no `sequencer/`, no `AUTO_MERGE`, and `main` back
    // where it was.
    out.push(
        Sequence::new("cherry-pick", "conflict-abort", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["cherry-pick", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // Three picks, conflicting on the first, so `.git/sequencer/todo` still
    // holds two. The abort has to undo the whole run, not just the pick that
    // stopped.
    out.push(
        Sequence::new("cherry-pick", "sequencer-todo-abort", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main~4"])
            .step(&["cherry-pick", "main~2", "main~1", "main"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // The abort, cross-examined. Steps 4-6 ask each resumption verb whether an
    // operation is in progress, and every one of them must say no — a `--abort`
    // that leaves `.git/sequencer` or `MERGE_HEAD` behind makes one of them try
    // to *do* something instead of refusing, which is a corrupted repository
    // rather than an error message.
    //
    // `strict`, because for a refusal the message *is* the behaviour: all three
    // steps exit 128 with empty stdout, so the exit code cannot tell "no
    // cherry-pick in progress" from "no rebase in progress" from a die() for an
    // unrelated reason. `--no-advice` globally, because git's hint blocks are
    // advice prose rather than the refusal, and comparing them would make this
    // a test of the advice text.
    out.push(
        Sequence::new("cherry-pick", "abort-then-refuse-resumption", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["cherry-pick", "--abort"])
            .step(&["cherry-pick", "--continue"])
            .step(&["rebase", "--continue"])
            .step(&["merge", "--continue"])
            .step(&["status", "--porcelain"]),
    );

    // The same workflow with the repository named by the environment instead of
    // discovered from the working directory. Every step re-resolves the
    // repository from scratch, so this asks whether the *second* invocation of a
    // multi-step operation finds the same repository the first one wrote to —
    // `setup.c:setup_git_directory_gently_1` takes a different branch entirely
    // when `GIT_DIR` is set, and nothing else in this corpus runs a stateful
    // operation down it.
    out.push(
        Sequence::new("cherry-pick", "conflict-continue-under-git-dir", Shape::Conflicted)
            .with_env(&[("GIT_DIR", "{repo}/.git"), ("GIT_WORK_TREE", "{repo}")])
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["checkout", "--theirs", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["cherry-pick", "--continue"])
            .step(&["log", "--oneline"]),
    );

    // The same stop, walked forward with `--skip`: the skipped pick is dropped,
    // the next one is committed, and the one after that conflicts in turn — so
    // this measures the sequencer *advancing*, which no single case can reach.
    out.push(
        Sequence::new("cherry-pick", "sequencer-todo-skip", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main~4"])
            .step(&["cherry-pick", "main~2", "main~1", "main"])
            .step(&["cherry-pick", "--skip"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--abort"])
            .step(&["log", "--oneline"]),
    );

    // The same stop, walked forward with `--continue`, which is the case
    // `sequencer-todo-skip` above cannot cover: the resolved pick is committed
    // and then the *remaining two* are replayed inside the same invocation, so
    // step 6 alone produces three commits and empties `.git/sequencer`. A port
    // that resumes only the pick it stopped on leaves two picks queued and a
    // clean worktree, which looks like success at step 6 and is caught by the
    // `log` at step 8.
    out.push(
        Sequence::new("cherry-pick", "sequencer-todo-continue-drain", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main~4"])
            .step(&["cherry-pick", "main~2", "main~1", "main"])
            .step(&["checkout", "--theirs", "--", "ws/indent.c"])
            .step(&["add", "ws/indent.c"])
            .step(&["cherry-pick", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // `-n`: apply without committing, then commit by hand. The pick writes
    // `MERGE_MSG` and `AUTO_MERGE` and deliberately does **not** write
    // `CHERRY_PICK_HEAD` — stock leaves no operation in progress at all — so the
    // `commit` at step 5 is an ordinary commit that happens to find a prepared
    // message. A port that records a cherry-pick here makes step 5 produce the
    // wrong parents.
    //
    // Step 5 is `--no-edit` rather than `-m`, so the message comes from
    // `MERGE_MSG` and is part of what is measured: stock's `MERGE_MSG` holds
    // `theirs`, and the `log` at step 6 prints that subject. A `-m` here would
    // have supplied the subject itself and left `MERGE_MSG` unread.
    //
    // `--strategy-option=theirs` because `theirs` and `main` add the same path
    // with different content: without a resolution the pick stops and `-n`
    // measures the conflict path a dozen other sequences already cover.
    out.push(
        Sequence::new("cherry-pick", "no-commit-then-commit", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "-n", "--strategy-option=theirs", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"])
            .step(&["commit", "--no-edit"])
            .step(&["log", "--oneline"]),
    );

    // `--quit`: the third resumption verb, and the one whose contract is the
    // opposite of `--abort`. It drops the sequencer state and leaves the
    // conflicted *index and worktree* exactly where the stop left them — stock's
    // `status` still reports `AA conflict.txt` at step 4 — so a port that
    // implements `--quit` by calling its own `--abort` produces a clean tree and
    // passes every state file check while losing the user's work.
    //
    // The tail is `cherry-pick --abort`, which must *refuse* — `--quit` removed
    // the sequencer, so there is nothing to abort, and a port that left it
    // behind acts here instead of erroring. Stock answers `error: no cherry-pick
    // or revert in progress` / `fatal: cherry-pick failed` and exits 128.
    //
    // `strict`, because that tail is the whole assertion and it exits 128 with
    // empty stdout: without the message a port that refused for an unrelated
    // reason scores the same.
    out.push(
        Sequence::new("cherry-pick", "conflict-quit-keeps-the-index", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["cherry-pick", "--quit"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"])
            .step(&["cherry-pick", "--abort"]),
    );

    // Picking a *merge*. `-m 1` names the parent whose diff is replayed, and
    // [`Shape::Merged`] is the only shape carrying a two-parent commit — so this
    // is the only place `sequencer.c`'s mainline handling runs at all. Step 2
    // moves onto the merge's own first parent's parent so the pick has somewhere
    // to land, and the `log` proves the picked commit is single-parent.
    out.push(
        Sequence::new("cherry-pick", "pick-a-merge-with-mainline", Shape::Merged)
            .step(&["log", "--oneline", "--graph"])
            .step(&["checkout", "-b", "pickhere", "main~2"])
            .step(&["cherry-pick", "-m", "1", "main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph"]),
    );

    // The refusal that guards it. Without `-m`, git names the merge by full oid
    // and dies; the interesting part is that it dies *before* touching anything,
    // so step 4 must show a clean tree and step 5 an unmoved branch.
    //
    // `strict`, because the exit code alone cannot distinguish "merge but no -m"
    // from any other `die()`, and the message carries the oid it objected to.
    out.push(
        Sequence::new("cherry-pick", "pick-a-merge-without-mainline-refuses", Shape::Merged)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["checkout", "-b", "pickhere", "main~2"])
            .step(&["cherry-pick", "main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // `--continue` with the conflict still unmerged. Git refuses at the *commit*
    // gate — `error: Committing is not possible because you have unmerged files`
    // then a `U conflict.txt` listing — rather than at a sequencer gate, and it
    // leaves the stop intact so the user can resolve and retry: step 4 must
    // still show `AA` and step 5 must still find the operation in progress.
    //
    // `strict`, because both refusal and success exit through the same verb and
    // the message is the only thing that says which gate stopped it. A port that
    // refuses for the right reason with the wrong words, or that quietly commits
    // the conflict markers, both score `Match` on exit code alone.
    out.push(
        Sequence::new("cherry-pick", "continue-with-unmerged-index-refuses", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step(&["cherry-pick", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--abort"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// revert
// ---------------------------------------------------------------------------

fn revert(out: &mut Vec<Sequence>) {
    // `revert` shares `sequencer.c` with `cherry-pick` and writes `REVERT_HEAD`
    // instead of `CHERRY_PICK_HEAD`; a port that wires the shared engine to one
    // filename passes every cherry-pick sequence above and fails here.
    out.push(
        Sequence::new("revert", "conflict-abort", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["revert", "--no-edit", "main~2"])
            .step(&["status", "--porcelain"])
            .step(&["revert", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"]),
    );

    // The continue half of the same stop. `REVERT_HEAD` has to survive steps 3-5
    // and be gone after step 6, and the commit git writes carries the
    // `Revert "…"` subject `sequencer.c` composes rather than the reverted
    // commit's own — which is what the `log` pins.
    out.push(
        Sequence::new("revert", "conflict-resolve-continue", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["revert", "--no-edit", "main~2"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "ws/indent.c"])
            .step(&["add", "ws/indent.c"])
            .step(&["revert", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"]),
    );

    // Two reverts, both of which conflict. Step 4's `--skip` drops the first and
    // walks straight into the second's conflict, so it is the one step in this
    // file where a *revert* sequencer both advances and re-stops: `sequencer/todo`
    // shrinks by one entry, `REVERT_HEAD` is rewritten to the second commit, and
    // the index goes unmerged again. A port that treats `--skip` as `--abort`
    // exits 0 with a clean tree at step 4 and is caught there rather than at the
    // end.
    out.push(
        Sequence::new("revert", "sequencer-todo-skip-into-second-conflict", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["revert", "--no-edit", "main~2", "main~3"])
            .step(&["status", "--porcelain"])
            .step(&["revert", "--skip"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"])
            .step(&["revert", "--abort"])
            .step(&["log", "--oneline", "-3"]),
    );

    // `-n` on a commit that reverts cleanly: `REVERT_HEAD` and `MERGE_MSG` are
    // written and no commit is made, so the `commit` at step 4 is what turns the
    // staged inverse into history. The asymmetry with `cherry-pick -n` is the
    // point — `cherry-pick -n` writes no `CHERRY_PICK_HEAD`, `revert -n` *does*
    // write `REVERT_HEAD` — and a port sharing one code path between the two
    // gets exactly one of them right.
    out.push(
        Sequence::new("revert", "no-commit-then-commit", Shape::Renamed)
            .step(&["revert", "-n", "HEAD"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"])
            .step(&["commit", "-m", "manual-revert"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"]),
    );

    // `--quit` on a revert, with the same contract as the cherry-pick one: the
    // sequencer state goes, the unmerged index stays. Step 4 must still report
    // `UU ws/indent.c`, and step 6's `restore --staged --worktree .` is what
    // proves the leftover really is an ordinary unmerged index — it collapses
    // the stage 1/2/3 entries back to `HEAD` and succeeds, which it could not do
    // against state a `--quit` had corrupted rather than merely abandoned.
    out.push(
        Sequence::new("revert", "conflict-quit-keeps-the-index", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["revert", "--no-edit", "main~2"])
            .step(&["revert", "--quit"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["restore", "--staged", "--worktree", "."]),
    );

    // Reverting a merge, which needs `-m` for the same reason picking one does
    // and produces a commit that *removes* the second parent's contribution
    // while keeping the merge in history. [`Shape::Merged`] is again the only
    // shape that can express it.
    out.push(
        Sequence::new("revert", "revert-a-merge-with-mainline", Shape::Merged)
            .step(&["revert", "-m", "1", "--no-edit", "HEAD"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-4"])
            .step(&["diff", "--stat", "HEAD~1", "HEAD"]),
    );
}

// ---------------------------------------------------------------------------
// rebase
// ---------------------------------------------------------------------------

fn rebase(out: &mut Vec<Sequence>) {
    // Conflicted rebase, resolved and continued. The interesting state is
    // `.git/rebase-merge/` — twenty-odd files (`sequencer.c:75`-`212`) that the
    // state probe walks in full — plus `REBASE_HEAD`, which survives the
    // *successful* continue and is therefore a fact about what the operation
    // left behind rather than about what it was doing.
    out.push(
        Sequence::new("rebase", "conflict-resolve-continue", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["rebase", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["rebase", "--continue"])
            .step(&["log", "--oneline"]),
    );

    // `-i` with `GIT_SEQUENCE_EDITOR=true` (pinned by `env::harden`) accepts the
    // generated todo unedited, which is exactly what makes the interactive
    // machinery reachable from a harness at all: the todo is written, read back
    // and executed, and nothing waits on a human.
    out.push(
        Sequence::new("rebase", "interactive-conflict-skip", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["rebase", "-i", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--skip"])
            .step(&["log", "--oneline"]),
    );

    out.push(
        Sequence::new("rebase", "interactive-conflict-abort", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["rebase", "-i", "theirs"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // A todo that stops for a reason other than a conflict. `--exec` appends an
    // `exec` line after every pick (`rebase --exec` implies `-i`), and a failing
    // one parks the rebase with a *clean* worktree and a half-consumed todo —
    // the `done`/`git-rebase-todo` split that a conflict stop never shows,
    // because a conflict stop always has an unmerged index to look at instead.
    // Continuing runs straight into the next `exec`, so step 3 measures the
    // todo advancing.
    out.push(
        Sequence::new("rebase", "interactive-exec-stop-continue", Shape::Renamed)
            .step(&["rebase", "-i", "--exec", "false", "HEAD~2"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--continue"])
            .step(&["rebase", "--abort"])
            .step(&["log", "--oneline"]),
    );

    // `--onto`: three commits replayed onto a base that is neither branch's
    // merge base, conflicting on the *first* of the three. That is the shape
    // `conflict-resolve-continue` above cannot produce — it rebases a single
    // commit, so `rebase-merge/git-rebase-todo` is empty at the stop and
    // `rebase-merge/done` holds everything. Here the todo still lists two picks,
    // and step 6's `--continue` has to commit the resolution *and* replay both
    // of them in the same invocation.
    out.push(
        Sequence::new("rebase", "onto-three-commit-conflict-continue", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main"])
            .step(&["rebase", "--onto", "main~4", "main~3"])
            .step(&["checkout", "--theirs", "--", "ws/indent.c"])
            .step(&["add", "ws/indent.c"])
            .step(&["rebase", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // The same three-commit `--onto`, walked with `--skip`. The skipped pick is
    // dropped, the second replays cleanly, and the third conflicts in turn — so
    // step 4 is the only place in this file where `rebase-merge/rewritten-list`
    // exists at a stop, because it is only written once a pick has actually
    // landed. The `--abort` after it has to undo the one commit the skip let
    // land (`whitespace: crlf to lf`, which step 6's `log` shows sitting on
    // `whitespace: seed`) and put `side` back where step 2 created it, which is
    // what step 8's `log` says — not merely drop the stop.
    out.push(
        Sequence::new("rebase", "onto-three-commit-skip-then-abort", Shape::Whitespace)
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main"])
            .step(&["rebase", "--onto", "main~4", "main~3"])
            .step(&["rebase", "--skip"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"])
            .step(&["rebase", "--abort"])
            .step(&["log", "--oneline"]),
    );

    // The **apply backend**, which parks its state in `.git/rebase-apply/`
    // rather than `.git/rebase-merge/` — a different directory, a different file
    // set (`patch`, `author-script`, `next`, `last`, `original-commit`), and a
    // different resume implementation in `builtin/rebase.c`. Every other rebase
    // sequence here runs the merge backend, so a port that implements one and
    // aliases the other scored full marks.
    out.push(
        Sequence::new("rebase", "apply-backend-conflict-abort", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["rebase", "--apply", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // `--autostash` over a dirty tree. [`Shape::Whitespace`] leaves an unstaged
    // whitespace-only edit to `ws/indent.c`, so step 2 has something to stash;
    // the stash is written to `rebase-merge/autostash` rather than to the stash
    // *reflog*, which is why step 6's `stash list` must be empty on both sides.
    // The abort at step 4 has to re-apply it — a port that drops the autostash
    // on abort silently discards the user's uncommitted work and leaves a clean
    // tree that looks correct until step 5.
    out.push(
        Sequence::new("rebase", "autostash-conflict-abort-restores", Shape::Whitespace)
            .step(&["checkout", "-b", "side", "main"])
            .step(&["rebase", "--autostash", "--onto", "main~4", "main~3"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"])
            .step(&["diff", "--stat"]),
    );

    // The refusal `--autostash` exists to lift. Without it the same rebase over
    // the same dirty tree is rejected before anything is written — `error:
    // cannot rebase: You have unstaged changes.`, exit 1 — so step 3 must find
    // no `rebase-merge/` at all. `restore .` then makes the identical argv get
    // *past* that gate at step 5, where it goes on to stop on the same conflict
    // every other `--onto` sequence here stops on: same command, different
    // failure, and the difference is the dirt rather than the arguments.
    //
    // `strict`, because the refusal and the conflict stop both exit 1 and both
    // print nothing on stdout. Only the message separates a port that checked
    // the worktree from one that refused for its own reasons.
    out.push(
        Sequence::new("rebase", "dirty-tree-refusal-then-restore", Shape::Whitespace)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["checkout", "-b", "side", "main"])
            .step(&["rebase", "--onto", "main~4", "main~3"])
            .step(&["status", "--porcelain"])
            .step(&["restore", "."])
            .step(&["rebase", "--onto", "main~4", "main~3"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"])
            .step(&["log", "--oneline"]),
    );

    // `--continue` after resolving the *worktree* but not staging it. Git checks
    // the index, not the file, and refuses with `ws/indent.c: needs merge` — so
    // a port that decides "is this resolved" by looking for conflict markers in
    // the worktree commits here and diverges by a whole commit.
    //
    // `strict`, because the refusal exits 1 and so does the conflict stop before
    // it; only the message separates them.
    out.push(
        Sequence::new("rebase", "continue-with-unstaged-resolution-refuses", Shape::Whitespace)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["restore", "."])
            .step(&["checkout", "-b", "side", "main"])
            .step(&["rebase", "--onto", "main~4", "main~3"])
            .step(&["checkout", "--theirs", "--", "ws/indent.c"])
            .step(&["rebase", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"]),
    );

    // `cherry-pick`'s `abort-then-refuse-resumption`, aimed at the rebase state
    // instead: after `rebase --abort` every resumption verb must say no, and the
    // *cherry-pick* one must say no too — a port that backs both operations with
    // one shared "in progress" flag leaves the flag set and makes step 6 try to
    // resume something.
    out.push(
        Sequence::new("rebase", "abort-then-refuse-resumption", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["rebase", "theirs"])
            .step(&["rebase", "--abort"])
            .step(&["rebase", "--continue"])
            .step(&["rebase", "--skip"])
            .step(&["cherry-pick", "--continue"])
            .step(&["status", "--porcelain"]),
    );

    // Every mutating verb, asked while a rebase is stopped on a conflict. Each
    // has its own gate in git and each must refuse without touching the stopped
    // rebase, so step 6's `status` is the assertion that the three refusals left
    // the state exactly as step 2 wrote it. All three exit 128 and all three
    // name the unmerged index rather than the rebase — `Merging is not
    // possible…`, `Cherry-picking is not possible…`, `Committing is not
    // possible…` — which is why a port with one blanket "operation in progress"
    // refusal is caught here rather than at the exit code.
    out.push(
        Sequence::new("rebase", "refused-while-a-rebase-is-stopped", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["rebase", "theirs"])
            .step(&["merge", "theirs"])
            .step(&["cherry-pick", "theirs"])
            .step(&["commit", "-m", "nope"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// am
// ---------------------------------------------------------------------------
//
// `am` parks its state in `.git/rebase-apply/` (`builtin/am.c:161`), which no
// other sequence here touches and which a single case can only reach by
// applying a mailbox that fails — after which it can do nothing further with it.

fn am(out: &mut Vec<Sequence>) {
    // [`Shape::Patches`] carries a two-patch mailbox whose pre-image is `main`'s
    // tree. Applying it succeeds; applying `mail/one.eml` (the first of those
    // two patches, again) then fails against the tree it just created, which is
    // a stop that needs no corrupt input to manufacture.
    out.push(
        Sequence::new("am", "mailbox-stop-skip", Shape::Patches)
            .step(&["am", "mail/series.mbox"])
            .step(&["am", "mail/one.eml"])
            .step(&["status", "--porcelain"])
            .step(&["am", "--skip"])
            .step(&["log", "--oneline"]),
    );

    out.push(
        Sequence::new("am", "mailbox-stop-abort", Shape::Patches)
            .step(&["am", "mail/one.eml"])
            .step(&["am", "mail/one.eml"])
            .step(&["am", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // The same shape of workflow with the mailbox arriving on **stdin**, which
    // is the mode `git am < patch` actually gets used in and the reason a step
    // carries its own payload: step 1 and step 2 are fed bytes, step 3 must be
    // fed nothing. A per-sequence payload would deliver the mailbox to
    // `am --skip` as well, which is a different invocation from the one meant.
    out.push(
        Sequence::new("am", "stdin-mailbox-stop-skip", Shape::Linear)
            .step_stdin(&["am"], super::MBOX)
            .step_stdin(&["am"], super::MBOX)
            .step(&["status", "--porcelain"])
            .step(&["am", "--skip"])
            .step(&["log", "--oneline"]),
    );

    // `--show-current-patch`, which exists only while `.git/rebase-apply/` does
    // and is the one `am` verb that *reads* the parked state back out. Its two
    // forms differ: bare prints the whole stored mail including the `From `/
    // `Subject:` headers `mailinfo` split off, `=diff` prints only from the
    // `---` onward. A port that stores the patch body and reconstructs a header
    // block passes `=diff` and fails the bare form.
    out.push(
        Sequence::new("am", "stop-show-current-patch-abort", Shape::Patches)
            .step(&["am", "mail/one.eml"])
            .step(&["am", "mail/one.eml"])
            .step(&["am", "--show-current-patch"])
            .step(&["am", "--show-current-patch=diff"])
            .step(&["am", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // `--3way` on a patch whose pre-image blob is reachable but whose current
    // file has moved on. [`Shape::Dirty`]'s `README.md` is committed at step 1,
    // after which the mailbox's `index 9741694..` line still resolves — so `am`
    // reconstructs the base tree, falls back to a real three-way merge, and
    // *conflicts*. That is a different stop from `mailbox-stop-skip`'s: the
    // index is unmerged rather than clean, and `rebase-apply/` carries the
    // `threeway` marker.
    //
    // Reached only in a sequence, because the divergence the three-way merge
    // needs has to be committed first and a case cannot commit.
    out.push(
        Sequence::new("am", "three-way-conflict-resolve-continue", Shape::Dirty)
            .step(&["commit", "-am", "dirty-committed"])
            .step_stdin(&["am", "--3way"], super::MBOX)
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "README.md"])
            .step(&["add", "README.md"])
            .step(&["am", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // The abort of that stop, cross-examined. All three `am` resumption verbs
    // answer the same `Resolve operation not in progress` once
    // `.git/rebase-apply/` is gone, so any one of them succeeding is a directory
    // that was not cleaned.
    out.push(
        Sequence::new("am", "three-way-abort-then-refuse-resumption", Shape::Dirty)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["commit", "-am", "dirty-committed"])
            .step_stdin(&["am", "--3way"], super::MBOX)
            .step(&["am", "--abort"])
            .step(&["am", "--continue"])
            .step(&["am", "--skip"])
            .step(&["am", "--show-current-patch"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // A two-patch mailbox applied in full, then `--3way` fed the first of those
    // two patches again. The three-way fallback finds the change already present
    // and reports `No changes -- Patch already applied.` while exiting **0** and
    // parking nothing — which is the one `am` outcome that is neither a clean
    // apply nor a stop, and the one a port is most likely to turn into a
    // failure. Step 4's `am --skip` is the proof that nothing was parked.
    out.push(
        Sequence::new("am", "three-way-already-applied-parks-nothing", Shape::Patches)
            .step(&["am", "mail/series.mbox"])
            .step(&["am", "--3way", "mail/one.eml"])
            .step(&["status", "--porcelain"])
            .step(&["am", "--skip"])
            .step(&["log", "--oneline"]),
    );
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

fn merge(out: &mut Vec<Sequence>) {
    // Stop at a conflict, resolve, commit. `MERGE_HEAD`/`MERGE_MSG`/`MERGE_MODE`
    // have to be written at step 2, survive steps 3-4 unchanged, and be gone
    // after step 5 — and the commit has to end up with two parents, which the
    // `log --graph` at the end is what pins.
    out.push(
        Sequence::new("merge", "conflict-resolve-commit", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--ours", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["commit", "--no-edit"])
            .step(&["log", "--oneline", "--graph"]),
    );

    // The same workflow from a subdirectory. A conflicted merge is resolved by
    // path, and every path in steps 4-5 is now relative to `src/` while the
    // index entries are relative to the root — the prefix has to be applied to
    // the pathspec and *not* to the unmerged entries git matches it against.
    // Nothing else here runs a stateful operation from anywhere but the root.
    out.push(
        Sequence::new("merge", "conflict-resolve-commit-from-subdir", Shape::Conflicted)
            .in_dir("src")
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--ours", "--", "../conflict.txt"])
            .step(&["add", "../conflict.txt"])
            .step(&["commit", "--no-edit"])
            .step(&["log", "--oneline"]),
    );

    // rerere: a feature that *only* exists across invocations. Steps 2-5 record
    // a resolution into `.git/rr-cache`; step 6 throws the merge away; step 7
    // hits the identical conflict and must replay the recorded resolution
    // instead of stopping. `runner::probe_rr_cache` compares the preimage and
    // postimage bytes, so a run that creates the cache and records the wrong
    // hunks — or records nothing — is caught at the step that recorded it
    // rather than at the step that failed to replay.
    //
    // `-c rerere.enabled=true` on the envelope rather than per step because the
    // setting has to hold for *both* halves: a config that reached only the
    // recording steps would make step 7 a test of the default instead.
    out.push(
        Sequence::new("merge", "rerere-record-then-replay", Shape::Conflicted)
            .with_config(&[("rerere.enabled", "true")])
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["checkout", "--ours", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["commit", "--no-edit"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"]),
    );

    // `merge --continue` rather than `commit`. Both finish a stopped merge and
    // they are not the same code path — `--continue` re-reads `MERGE_MODE` and
    // refuses if no merge is in progress, `commit` reads `MERGE_HEAD` and would
    // happily make an ordinary commit without one. A port that maps
    // `merge --continue` onto `commit` produces a single-parent commit here and
    // the `log --graph` at step 7 is what sees it.
    out.push(
        Sequence::new("merge", "conflict-resolve-merge-continue", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["checkout", "--ours", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["merge", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph"]),
    );

    // `--quit`, the merge verb with no analogue in the `--continue`/`--abort`
    // pair: it removes `MERGE_HEAD`/`MERGE_MSG`/`MERGE_MODE` and leaves the
    // unmerged index alone, so step 4's `status` still reports `AA conflict.txt`
    // with no merge in progress. Steps 5-7 then ask each finishing verb in turn,
    // and every one must refuse — `--abort` and `--continue` because the state
    // files are gone, `commit` because the index is still unmerged, which is a
    // *different* refusal and the one that proves `--quit` kept the work.
    //
    // `strict`: three refusals that all exit 128 with empty stdout are
    // indistinguishable by exit code, and the whole finding is which of the
    // three reasons each gave.
    out.push(
        Sequence::new("merge", "quit-keeps-the-index-then-refuses", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["merge", "--quit"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "--abort"])
            .step(&["merge", "--continue"])
            .step(&["commit", "-m", "nope"])
            .step(&["status", "--porcelain"]),
    );

    // `merge --abort` twice. The second is the canonical "the refusal is the
    // contract" case: `fatal: There is no merge to abort (MERGE_HEAD missing).`
    // A port whose `--abort` is idempotent — resetting to `HEAD` and exiting 0
    // whether or not a merge was in progress — throws away a user's uncommitted
    // work on a typo and reports success.
    out.push(
        Sequence::new("merge", "abort-twice-second-refuses", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );

    // `--squash`: the branch's content without its history. Git stages the
    // result, writes `.git/SQUASH_MSG`, prints `Squash commit -- not updating
    // HEAD`, and deliberately leaves `MERGE_HEAD` **unwritten** — so step 4's
    // `commit` makes a single-parent commit, which is the whole point and what
    // the `log --graph` at step 5 pins. A port that squashes by merging and
    // amending produces two parents.
    //
    // On [`Shape::Branched`], where `feature` is a fast-forward away: `--squash`
    // over a fast-forward is the case that separates "did not update HEAD" from
    // "did nothing", and it is reachable with no conflict to resolve first.
    out.push(
        Sequence::new("merge", "squash-then-commit", Shape::Branched)
            .step(&["merge", "--squash", "feature"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"])
            .step(&["commit", "-m", "squashed"])
            .step(&["log", "--oneline", "--graph"]),
    );

    // `--no-commit --no-ff`: the opposite half of the same question. Here git
    // *does* write `MERGE_HEAD`, `MERGE_MSG` and `MERGE_MODE` and stops before
    // committing, so the commit at step 4 finds a merge in progress and produces
    // two parents from the message git prepared. Run beside `squash-then-commit`
    // on the same shape, because the two differ only in which state files step 1
    // leaves behind.
    out.push(
        Sequence::new("merge", "no-commit-then-commit", Shape::Branched)
            .step(&["merge", "--no-commit", "--no-ff", "feature"])
            .step(&["status", "--porcelain"])
            .step(&["commit", "--no-edit"])
            .step(&["log", "--oneline", "--graph"])
            .step(&["status", "--porcelain"]),
    );

    // A fast-forward over a dirty tree, then a fast-forward that must refuse.
    // [`Shape::MergeableDirty`] holds an unstaged edit to `hot.txt` and none to
    // `cold.txt`, so `ff-cold` may move `HEAD` across the dirt and `ff-hot` may
    // not — and the refusal is per path, not a blanket "the tree is dirty".
    // Step 2 having *succeeded* is what makes step 4's refusal mean something:
    // a port that refuses both scores identically on step 4 alone.
    //
    // Step 6 is the tail: no merge was started, so `merge --abort` must refuse
    // rather than reset the fast-forward away.
    out.push(
        Sequence::new("merge", "fast-forward-then-per-path-refusal", Shape::MergeableDirty)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["status", "--porcelain"])
            .step(&["merge", "ff-cold"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "ff-hot"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "--abort"])
            .step(&["log", "--oneline", "-1"]),
    );

    // Every mutating verb, asked while a merge is stopped on a conflict. Each
    // refuses from a different gate — `cherry-pick` and `merge` from the
    // unmerged-index check in `sequencer.c`, `rebase` from its dirty-tree check,
    // `checkout` from `you need to resolve your current index first`, `stash`
    // from `could not write index` — and a port with one blanket "operation in
    // progress" refusal produces one message five times.
    out.push(
        Sequence::new("merge", "refused-while-a-merge-is-stopped", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["cherry-pick", "theirs"])
            .step(&["rebase", "theirs"])
            .step(&["checkout", "theirs"])
            .step(&["stash", "push", "-m", "nope"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"]),
    );

    // The rest of rerere, which `rerere-record-then-replay` above does not
    // reach: `status` names the paths with a preimage, `diff` prints the
    // conflict as git normalised it into `rr-cache/<hash>/preimage` (note the
    // bare `<<<<<<<`/`>>>>>>>` markers — the branch names are stripped, and a
    // port that stores the raw worktree bytes prints them back), the explicit
    // `rerere` verb records the resolution the merge left staged, and `forget`
    // tears the recording back down and re-conflicts the file.
    //
    // `runner::probe_rr_cache` compares `preimage`/`postimage` byte for byte at
    // every step, so each of those five transitions is scored where it happened.
    out.push(
        Sequence::new("merge", "rerere-status-diff-record-forget", Shape::Conflicted)
            .with_config(&[("rerere.enabled", "true")])
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["rerere", "status"])
            .step(&["rerere", "diff"])
            .step(&["checkout", "--ours", "--", "conflict.txt"])
            .step(&["add", "conflict.txt"])
            .step(&["rerere"])
            .step(&["commit", "--no-edit"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "forget", "conflict.txt"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// bisect
// ---------------------------------------------------------------------------

fn bisect(out: &mut Vec<Sequence>) {
    // A whole bisect, start to verdict to reset. Every step moves `BISECT_*`
    // and `.git/BISECT_LOG`, and the verdict is only reachable by walking:
    // which commit git checks out at step 4 is a function of the answers given
    // at steps 2-3, so no single case can be written that lands on it.
    //
    // [`Shape::Renamed`] rather than `Branched`: bisect needs a history long
    // enough to halve more than once, and `Renamed` has six commits on `main`.
    out.push(
        Sequence::new("bisect", "start-good-bad-verdict-reset", Shape::Renamed)
            .step(&["bisect", "start"])
            .step(&["bisect", "bad", "HEAD"])
            .step(&["bisect", "good", "HEAD~4"])
            .step(&["bisect", "good"])
            .step(&["bisect", "bad"])
            .step(&["bisect", "log"])
            .step(&["bisect", "reset"])
            .step(&["status", "--porcelain"]),
    );

    // The same walk under **renamed terms**, plus `skip`. `--term-old`/
    // `--term-new` are stored in `.git/BISECT_TERMS` and every subsequent
    // invocation reads them back — `bisect working` is only a valid verb because
    // step 1 said so, and `bisect log` replays the session using the custom
    // words rather than good/bad. A port that hard-codes good/bad passes
    // `start-good-bad-verdict-reset` above and fails at step 3 here.
    //
    // `skip` is the third answer, and the only one that does not narrow the
    // range: it moves to a different commit in the same interval and records a
    // `# skip:` line rather than a bound.
    out.push(
        Sequence::new("bisect", "custom-terms-skip-log-reset", Shape::Renamed)
            .step(&["bisect", "start", "--term-old", "working", "--term-new", "broken"])
            .step(&["bisect", "terms"])
            .step(&["bisect", "broken", "HEAD"])
            .step(&["bisect", "working", "HEAD~5"])
            .step(&["bisect", "skip"])
            .step(&["bisect", "log"])
            .step(&["status", "--porcelain"])
            .step(&["bisect", "reset"])
            .step(&["status", "--porcelain"]),
    );

    // `bisect run`, which drives the whole search from one invocation: git
    // checks a commit out, runs the command, reads its exit status as the
    // verdict, and repeats until one commit is left. `true` is the command
    // because it needs no repository content to be deterministic — every
    // revision is "good", so the search walks the *entire* interval and lands on
    // the tip, and the transcript is a function only of how git halves the
    // range.
    //
    // Nothing else in this corpus reaches `bisect_run_cmd`; a single case can
    // start a bisect but never let it iterate.
    out.push(
        Sequence::new("bisect", "run-to-verdict", Shape::Renamed)
            .step(&["bisect", "start", "HEAD", "HEAD~4"])
            .step(&["bisect", "run", "true"])
            .step(&["bisect", "log"])
            .step(&["bisect", "reset"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"]),
    );
}

// ---------------------------------------------------------------------------
// stash
// ---------------------------------------------------------------------------

fn stash(out: &mut Vec<Sequence>) {
    // A conflicting `stash pop`, which is a genuinely awkward state: the index
    // is left unmerged, `AUTO_MERGE` is written, and the entry is *kept* rather
    // than dropped. Manufacturing it needs three steps — stash the worktree,
    // restore an older entry, commit it — so that the entry being popped and
    // `HEAD` disagree about `counter.txt`.
    //
    // Step 8's `stash drop` is the assertion that the conflicting pop did not
    // consume the entry: it drops `stash@{0}`, which is still the entry step 4
    // failed to pop, and step 9 shows the two of the fixture's three the run has
    // not consumed (`staged and unstaged` and `with untracked`; step 2 took
    // `unstaged only`). A pop that dropped on conflict leaves a different list.
    out.push(
        Sequence::new("stash", "pop-conflict-resolve-drop", Shape::Stashed)
            .step(&["stash", "push", "-m", "seq"])
            .step(&["stash", "pop", "stash@{3}"])
            .step(&["commit", "-am", "seq-base"])
            .step(&["stash", "pop"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "counter.txt"])
            .step(&["add", "counter.txt"])
            .step(&["stash", "drop"])
            .step(&["stash", "list"]),
    );

    // `--keep-index`: stash everything, then put the *staged* half back. On
    // [`Shape::Stashed`] `notes.txt` is both staged and further modified, so
    // after step 2 the index still holds the staged version and the worktree
    // matches it — which is what makes step 4's `commit` commit exactly the
    // staged half and nothing else. Step 5's `pop` then collides with that new
    // commit and stops unmerged, keeping the entry, and step 8's `stash list`
    // shows all four entries still present.
    //
    // A port that implements `--keep-index` by not stashing the staged changes
    // at all produces the same `status` at step 3 and a different stash entry,
    // which only surfaces at the `pop`.
    out.push(
        Sequence::new("stash", "keep-index-commit-then-pop-conflict", Shape::Stashed)
            .step(&["stash", "list"])
            .step(&["stash", "push", "--keep-index", "-m", "keep"])
            .step(&["status", "--porcelain"])
            .step(&["commit", "-m", "staged-only"])
            .step(&["stash", "pop"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"])
            .step(&["stash", "list"]),
    );

    // `apply` twice. The first restores the entry over a clean tree; the second
    // meets its own result and must refuse per path — `Your local changes to the
    // following files would be overwritten by merge` naming `counter.txt` and
    // `notes.txt` — while leaving the worktree as the first apply left it and
    // the entry still on the stack. A port that re-applies idempotently, or that
    // refuses by dropping the entry, both diverge at step 4.
    out.push(
        Sequence::new("stash", "apply-twice-second-refuses", Shape::Stashed)
            .step(&["stash", "push", "-m", "base"])
            .step(&["stash", "apply"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "apply"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"]),
    );

    // `stash branch`, which is three operations in one: create a branch at the
    // commit the entry was *made from*, check it out, apply the entry with its
    // index restored, and drop it. The entry named is `stash@{1}` — after step
    // 1's push that is the fixture's `staged and unstaged` one, so the branch is
    // created from an entry that is *not* on top of the stack and that carries
    // both a staged and an unstaged half: step 3's `MM notes.txt` is what says
    // the index was restored rather than folded into the worktree.
    //
    // Every fixture entry predates the `.gitignore` commit, so `recovered`
    // lands one commit back from `main` and `ignored.txt` shows up as untracked
    // rather than ignored — a fact about which commit was chosen rather than
    // about which files were restored.
    out.push(
        Sequence::new("stash", "branch-from-a-named-entry", Shape::Stashed)
            .step(&["stash", "push", "-m", "base"])
            .step(&["stash", "branch", "recovered", "stash@{1}"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"])
            .step(&["log", "--oneline", "-2"])
            .step(&["branch", "--format=%(refname:short)"]),
    );

    // Dropping by index from the middle of the stack, then popping the top. The
    // renumbering is the property: after `drop stash@{1}` the entry that was
    // `@{2}` becomes `@{1}`, and the following `pop` must take the entry that
    // was `@{0}` all along. A port that stores entries by name rather than as a
    // reflog gets the list right and the `pop` wrong.
    out.push(
        Sequence::new("stash", "drop-by-index-then-pop", Shape::Stashed)
            .step(&["stash", "push", "-u", "-m", "all"])
            .step(&["stash", "list"])
            .step(&["stash", "drop", "stash@{1}"])
            .step(&["stash", "list"])
            .step(&["stash", "pop"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"]),
    );

    // The empty-stack contract, reached by emptying a *non*-empty stack. `clear`
    // has to remove three entries and the `refs/stash` reflog with them, after
    // which `pop`, `drop` and `show` all answer `No stash entries found.` and
    // exit 1 — and the worktree the fixture came with is untouched throughout,
    // which step 7's `status` is what says.
    //
    // `strict`: the three refusals are identical in exit code and empty in
    // stdout, and a `clear` that only truncated the list would let one of them
    // find an entry and act on it.
    out.push(
        Sequence::new("stash", "clear-then-empty-stack-refusals", Shape::Stashed)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["stash", "list"])
            .step(&["stash", "clear"])
            .step(&["stash", "pop"])
            .step(&["stash", "drop"])
            .step(&["stash", "show"])
            .step(&["stash", "list"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// sparse-checkout
// ---------------------------------------------------------------------------

fn sparse_checkout(out: &mut Vec<Sequence>) {
    // `set` narrows the cone, `reapply` re-runs the checkout against the current
    // patterns, `add` widens it, `disable` unwinds the whole mode. Each step's
    // meaning is the previous step's patterns — `reapply` against a cone nobody
    // changed is a no-op, and only a sequence can put it after a `set` that
    // moved something.
    out.push(
        Sequence::new("sparse-checkout", "set-reapply-add-disable", Shape::Sparse)
            .step(&["sparse-checkout", "list"])
            .step(&["sparse-checkout", "set", "outside"])
            .step(&["status", "--porcelain"])
            .step(&["sparse-checkout", "reapply"])
            .step(&["sparse-checkout", "add", "inside"])
            .step(&["sparse-checkout", "list"])
            .step(&["sparse-checkout", "disable"])
            .step(&["status", "--porcelain"]),
    );

    // The **non-cone** mode, which is a different pattern language stored in the
    // same file: `set --no-cone` writes the given `.gitignore`-style lines to
    // `.git/info/sparse-checkout` verbatim and clears `core.sparseCheckoutCone`,
    // so `list` at step 3 must print them back unchanged rather than as
    // directories. The sequence starts from the fixture's *cone* configuration,
    // so step 2 is a mode switch and not a fresh init — which is the transition
    // a port is most likely to get wrong, because it has to rewrite the config
    // as well as the pattern file.
    out.push(
        Sequence::new("sparse-checkout", "cone-to-no-cone-and-back", Shape::Sparse)
            .step(&["sparse-checkout", "list"])
            .step(&["sparse-checkout", "set", "--no-cone", "/*", "!/outside/"])
            .step(&["sparse-checkout", "list"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "-t"])
            .step(&["sparse-checkout", "reapply"])
            .step(&["sparse-checkout", "disable"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// worktree
// ---------------------------------------------------------------------------

fn worktree(out: &mut Vec<Sequence>) {
    // `add` writes `.git/worktrees/wt2/{gitdir,HEAD,commondir}` and a `.git`
    // file in the new tree; `move` rewrites both ends of that pair; `remove`
    // deletes them; `prune` reports on what is left. Every one of those steps
    // reads the administrative files the one before it wrote, and
    // [`Shape::Worktree`] can only supply the state after `add` — never the
    // transitions.
    //
    // Deliberately without `--relative-paths`, which [`Shape::Worktree`] needs
    // because the fixture is *copied* to a new root after it is built. A
    // sequence runs in the repository it created, so absolute paths are correct
    // here, and the two sides' differing roots are already masked to `<REPO>` by
    // `runner::normalize`. Using it instead would fail at step 1 on a flag gap
    // and leave `move`/`remove`/`prune` unmeasured behind it.
    out.push(
        Sequence::new("worktree", "add-move-remove-prune", Shape::Branched)
            .step(&["worktree", "add", "-b", "wtb", "wt2"])
            .step(&["worktree", "list"])
            .step(&["worktree", "move", "wt2", "wt3"])
            .step(&["worktree", "list"])
            .step(&["worktree", "remove", "wt3"])
            .step(&["worktree", "list"])
            .step(&["worktree", "prune", "-v"]),
    );

    // `lock`, which is administrative state living in one file
    // (`.git/worktrees/wtb/locked`) that nothing but `worktree` itself reads.
    // Its whole effect is on the *next* command: `list` gains a `locked`
    // column, `remove` refuses with `cannot remove a locked working tree`, and
    // only after `unlock` — or `remove --force` — does it go. A port that writes
    // the file and never consults it passes `lock` and `list` and fails at the
    // refusal, which is the one step that matters.
    out.push(
        Sequence::new("worktree", "add-lock-refuse-remove-unlock", Shape::Branched)
            .step(&["worktree", "add", "-b", "wtb", "wt2"])
            .step(&["worktree", "lock", "wt2"])
            .step(&["worktree", "list"])
            .step(&["worktree", "remove", "wt2"])
            .step(&["worktree", "unlock", "wt2"])
            .step(&["worktree", "list"])
            .step(&["worktree", "remove", "--force", "wt2"])
            .step(&["worktree", "list"])
            .step(&["worktree", "prune", "-v"]),
    );

    // A worktree that is **used** rather than only administered. `--detach`
    // gives it a detached `HEAD` of its own, and steps 3-5 run `-C wt2` so the
    // repository is discovered from inside the linked tree — where `HEAD` is
    // read from `.git/worktrees/wt2/HEAD` and the object store from the common
    // directory. A commit made there moves only that worktree's `HEAD`, which
    // step 6's `list` and step 8's `log` on the main worktree are what pin.
    //
    // [`Shape::Worktree`] can supply a linked worktree but not one this sequence
    // created and then committed in: a case is one argv, so it can do exactly
    // one of those things.
    out.push(
        Sequence::new("worktree", "add-detached-commit-inside-remove", Shape::Branched)
            .step(&["worktree", "add", "--detach", "wt2", "HEAD"])
            .step(&["worktree", "list"])
            .step(&["-C", "wt2", "rev-parse", "--abbrev-ref", "HEAD"])
            .step(&["-C", "wt2", "commit", "--allow-empty", "-m", "inside"])
            .step(&["-C", "wt2", "log", "--oneline", "-2"])
            .step(&["worktree", "list"])
            .step(&["log", "--oneline", "-1"])
            .step(&["worktree", "remove", "--force", "wt2"])
            .step(&["worktree", "list"]),
    );
}

// ---------------------------------------------------------------------------
// apply
// ---------------------------------------------------------------------------
//
// `apply` is a single invocation by nature, which is exactly why it belongs
// here: the interesting questions are what it leaves in the *index* for the next
// command, and — for `--3way` — whether the conflict it writes is a real
// unmerged index that `commit` will refuse or only markers in a file.

fn apply(out: &mut Vec<Sequence>) {
    // `--index` updates the index and the worktree together, so step 3's
    // `commit` (with no `-a`) has something staged, and `-R --index` at step 5
    // must undo both halves. `patches/valid.patch` is `main..pending~1` on
    // `app/main.c`, generated by the fixture from its own objects, so the
    // pre-image matches [`Shape::Patches`]'s `main` exactly.
    //
    // Reachable only in a sequence: a case can run `apply --index` but nothing
    // afterwards, and the whole claim of `--index` is about what comes after.
    out.push(
        Sequence::new("apply", "index-apply-commit-then-reverse", Shape::Patches)
            .step(&["apply", "--index", "patches/valid.patch"])
            .step(&["status", "--porcelain"])
            .step(&["commit", "-m", "applied"])
            .step(&["log", "--oneline", "-2"])
            .step(&["apply", "-R", "--index", "patches/valid.patch"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"]),
    );

    // `--3way` writing a genuinely conflicted index. The payload's `index
    // 9741694..` line names the blob for `# fixture\n`; step 1 commits
    // [`Shape::Dirty`]'s divergent `README.md`, so the pre-image is reachable
    // but no longer current and the three-way fallback conflicts. `apply` exits
    // 1 with `Applied patch to 'README.md' with conflicts.` and — unlike `am` —
    // parks **no** operation state at all, so steps 4-6 finish the work as an
    // ordinary commit. A port that writes `MERGE_HEAD` here makes step 6 produce
    // a two-parent commit.
    out.push(
        Sequence::new("apply", "three-way-conflict-then-commit", Shape::Dirty)
            .step(&["commit", "-am", "dirty-committed"])
            .step_stdin(&["apply", "--3way", "-"], README_DIFF)
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "README.md"])
            .step(&["add", "README.md"])
            .step(&["commit", "-m", "resolved"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-3"]),
    );

    // `--cached`: the index moves and the worktree does not, which `status` can
    // only say as `MM` — staged against `HEAD` *and* modified against the index
    // — and which needs a second command to observe at all. The `commit` at step
    // 4 then records the staged version while leaving the file on disk as it
    // was, so step 5's `status` must still report a modification.
    out.push(
        Sequence::new("apply", "cached-then-commit-leaves-worktree", Shape::Linear)
            .step_stdin(&["apply", "--cached", "-"], README_DIFF)
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--stat"])
            .step(&["commit", "-m", "staged-only"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline"]),
    );
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------
//
// Notes live on `refs/notes/*` and are read back by object name, so every verb
// here reads a ref the previous one wrote. A conflicted notes merge is also the
// only thing in this harness that populates `.git/NOTES_MERGE_WORKTREE` and
// `.git/NOTES_MERGE_REF` — both of which `runner::probe_op_state` walks and
// which nothing else could reach.

fn notes(out: &mut Vec<Sequence>) {
    // The note lifecycle on one object. `append` is the step under test: it must
    // read the existing note, add a blank line, and append — so a port that
    // implements it as "write" loses `first note` and the `show` at step 5 is
    // where that is caught. `copy` then puts the *appended* blob on a second
    // object, which is why the `list` at step 7 must show one blob id twice.
    out.push(
        Sequence::new("notes", "add-append-copy-remove", Shape::Branched)
            .step(&["notes", "add", "-m", "first-note", "HEAD"])
            .step(&["notes", "list"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "append", "-m", "second-line", "HEAD"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "copy", "HEAD", "HEAD~1"])
            .step(&["notes", "list"])
            .step(&["log", "--oneline", "--notes", "-2"])
            .step(&["notes", "remove", "HEAD"])
            .step(&["notes", "list"]),
    );

    // Two notes refs carrying different text for the same object, merged. Git
    // cannot resolve an add/add on a note, so it writes the conflicted note into
    // `.git/NOTES_MERGE_WORKTREE/<oid>`, records `NOTES_MERGE_REF`, and exits 1
    // — a stopped operation with *no* index involvement at all, which is why
    // `status` at step 4 is clean and the only evidence is in the state probe.
    // `--abort` must remove both and leave `refs/notes/commits` untouched.
    out.push(
        Sequence::new("notes", "merge-conflict-abort", Shape::Branched)
            .step(&["notes", "add", "-m", "on-main", "HEAD"])
            .step(&["notes", "--ref", "other", "add", "-m", "on-other", "HEAD"])
            .step(&["notes", "merge", "other"])
            .step(&["status", "--porcelain"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "merge", "--abort"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "list"]),
    );

    // The same stop, committed instead. `notes merge --commit` reads back
    // whatever is in `NOTES_MERGE_WORKTREE` — here the untouched conflict, since
    // nothing in a sequence can edit it — and stores it as the note, markers and
    // all, naming the two refs in the `<<<<<<<`/`>>>>>>>` lines. That is git's
    // actual behaviour and it is the resume path the abort variant cannot cover.
    out.push(
        Sequence::new("notes", "merge-conflict-commit", Shape::Branched)
            .step(&["notes", "add", "-m", "on-main", "HEAD"])
            .step(&["notes", "--ref", "other", "add", "-m", "on-other", "HEAD"])
            .step(&["notes", "merge", "other"])
            .step(&["notes", "merge", "--commit"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "list"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// replace
// ---------------------------------------------------------------------------

fn replace(out: &mut Vec<Sequence>) {
    // `refs/replace/<oid>` changes what *every later command* sees when it reads
    // that object, which is a property no single invocation can show: step 2
    // lists the ref, step 3's `log` walks a history in which `HEAD~1` has been
    // swapped for `HEAD`'s content, and step 6 takes it back off, after which
    // step 8's `log` must be the fixture's again.
    //
    // Steps 5 and 6 run under `--no-replace-objects` so the global is exercised
    // on both a reading and a mutating verb while a replacement is live: step 5
    // is the assertion that it suppresses the swap (`edfab1b initial`, against
    // step 3's `edfab1b add two`), and step 6 that a `replace -d` under it still
    // finds and deletes the ref. It is not required for the delete — stock
    // deletes the same ref without it — which is exactly why a port that
    // mishandles the flag fails step 5 and not step 6.
    out.push(
        Sequence::new("replace", "replace-then-log-then-delete", Shape::Branched)
            .step(&["replace", "HEAD~1", "HEAD"])
            .step(&["replace", "-l"])
            .step(&["log", "--oneline"])
            .step(&["cat-file", "-p", "HEAD"])
            .step(&["--no-replace-objects", "log", "--oneline"])
            .step(&["--no-replace-objects", "replace", "-d", "HEAD~1"])
            .step(&["replace", "-l"])
            .step(&["log", "--oneline"]),
    );
}

// ---------------------------------------------------------------------------
// refs and storage
// ---------------------------------------------------------------------------
//
// Three commands whose effect is only visible to the command after them:
// `update-ref --stdin` (whose transaction either lands whole or not at all),
// `reflog` (which `runner::probe_reflogs` compares line for line), and
// `gc` (whose result is what survived).

fn refs_and_storage(out: &mut Vec<Sequence>) {
    // A committed transaction, then a failing one. Step 4's transaction opens
    // with a legal `delete refs/heads/txn-a` and then names a non-existent ref
    // as a new value, so a port that applies commands as it parses them has
    // already deleted `txn-a` by the time it dies — and step 5's `for-each-ref`
    // is what says so. Both transactions are fed on stdin, per step, because
    // step 3 and step 5 must be fed nothing.
    out.push(
        Sequence::new("update-ref", "stdin-transaction-then-failed-transaction", Shape::Branched)
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_TXN)
            .step(&["for-each-ref", "--format=%(refname)", "refs/heads"])
            .step(&["reflog", "show", "refs/heads/txn-a"])
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_BAD_TXN)
            .step(&["for-each-ref", "--format=%(refname)", "refs/heads"])
            .step(&["status", "--porcelain"]),
    );

    // `reflog delete` from the middle of `HEAD`'s log, then `expire --expire=all`
    // over every log there is. The delete renumbers everything below it — the
    // entry that was `HEAD@{2}` becomes `HEAD@{1}` — and `expire` empties the
    // file without touching the commits, so step 7's `log` must still show both
    // and step 8's `fsck` must still be silent. `probe_reflogs` compares
    // `.git/logs/**` verbatim at every step, including the pinned committer
    // date, so a port that rewrites the surviving lines is caught at the step
    // that rewrote them.
    out.push(
        Sequence::new("reflog", "delete-one-then-expire-all", Shape::Branched)
            .step(&["commit", "--allow-empty", "-m", "e1"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["reflog", "delete", "HEAD@{1}"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["log", "--oneline", "-2"])
            .step(&["fsck", "--no-progress", "--no-dangling"]),
    );

    // `gc` over [`Shape::Packed`], which carries loose duplicates of packed
    // objects, two packs, and a commit no ref reaches. Step 1 shows the loose
    // duplicates `prune-packed` would remove; steps 2-3 expire the reflogs and
    // collect; step 6 asks `prune-packed` again and must find nothing left to
    // do. The last is the assertion: a `gc` that repacks without pruning leaves
    // the same five loose objects and prints the same five `rm -f` lines, which
    // is indistinguishable from a working `gc` by exit code alone.
    //
    // Deliberately no `count-objects -v`: its `size-pack` is a byte count of a
    // pack, and `runner::probe_storage` documents why pack *bytes* are not a
    // property two correct implementations must share.
    out.push(
        Sequence::new("gc", "prune-packed-expire-collect", Shape::Packed)
            .step(&["prune-packed", "-n"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["gc", "--prune=all", "--quiet"])
            .step(&["fsck", "--no-progress", "--no-dangling"])
            .step(&["rev-list", "--all", "--count"])
            .step(&["prune-packed", "-n"])
            .step(&["log", "--oneline", "-1"]),
    );
}

// ---------------------------------------------------------------------------
// remote-side workflows
// ---------------------------------------------------------------------------
//
// [`Shape::BehindRemote`] carries a bare remote inside the fixture reached by a
// relative URL, so these run entirely offline and each copy talks to its own.

fn remote_side(out: &mut Vec<Sequence>) {
    // `remote add` writes a config section and nothing else; the refspec it
    // wrote is only exercised by the `fetch` after it, and the remote-tracking
    // refs that fetch created are only visible to the `branch -r` after that.
    // `remote remove` then has to delete `refs/remotes/mirror/*` as well as the
    // config — a port that removes only the section leaves two stale tracking
    // refs, which step 6 is what sees.
    out.push(
        Sequence::new("remote", "add-fetch-list-then-remove", Shape::BehindRemote)
            .step(&["remote", "add", "mirror", "./.remote.git"])
            .step(&["remote", "-v"])
            .step(&["fetch", "mirror"])
            .step(&["branch", "-r"])
            .step(&["remote", "remove", "mirror"])
            .step(&["branch", "-r"])
            .step(&["config", "--get-regexp", "^remote\\."]),
    );

    // `main` is three commits behind `origin/main` and the worktree is dirty on
    // two paths `origin/main` never rewrites — it moves `shared.txt` alone,
    // while `clash.txt` is only rewritten on the remote's `div` — so the
    // fast-forward at step 4 must
    // succeed *and* carry the dirt through — the per-path gate, on the
    // fast-forward side, where a blanket refusal is the common mistake. Step 5
    // proves the edits survived; step 6 proves the branch fast-forwarded onto
    // `origin/main` rather than merging.
    out.push(
        Sequence::new("fetch", "fetch-then-fast-forward-over-dirt", Shape::BehindRemote)
            .step(&["fetch", "origin"])
            .step(&["log", "--oneline", "-1", "origin/main"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "--ff-only", "origin/main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["diff", "--stat"]),
    );

    // `pull` on the diverged branch, which has to fetch and then *merge* — and
    // the merge must refuse, because `clash.txt` is rewritten by the remote side
    // of `div` and held dirty locally. Step 5 discards that one edit and reruns
    // the identical command, which then succeeds: same argv, opposite outcome,
    // and the difference is the per-path check rather than the arguments.
    //
    // Deliberately **not** `strict`, unlike the refusal sequences elsewhere in
    // this file, and for the reason those are: there the refusal is the tail and
    // its message is the whole finding, here it is step 3 of 8. A stderr
    // mismatch on it would stop the run before steps 5-8 — the discard, the
    // identical re-pull, and the merge commit it produces — ever executed, and
    // those are the part no other case can reach. `--no-advice` is kept on the
    // envelope so the refusal git prints stays the refusal rather than the
    // advice paragraph after it, should this ever be tightened.
    out.push(
        Sequence::new("pull", "diverged-dirty-refusal-then-merge", Shape::BehindRemote)
            .with_globals(&[&["--no-advice"]])
            .step(&["checkout", "div"])
            .step(&["status", "--porcelain"])
            .step(&["pull", "--no-rebase", "origin", "div"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--", "clash.txt"])
            .step(&["pull", "--no-rebase", "origin", "div"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-4"]),
    );
}

// ---------------------------------------------------------------------------
// submodule
// ---------------------------------------------------------------------------

fn submodule(out: &mut Vec<Sequence>) {
    // `deinit` empties the submodule's working directory and drops its
    // `submodule.sub.*` config, after which `submodule status` prefixes the oid
    // with `-` to mean "not initialized" — and the *parent's* `status` must stay
    // clean, because emptying a submodule worktree is not a change to the gitlink.
    // `update --init` puts it all back and the two `status` lines must match the
    // ones from step 1.
    //
    // `protocol.file.allow=always` on the envelope because the submodule's URL
    // is a local path and git refuses those by default since CVE-2022-39253; the
    // fixture is built with the same setting.
    out.push(
        Sequence::new("submodule", "deinit-then-update-init", Shape::Submodule)
            .with_config(&[("protocol.file.allow", "always")])
            .step(&["submodule", "status"])
            .step(&["status", "--porcelain"])
            .step(&["submodule", "deinit", "sub"])
            .step(&["submodule", "status"])
            .step(&["status", "--porcelain"])
            .step(&["submodule", "update", "--init", "sub"])
            .step(&["submodule", "status"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// unwinding a conflicted index by other means
// ---------------------------------------------------------------------------
//
// `merge --abort` is not the only way out of a stopped merge, and the other two
// are not aliases for it.

fn unwind(out: &mut Vec<Sequence>) {
    // `reset --merge`: drops the merge state and resets the index and worktree,
    // but only for paths that differ between the index and `HEAD` — it is
    // documented as the safe unwind precisely because it refuses to discard
    // unrelated local changes. Reached here on the stopped merge, where its
    // effect must equal `merge --abort`'s: no `MERGE_HEAD`, a clean `status`,
    // and `main` unmoved. The `merge --abort` at step 7 is the assertion of
    // exactly that — it has to refuse with `fatal: There is no merge to abort
    // (MERGE_HEAD missing).` and exit 128, because there is no longer a merge
    // for it to find.
    //
    // `strict`, because that refusal is the tail and it carries no stdout: only
    // the message separates "reset --merge cleared MERGE_HEAD" from a port that
    // refused for some other reason.
    out.push(
        Sequence::new("reset", "merge-reset-unwinds-a-stopped-merge", Shape::Conflicted)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["status", "--porcelain"])
            .step(&["reset", "--merge"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["merge", "--abort"]),
    );

    // `checkout -m -- <path>`: the inverse of every other step in this file. It
    // re-creates a conflict in the *worktree* from the path's stage 1/2/3 index
    // entries. Step 3's `checkout --ours` is what sets that up: it overwrites
    // the worktree with the ours side and deliberately leaves the stages alone,
    // so step 4's `status` is still `AA conflict.txt` and step 5 has something
    // to rebuild from. Step 7's `diff` is the assertion — a combined `diff --cc`
    // carrying `<<<<<<< ours` / `>>>>>>> theirs`. A port whose `checkout --ours`
    // collapses the path to a single stage-0 entry has nothing to recreate and
    // diverges at step 5.
    out.push(
        Sequence::new("checkout", "merge-recreates-a-resolved-conflict", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["merge", "theirs"])
            .step(&["checkout", "--ours", "--", "conflict.txt"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "-m", "--", "conflict.txt"])
            .step(&["status", "--porcelain"])
            .step(&["diff"])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"]),
    );
}
