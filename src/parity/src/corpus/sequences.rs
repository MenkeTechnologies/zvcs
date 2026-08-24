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
    criss_cross(&mut s);
    unrelated(&mut s);
    cherry(&mut s);
    damaged(&mut s);
    symlinks(&mut s);
    commit_graph(&mut s);
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

// ---------------------------------------------------------------------------
// criss-cross: the workflows that follow a merge with two merge bases
// ---------------------------------------------------------------------------
//
// [`Shape::CrissCross`] is checked out on `cc-left`, clean, with `cc-right` a
// second tip that shares two incomparable merge bases with it (`cc-a` and
// `cc-b`). Merging the two makes `merge-ort` build a *virtual* base by merging
// those two bases with each other, and the fixture's `clash.txt` makes that
// inner merge itself conflict — so stage 1 of the outer conflict holds a blob
// that exists in no commit. Stock's is
// `<<<<<<<<< Temporary merge branch 1\nb\n=========\na\n>>>>>>>>> Temporary
// merge branch 2\n`, nine-character markers and all.
//
// A single case can see that stop. What it cannot see is whether the stop can
// be resumed, aborted or recorded — whether the operation-state files describe
// a merge that is really in progress — which is what every sequence below is
// for.

fn criss_cross(out: &mut Vec<Sequence>) {
    // The whole conflicted criss-cross merge, resolved and finished. Steps 3-5
    // print the three index stages on **stdout**, which is the only place in
    // this corpus where the virtual merge base is directly readable rather than
    // inferred from a state digest: step 3 is the inner merge's own conflict,
    // steps 4 and 5 are the two tips. A port that picks one of the two real
    // bases instead of building a virtual one prints `a`, `b` or `base` at step
    // 3 and is caught there rather than at the commit.
    //
    // `cc.txt` merges cleanly through the same virtual base and is staged by the
    // same invocation, so step 2's `M  cc.txt` beside `UU clash.txt` is the
    // assertion that the strategy did not simply give up at the first conflict.
    out.push(
        Sequence::new("merge", "criss-cross-conflict-resolve-continue", Shape::CrissCross)
            .step(&["merge", "cc-right"])
            .step(&["status", "--porcelain"])
            .step(&["show", ":1:clash.txt"])
            .step(&["show", ":2:clash.txt"])
            .step(&["show", ":3:clash.txt"])
            .step(&["checkout", "--ours", "--", "clash.txt"])
            .step(&["add", "clash.txt"])
            .step(&["merge", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-4"]),
    );

    // The same merge, thrown away, then re-run with a strategy option that
    // resolves the conflict instead of stopping. The pair is the point: step 3
    // reads the virtual base out of the stopped index, step 5 discards it, and
    // step 6 makes the identical merge succeed — so step 7's `show HEAD:clash.txt`
    // must be `a`, ours, the side `-X ours` names.
    //
    // `-X ours` over a criss-cross is the case that shipped wrong: a port that
    // builds no virtual base at all can still exit 0 here and commit *theirs*'
    // content, which every state file and every exit code agrees with. Step 7 is
    // one line of stdout and is the only thing that separates them.
    //
    // Steps 1-2 are the base enumeration that has to be right for any of it:
    // `--all` must return both `cc-a` and `cc-b`, and `--independent` must prune
    // the pair down to the two tips.
    out.push(
        Sequence::new("merge", "criss-cross-virtual-base-then-strategy-ours", Shape::CrissCross)
            .step(&["merge-base", "--all", "cc-left", "cc-right"])
            .step(&["merge-base", "--independent", "cc-left", "cc-right", "cc-a", "cc-b"])
            .step(&["merge", "cc-right"])
            .step(&["show", ":1:clash.txt"])
            .step(&["merge", "--abort"])
            .step(&["merge", "-X", "ours", "--no-edit", "cc-right"])
            .step(&["show", "HEAD:clash.txt"])
            .step(&["log", "--oneline", "--graph", "-3"]),
    );

    // The abort, cross-examined. `merge --abort` has to put `cc-left` back and
    // remove `MERGE_HEAD`/`MERGE_MSG`/`MERGE_MODE`/`AUTO_MERGE`, after which
    // both finishing verbs must refuse — `There is no merge to abort (MERGE_HEAD
    // missing).` and `There is no merge in progress (MERGE_HEAD missing).`, two
    // different sentences from two different gates.
    //
    // `strict`, because those two refusals are the tail and both exit 128 with
    // empty stdout. Safe to make strict here where it is not for the rebase
    // sequences below: every step of this one writes its diagnostics to stdout
    // and stock leaves stderr empty until step 5, so a port cannot be stopped at
    // step 1 by a progress line.
    out.push(
        Sequence::new("merge", "criss-cross-abort-then-refuse-resumption", Shape::CrissCross)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "cc-right"])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["merge", "--abort"])
            .step(&["merge", "--continue"]),
    );

    // `--quit` over the criss-cross stop. The state files go and the unmerged
    // index stays, so step 3 must still report `M  cc.txt` *and* `UU clash.txt`
    // — the clean half of the merge is still staged, which is the part a port
    // that implements `--quit` as `--abort` throws away along with everything
    // else. Steps 5-7 are the three refusals that prove it: two because the
    // state files are gone and one, differently worded, because the index is
    // still unmerged.
    out.push(
        Sequence::new("merge", "criss-cross-quit-keeps-the-index", Shape::CrissCross)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "cc-right"])
            .step(&["merge", "--quit"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["merge", "--abort"])
            .step(&["merge", "--continue"])
            .step(&["commit", "-m", "nope"]),
    );

    // rerere over a conflict whose preimage came out of a *virtual* base. The
    // recorded preimage is the normalised outer conflict — `a` against `b` with
    // the branch names stripped from the markers — and `probe_rr_cache` compares
    // it byte for byte at every step, so a port whose virtual base differs
    // records different bytes at step 1 and is named there rather than at the
    // replay.
    //
    // Step 8 is the replay and its outcome is not "the merge succeeds": stock
    // re-runs the same merge, rerere rewrites the file, and the merge still
    // exits 1 with `UU clash.txt` because the resolution was applied to the
    // worktree and not staged. Step 9 is what pins that — a port that treats a
    // replayed resolution as a resolution scores exit 0 and a clean tree.
    out.push(
        Sequence::new("rerere", "criss-cross-record-then-replay", Shape::CrissCross)
            .with_config(&[("rerere.enabled", "true")])
            .step(&["merge", "cc-right"])
            .step(&["rerere", "status"])
            .step(&["rerere", "diff"])
            .step(&["checkout", "--ours", "--", "clash.txt"])
            .step(&["add", "clash.txt"])
            .step(&["commit", "--no-edit"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["merge", "cc-right"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "forget", "clash.txt"])
            .step(&["status", "--porcelain"]),
    );

    // Rebasing across the criss-cross. `rebase cc-b` linearises `cc-left` onto
    // one of the two bases: the merge commit is dropped, `criss-cross: a` is
    // replayed first and conflicts on `clash.txt`, and one pick stays queued —
    // so this is a rebase stop whose premise is a two-base history rather than a
    // two-branch one. Step 5's `--continue` has to commit the resolution and
    // replay the remaining pick in the same invocation, which step 7's `log`
    // (four commits, no merge) is what says.
    //
    // Deliberately **not** `strict`: stock writes `Rebasing (1/2)` and the
    // `could not apply` line to stderr at step 1, so a stderr comparison would
    // stop the sequence at its first step and leave the resume unmeasured. The
    // exit code, the unmerged index and `rebase-merge/` are compared regardless.
    out.push(
        Sequence::new("rebase", "criss-cross-conflict-resolve-continue", Shape::CrissCross)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "cc-b"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "clash.txt"])
            .step(&["add", "clash.txt"])
            .step(&["rebase", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-5"]),
    );

    // The same stop, walked with `--skip`. Two picks and only the first
    // conflicts, so the skip drops it *and finishes the rebase in the same
    // invocation* — `rebase-merge/` is gone at step 3 and `cc-left` is three
    // commits long at step 4, with `criss-cross: a` absent. Step 5 is the proof
    // that the operation really ended rather than merely advancing: `fatal: no
    // rebase in progress`.
    out.push(
        Sequence::new("rebase", "criss-cross-skip-drops-the-pick-and-finishes", Shape::CrissCross)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "cc-b"])
            .step(&["rebase", "--skip"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-4"])
            .step(&["rebase", "--abort"])
            .step(&["log", "--oneline", "-4"]),
    );

    // The abort of the same stop, which has to restore a *merge* — `cc-left`'s
    // tip is a commit whose parent is the criss-cross merge, and putting it back
    // means putting both parents back. Step 5's `log --graph` is where a port
    // that resets to the first parent alone is caught; step 6 is where one that
    // left `rebase-merge/` behind is.
    out.push(
        Sequence::new("rebase", "criss-cross-abort-restores-the-merge", Shape::CrissCross)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "cc-b"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-5"])
            .step(&["rebase", "--continue"]),
    );

    // Cherry-picking the criss-cross *merge* onto the branch that already
    // contains both its parents' content. `-m 1` names the mainline, the pick
    // applies, and the result is **empty** — so git stops with `CHERRY_PICK_HEAD`
    // and `MERGE_MSG` written, `AUTO_MERGE` written, a clean worktree, and exit
    // 1. That is a sequencer stop with nothing unmerged to look at, which no
    // other sequence in this file reaches: every other stop here has an unmerged
    // index, and a port that keys "is something in progress" off the index alone
    // reports no operation at step 2.
    //
    // Step 3's `--continue` must re-report the same emptiness rather than
    // committing it; step 5's `--skip` is the way out and leaves `cc-left`
    // unmoved, which step 6's `log` says.
    out.push(
        Sequence::new("cherry-pick", "criss-cross-pick-a-merge-goes-empty", Shape::CrissCross)
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry-pick", "-m", "1", "cc-right~1"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--continue"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cherry-pick", "--skip"])
            .step(&["log", "--oneline", "-2"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--abort"]),
    );

    // Picking the far tip instead, which merges cleanly through the same two
    // bases and commits in one invocation — so nothing is parked and step 4's
    // `cherry-pick --abort` must refuse with `error: no cherry-pick or revert in
    // progress` / `fatal: cherry-pick failed`.
    //
    // Run beside `criss-cross-pick-a-merge-goes-empty` because the two differ
    // only in whether the pick had anything to do: a port that parks a sequencer
    // for every pick passes the empty case and acts at step 4 here.
    //
    // `strict`, because that refusal is the tail, exits 128 and prints nothing
    // on stdout; stock leaves stderr empty on steps 1-3.
    out.push(
        Sequence::new("cherry-pick", "criss-cross-pick-the-far-tip-parks-nothing", Shape::CrissCross)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry-pick", "cc-right"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cherry-pick", "--abort"]),
    );
}

// ---------------------------------------------------------------------------
// unrelated histories: two roots, and the operations that join them
// ---------------------------------------------------------------------------
//
// [`Shape::Unrelated`] is checked out on `main`, clean, beside two orphan
// branches: `alien`, which shares no path with `main`, and `alien-clash`, which
// carries its own `README.md`. Every pair of revisions across that boundary has
// **no** merge base, so `merge` and `pull` refuse until told otherwise and
// `merge-base` exits 1 with no output.
//
// The refusal is a single invocation and is already covered by the case corpus.
// What is not is what happens *after* the flag is given, and — the sharper
// question — what happens after `replace --graft` makes the two histories
// related, which changes the answer for every later command.

fn unrelated(out: &mut Vec<Sequence>) {
    // The documented recovery, end to end: the refusal, then the same merge with
    // `--allow-unrelated-histories`. `alien` shares no path with `main`, so the
    // allowed merge is *clean* and the resulting tree is the union of two roots
    // — step 7's `rev-list --max-parents=0 --count` must be `2`, which is a fact
    // only a two-root repository can produce and which a port that quietly
    // fast-forwarded instead gets wrong.
    //
    // `strict`, because step 1's `fatal: refusing to merge unrelated histories`
    // is the contract the rest of the workflow exists to lift, and it exits 128
    // with empty stdout. Stock leaves stderr empty on every other step here.
    out.push(
        Sequence::new("merge", "unrelated-refused-then-allowed", Shape::Unrelated)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "alien"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["merge", "--allow-unrelated-histories", "--no-edit", "alien"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-5"])
            .step(&["rev-list", "--max-parents=0", "--count", "HEAD"]),
    );

    // The other outcome of the same flag. `alien-clash` carries its own
    // `README.md`, so the allowed merge is an **add/add conflict between two
    // roots**: there is no common ancestor, so the index gets stages 2 and 3 and
    // *no stage 1* — step 3's `ls-files -u` is two lines, not three, which is
    // the one place in this corpus where a conflict legitimately has no base.
    // A port that synthesises an empty stage 1 prints three lines there.
    out.push(
        Sequence::new("merge", "unrelated-add-add-resolve-continue", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--allow-unrelated-histories", "alien-clash"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "-u"])
            .step(&["checkout", "--theirs", "--", "README.md"])
            .step(&["add", "README.md"])
            .step(&["merge", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-4"]),
    );

    // The abort of that stop. A merge between two roots writes the same state
    // files as any other, so `--abort` has to remove them and put `main` back —
    // and both finishing verbs must then refuse from their own gates.
    out.push(
        Sequence::new("merge", "unrelated-add-add-abort-then-refuse", Shape::Unrelated)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "--allow-unrelated-histories", "alien-clash"])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"])
            .step(&["merge", "--abort"])
            .step(&["merge", "--continue"]),
    );

    // `pull` down the same road, which is the composite: fetch from the
    // repository itself by relative URL, then merge what `FETCH_HEAD` names. The
    // refusal at step 1 arrives *after* the fetch has already written
    // `FETCH_HEAD` and printed its `* branch alien -> FETCH_HEAD` line, so a
    // port that checks relatedness before fetching prints nothing and is caught
    // on stderr; one that never fetches at all is caught at step 3, where the
    // second `pull` has to succeed against the same URL.
    //
    // Not `strict`: the refusal is step 1 of 5 and a stderr mismatch there would
    // hide the allowed pull behind it. `--no-advice` is on the envelope so the
    // refusal stays the refusal.
    out.push(
        Sequence::new("pull", "unrelated-refused-then-allowed", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["pull", "--no-rebase", ".", "alien"])
            .step(&["status", "--porcelain"])
            .step(&["pull", "--no-rebase", "--allow-unrelated-histories", "--no-edit", ".", "alien"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-5"]),
    );

    // `rebase --onto <other root> --root`, which replays a *root commit* onto a
    // history it shares nothing with — the one way to move commits across the
    // boundary without a merge. Onto `alien` it is clean, because the two sides
    // touch disjoint paths, and the result is a single-root history four commits
    // long: step 3's `log` is the assertion, and step 4's refusal is what says
    // the rebase ended rather than parking.
    out.push(
        Sequence::new("rebase", "unrelated-root-replayed-onto-the-far-root", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "--onto", "alien", "--root", "main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-5"])
            .step(&["rebase", "--continue"])
            .step(&["log", "--oneline", "-5"])
            .step(&["rev-list", "--max-parents=0", "--count", "HEAD"]),
    );

    // The same replay onto the root that *does* collide. `initial` and `alien
    // clash root` both add `README.md`, so the first of the two picks stops on an
    // add/add conflict with one pick still queued — a rebase stop whose conflict
    // has no ancestor, which no other shape can produce. Step 4's `--continue`
    // has to commit the resolution and replay the remaining pick.
    out.push(
        Sequence::new("rebase", "unrelated-root-onto-clash-resolve-continue", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "--onto", "alien-clash", "--root", "main"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "--theirs", "--", "README.md"])
            .step(&["add", "README.md"])
            .step(&["rebase", "--continue"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-4"]),
    );

    // The abort of that stop. `main` was rewritten from its *root*, so putting it
    // back is not "reset one commit" — step 4's `log` has to be the fixture's two
    // commits again, `edfab1b initial` included, and step 5 has to find no rebase.
    out.push(
        Sequence::new("rebase", "unrelated-root-onto-clash-abort", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "--onto", "alien-clash", "--root", "main"])
            .step(&["rebase", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"])
            .step(&["rebase", "--continue"])
            .step(&["status", "--porcelain"]),
    );

    // `format-patch` across the boundary, then `am` of what it wrote. `--root`
    // is required because `alien` has no ancestor to bound the range with, and
    // it makes the *root commit itself* a patch — the `--- /dev/null` creation
    // form for every file in the tree. Step 2 names the two files it produced on
    // stdout; steps 4 and 7 read those files back by the names step 2 printed,
    // which is a dependency between steps that no single case has: a port whose
    // `format-patch` numbers or slugs a subject differently fails at step 4 with
    // `does not exist` rather than silently producing a different file.
    //
    // The `am` lands on `landing`, forked from `main`, so the patches apply to a
    // tree they were never generated against — and both apply cleanly, because
    // `alien` shares no path with `main`.
    out.push(
        Sequence::new("format-patch", "unrelated-format-patch-then-am-across-roots", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["format-patch", "--stdout", "main..alien"])
            .step(&["format-patch", "-o", "out", "--root", "alien"])
            .step(&["checkout", "-b", "landing", "main"])
            .step(&["am", "out/0001-alien-root.patch"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"])
            .step(&["am", "out/0002-alien-second.patch"])
            .step(&["log", "--oneline", "-4"])
            .step(&["am", "--skip"]),
    );

    // `replace --graft`, which is the only thing in git that makes two unrelated
    // histories related — and it does so for *every later command* rather than
    // for the one that ran it, which is precisely what a single case cannot show.
    //
    // Step 1 is the before: `merge-base main alien` exits 1 and prints nothing.
    // Step 2 grafts `alien root` onto `main`'s tip. From then on the same
    // question has an answer (step 5), `alien` walks through `main`'s commits
    // (step 4), and the repository has *one* root rather than two (step 6). Step
    // 7 takes the replacement back off and step 8 must return to exit 1 — a port
    // whose `replace -d` leaves the ref, or whose object reader caches the
    // replaced parent, keeps answering.
    out.push(
        Sequence::new("replace", "unrelated-graft-joins-two-roots", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["merge-base", "main", "alien"])
            .step(&["replace", "--graft", "alien~1", "main"])
            .step(&["replace", "-l"])
            .step(&["log", "--oneline", "alien"])
            .step(&["merge-base", "main", "alien"])
            .step(&["rev-list", "--max-parents=0", "--count", "alien"])
            .step(&["replace", "-d", "alien~1"])
            .step(&["merge-base", "main", "alien"]),
    );

    // The same graft, aimed at the refusal instead. Step 1 is
    // `refusing to merge unrelated histories`; step 3 is the **identical argv**
    // succeeding — and not as a merge but as a *fast-forward*, because the graft
    // made `alien` a descendant of `main`. Stock's step 3 deletes `README.md` and
    // `src/lib.rs` from the worktree and moves `main` to `alien`'s tip.
    //
    // Step 5 then removes the replacement, and step 6 is the finding the abort
    // sequences elsewhere in this file exist for: the history `main` now points
    // at is genuinely the alien root's, two commits long, and un-grafting does
    // not put the deleted files back. That is stock's behaviour and it is
    // measured rather than avoided — a port that keeps the pre-graft parent
    // cached prints four commits at step 6.
    out.push(
        Sequence::new("replace", "unrelated-graft-turns-a-refusal-into-a-fast-forward", Shape::Unrelated)
            .with_globals(&[&["--no-advice"]])
            .step(&["merge", "alien"])
            .step(&["replace", "--graft", "alien~1", "main"])
            .step(&["merge", "--no-edit", "alien"])
            .step(&["log", "--oneline", "--graph", "-5"])
            .step(&["replace", "-d", "alien~1"])
            .step(&["log", "--oneline", "--graph", "-5"])
            .step(&["status", "--porcelain"])
            .step(&["merge-base", "main", "alien"]),
    );
}

// ---------------------------------------------------------------------------
// cherry: one patch, two commits, and what replays it a second time
// ---------------------------------------------------------------------------
//
// [`Shape::Cherry`] is checked out on `topic`, clean. `main` and `topic` each
// hold a commit the other does not, plus one commit whose *patch id* both have —
// `topic`'s copy was made by `cherry-pick` onto a different parent, so the two
// commits differ in every byte except the diff they carry.
//
// That duplicate is the only way to reach git's "already applied" paths, and
// every one of them is a decision made by one invocation about work a *previous*
// one did: `rebase` drops the commit before it replays anything, `cherry-pick`
// stops with an empty result, `am --3way` reports `No changes` and exits 0. What
// a sequence adds is what each of those leaves behind for the next command.

fn cherry(out: &mut Vec<Sequence>) {
    // The duplicate, named three ways, then rebased away. Steps 1-2 are the two
    // readers that can see it — `cherry`'s `-` marker and `--cherry-mark`'s `=`
    // class — and step 3 is the writer: `rebase main` builds a todo of *two*
    // picks rather than three, because `topic`'s copy of the shared patch is
    // dropped at todo-generation time with `warning: skipped previously applied
    // commit`.
    //
    // Step 6 is the assertion that closes the loop: after the rebase, `cherry`
    // must report only `+` lines, because the duplicate is gone rather than
    // duplicated again. A port that replays all three commits prints a `-` line
    // there and a five-commit history at step 5.
    out.push(
        Sequence::new("rebase", "cherry-already-applied-is-dropped", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry", "-v", "main"])
            .step(&["rev-list", "--cherry-mark", "--left-right", "main...topic"])
            .step(&["rebase", "main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-5"])
            .step(&["cherry", "-v", "main"]),
    );

    // The same rebase, undone, then re-run with the two flags that change what
    // "already applied" means — and the undo is the step under test. `rebase`
    // writes `ORIG_HEAD`, so step 3 can put `topic` back without naming an id;
    // step 4 must show the fixture's history again, `7a4b88a cherry: shared
    // patch` included. A port whose rebase never writes `ORIG_HEAD` fails at step
    // 3 with `unknown revision`, which is a defect no single case can see because
    // the file is written by the invocation before.
    //
    // Step 5 is then the three-way contrast in one argv:
    // `--reapply-cherry-picks` puts the duplicate *into* the todo (three picks,
    // not two) and `--empty=keep` stops it being dropped when it applies to
    // nothing — so stock's step 6 is six commits with `cherry: shared patch`
    // appearing **twice**, once from `main` and once as an empty commit.
    out.push(
        Sequence::new("rebase", "cherry-orig-head-then-reapply-keeps-the-empty", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["rebase", "main"])
            .step(&["log", "--oneline", "-5"])
            .step(&["reset", "--hard", "ORIG_HEAD"])
            .step(&["log", "--oneline", "-5"])
            .step(&["rebase", "--reapply-cherry-picks", "--empty=keep", "main"])
            .step(&["log", "--oneline", "-6"])
            .step(&["status", "--porcelain"]),
    );

    // `cherry-pick` of the commit whose patch `topic` already carries. The pick
    // applies to nothing, so git stops with exit 1, a **clean worktree**, and
    // `CHERRY_PICK_HEAD` written — the sequencer's empty-result stop, which is
    // not a conflict and which a port that only parks state on conflicts never
    // reaches.
    //
    // Step 4's `--continue` must re-report the emptiness rather than committing
    // it — the stop survives its own resumption verb — and step 6's `--skip` is
    // the way out, after which `topic` is exactly where step 3 found it. That
    // last equality is the finding: a `--skip` that resets instead of dropping
    // the pick loses `cherry: topic only`.
    out.push(
        Sequence::new("cherry-pick", "cherry-already-applied-empty-stop-skip", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry-pick", "main~1"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"])
            .step(&["cherry-pick", "--continue"])
            .step(&["log", "--oneline", "-3"])
            .step(&["cherry-pick", "--skip"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-3"]),
    );

    // The same pick under the two `--empty` modes that do not stop, run back to
    // back so the second's premise is the first's result. `--empty=drop` exits 0,
    // prints `dropping <oid> … -- patch contents already upstream` on stderr and
    // leaves `topic` unmoved; `--empty=keep` exits 0 and commits an **empty**
    // `cherry: shared patch` on top. Same argv but for one word, opposite effect
    // on history, and step 5's `log` is where a port that treats the two modes
    // alike is caught.
    out.push(
        Sequence::new("cherry-pick", "cherry-empty-drop-then-empty-keep", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry-pick", "--empty=drop", "main~1"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cherry-pick", "--empty=keep", "main~1"])
            .step(&["log", "--oneline", "-3"])
            .step(&["status", "--porcelain"])
            .step(&["cherry-pick", "--abort"]),
    );

    // `am` of a patch the branch already carries, in both of its forms, over a
    // mailbox this sequence generates for itself. Step 1 writes it; nothing else
    // in the corpus can, because a case is one argv against a pristine copy.
    //
    // Step 2 is the plain apply, which fails at the *text* level — the context
    // lines no longer match — with exit 128, and parks `.git/rebase-apply/`.
    // Step 3 reads the parked patch back out. Step 4 clears it. Step 6 is the
    // same mailbox with `--3way`, which reconstructs the pre-image from the
    // `index` line, finds the change already present, prints `No changes --
    // Patch already applied.` and exits **0** while parking nothing — and step 9's
    // refusal is the proof that nothing was parked.
    //
    // The pair is the point: the same bytes are a hard failure in one mode and a
    // silent success in the other, and a port that shares one apply path between
    // them gets exactly one of the two right.
    out.push(
        Sequence::new("am", "cherry-already-applied-plain-then-three-way", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["format-patch", "-o", "out", "main~2..main~1"])
            .step(&["am", "out/0001-cherry-shared-patch.patch"])
            .step(&["am", "--show-current-patch=diff"])
            .step(&["am", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["am", "--3way", "out/0001-cherry-shared-patch.patch"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"])
            .step(&["am", "--skip"]),
    );

    // `am --empty`, all three modes, over a mailbox that carries no diff at all.
    // Steps 1-3 manufacture it — an empty commit, `format-patch --always` (which
    // is what makes a patchless mail rather than no file), then `reset --hard` so
    // the mail has somewhere to land — and none of those three can be done by a
    // case.
    //
    // The three modes then run against the identical file: `drop` prints
    // `Skipping: empty-mail` and exits 0 leaving history alone; `keep` prints
    // `Creating an empty commit: empty-mail` and exits 0 having made one; `stop`
    // prints `Patch is empty.`, exits 128 and **parks** `rebase-apply/`, which is
    // the one of the three that leaves an operation in progress and which step 11
    // has to clear. A port with one behaviour for all three passes a third of
    // this and is named at the step that diverged.
    out.push(
        Sequence::new("am", "cherry-empty-mail-drop-keep-stop", Shape::Cherry)
            .with_globals(&[&["--no-advice"]])
            .step(&["commit", "--allow-empty", "-m", "empty-mail"])
            .step(&["format-patch", "-o", "out", "--always", "-1"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["am", "--empty=drop", "out/0001-empty-mail.patch"])
            .step(&["log", "--oneline", "-2"])
            .step(&["am", "--empty=keep", "out/0001-empty-mail.patch"])
            .step(&["log", "--oneline", "-2"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["am", "--empty=stop", "out/0001-empty-mail.patch"])
            .step(&["status", "--porcelain"])
            .step(&["am", "--abort"])
            .step(&["log", "--oneline", "-2"]),
    );
}

// ---------------------------------------------------------------------------
// damaged: maintenance verbs over a repository that is already broken
// ---------------------------------------------------------------------------
//
// [`Shape::Damaged`] carries a ref to a missing object, a dangling symref, a
// loose object file that is not a zlib stream, and an empty `alternates` entry.
// Stock git operates on it — `log` walks, `status` is clean — and every
// maintenance verb refuses in its own way.
//
// This is the one shape where a port can *destroy data*, so every sequence below
// ends in a read that says what survived: `cat-file --batch-all-objects
// --batch-check` enumerates the object set (the corrupt one included, as
// `missing`) and `log` proves the two commits are still walkable. Stock's answer
// to all four workflows is the same and it is worth stating plainly: it refuses,
// and it deletes nothing. A port that "repairs" by dropping the corrupt object,
// or that repacks past the broken ref, diverges on the last step rather than the
// first.

fn damaged(out: &mut Vec<Sequence>) {
    // `fsck` → `gc` → `fsck`. Stock's `gc` refuses at step 2 — `error:
    // refs/heads/dangling does not point to a valid object!` then `fatal: bad
    // object refs/heads/dangling` then `fatal: failed to run repack`, exit 128 —
    // *before* repacking anything, so step 3's `fsck` must report the identical
    // five errors step 1 did and step 5 must list the identical nine objects.
    //
    // The question the sequence asks is whether the port's `gc` makes it worse.
    // A `gc` that skips the unreadable ref and repacks anyway exits 0 here, which
    // looks better than stock and is the failure: the corrupt loose object is
    // then either copied into a pack or deleted, and either answer shows up at
    // step 5 as a different object listing.
    out.push(
        Sequence::new("fsck", "damaged-fsck-gc-fsck-changes-nothing", Shape::Damaged)
            .with_globals(&[&["--no-advice"]])
            .step(&["fsck", "--no-progress"])
            .step(&["gc", "--quiet"])
            .step(&["fsck", "--no-progress"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["for-each-ref", "--format=%(refname)"]),
    );

    // The same `gc`, given the one repair a sequence can perform. `branch -D
    // dangling` removes the ref to the missing object — stock prints `Deleted
    // branch dangling (was deadbee).`, abbreviating an id it cannot resolve —
    // after which `rev-list --all` works (step 4) and `gc` gets *further* before
    // failing: `fatal: unable to add cruft objects` rather than `bad object`,
    // still exit 128, still nothing deleted.
    //
    // That progression is the finding. A port whose `branch -D` refuses to delete
    // a ref it cannot resolve fails at step 2 and never reaches the second `gc`
    // at all; one whose `gc` succeeds where stock's cruft pass gives up has
    // written a pack stock would not have, which step 7's object listing sees.
    out.push(
        Sequence::new("gc", "damaged-ref-deleted-then-gc-still-refuses", Shape::Damaged)
            .with_globals(&[&["--no-advice"]])
            .step(&["branch", "--list"])
            .step(&["branch", "-D", "dangling"])
            .step(&["branch", "--list"])
            .step(&["rev-list", "--all", "--count"])
            .step(&["gc", "--quiet"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["fsck", "--no-progress"]),
    );

    // `prune`, which is the verb with the most to lose: its whole job is to
    // delete unreachable objects, and this repository's reachability cannot be
    // computed. Stock refuses three times for two different reasons — `fatal:
    // unable to parse object: refs/heads/dangling` while the ref is there, and
    // `fatal: unable to mark recent objects` once it is gone and the corrupt
    // loose object is what stops the walk — and the `--dry-run` at step 3 and the
    // real `prune` at step 4 give the *same* answer, which is the property that
    // matters: a dry run that refuses and a real run that proceeds is the worst
    // possible pairing and is exactly what step 5's object listing would catch.
    out.push(
        Sequence::new("prune", "damaged-prune-dry-run-and-real-both-refuse", Shape::Damaged)
            .with_globals(&[&["--no-advice"]])
            .step(&["prune", "--dry-run", "--expire=all"])
            .step(&["branch", "-D", "dangling"])
            .step(&["prune", "--dry-run", "--expire=all"])
            .step(&["prune", "--expire=all"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["log", "--oneline", "-2"])
            .step(&["fsck", "--no-progress"]),
    );

    // `repack -ad`, and the one maintenance verb here that *succeeds*. With the
    // broken ref present it dies at `fatal: bad object refs/heads/dangling`
    // (step 1) and leaves the nine loose objects alone (step 2). With the ref
    // gone it exits **0** (step 4) and packs the reachable objects — and the
    // corrupt loose object survives untouched, because `-d` deletes only what it
    // packed and it could not read that one. Step 5's listing must still name
    // `ab1234…` as `missing` and step 6 must still walk both commits.
    //
    // The success is what makes this worth a sequence: a port that refuses at
    // step 4 diverges, and so does one that succeeds while deleting the object it
    // failed to read. Both are single-step facts that only exist because step 2
    // removed the ref first.
    out.push(
        Sequence::new("repack", "damaged-repack-refuses-then-packs-what-it-can", Shape::Damaged)
            .with_globals(&[&["--no-advice"]])
            .step(&["repack", "-ad"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["branch", "-D", "dangling"])
            .step(&["repack", "-ad"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["log", "--oneline", "-2"])
            .step(&["fsck", "--no-progress"]),
    );

    // The dangling *symref*, which is a different kind of damage from the
    // dangling id and is handled by a different set of answers. `symbolic-ref`
    // reads it happily (step 1, `refs/heads/does-not-exist` on stdout);
    // `rev-parse --verify` warns and fails (step 2); `branch -D` deletes **the
    // symref itself** rather than its target and says so — `Deleted branch
    // broken-symref (was refs/heads/does-not-exist).`, a "was" that is a ref name
    // rather than an id (step 3).
    //
    // Steps 5-7 are the after: `symbolic-ref -d` must now refuse because there is
    // nothing there, `show-ref` must still fail on the *other* damage, and
    // `for-each-ref` must still succeed and list two refs. Those three
    // disagreeing about the same repository is the fixture's whole point, and
    // only a sequence can ask them in an order where the second damage is all
    // that is left.
    out.push(
        Sequence::new("branch", "damaged-broken-symref-deleted-by-branch-d", Shape::Damaged)
            .with_globals(&[&["--no-advice"]])
            .step(&["symbolic-ref", "refs/heads/broken-symref"])
            .step(&["rev-parse", "--verify", "refs/heads/broken-symref"])
            .step(&["branch", "-D", "broken-symref"])
            .step(&["branch", "--list"])
            .step(&["symbolic-ref", "-d", "refs/heads/broken-symref"])
            .step(&["show-ref"])
            .step(&["for-each-ref", "--format=%(refname)"])
            .step(&["fsck", "--no-progress"]),
    );
}

// ---------------------------------------------------------------------------
// symlinks: mode 120000 across a workflow
// ---------------------------------------------------------------------------
//
// [`Shape::Symlinks`] is checked out on `main` with `link-wt` retargeted in the
// worktree and two untracked entries beside it, one of which is itself a
// symlink. `sym-pending` holds the same tree with `dir/target.txt` replaced by a
// symlink — a **typechange**, `T` in `--raw` — and `patches/symlink.patch`
// describes exactly that difference.
//
// A single case can ask `ls-files --stage` what mode a path has. What it cannot
// do is move a path between `100644` and `120000` and then ask again, which is
// the transition every sequence below is built around.

fn symlinks(out: &mut Vec<Sequence>) {
    // The typechange, walked in both directions. `dir/target.txt` is a regular
    // file at step 2, a symlink at step 5, and a regular file again at step 8 —
    // and the dirt the fixture ships with has to survive all three: `link-wt` is
    // reported modified at every `status`, and stock's `checkout` prints
    // `M\tlink-wt` on stdout as it carries the edit across.
    //
    // A port that writes the symlink's *target text* into a regular file gets the
    // mode wrong at step 5, and one that refuses to carry the dirty symlink
    // across the branch switch fails at step 3 before anything else is measured.
    out.push(
        Sequence::new("checkout", "symlink-typechange-across-branches", Shape::Symlinks)
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "--stage", "dir/target.txt"])
            .step(&["checkout", "sym-pending"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "--stage", "dir/target.txt"])
            .step(&["diff", "--raw", "main", "sym-pending"])
            .step(&["checkout", "main"])
            .step(&["ls-files", "--stage", "dir/target.txt"])
            .step(&["status", "--porcelain"]),
    );

    // `checkout -- <path>` over a retargeted symlink: the index still holds the
    // committed target, so restoring the path means *replacing a symlink with a
    // different symlink* rather than rewriting a file's bytes. Step 3 must leave
    // the tree clean but for the two untracked entries, and step 4's `diff` must
    // be empty.
    //
    // Step 5 is the tail and the contract: `stray-link` is an untracked symlink,
    // so `checkout --` has nothing in the index to restore it from and must
    // refuse with `error: pathspec 'stray-link' did not match any file(s) known
    // to git`, exit 1. A port whose directory walk treats a symlink as a tracked
    // path answers something else there.
    //
    // `strict`, because that refusal is the tail and carries no stdout; stock
    // leaves stderr empty on steps 1-4.
    out.push(
        Sequence::new("checkout", "symlink-restore-then-untracked-refusal", Shape::Symlinks)
            .strict()
            .with_globals(&[&["--no-advice"]])
            .step(&["ls-files", "--stage", "link-wt"])
            .step(&["checkout", "--", "link-wt"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--stat"])
            .step(&["checkout", "--", "stray-link"]),
    );

    // `stash push -u` over a worktree whose dirt *is* symlinks: one tracked link
    // retargeted, one untracked link, and one untracked zero-byte file. The
    // stash has to store all three as objects — the untracked half in its own
    // third commit — and `pop` has to put all three back as the same modes.
    //
    // Step 4 is the assertion in the middle: with the edit stashed, `link-wt`'s
    // index entry must be the *committed* target blob, mode `120000`. A port that
    // stashed the symlink by reading through it stores `README.md`'s contents and
    // step 4 shows a `100644`. Step 9's empty `stash list` proves the pop
    // consumed the entry rather than leaving it.
    out.push(
        Sequence::new("stash", "symlink-push-untracked-then-pop", Shape::Symlinks)
            .step(&["status", "--porcelain"])
            .step(&["stash", "push", "-u", "-m", "links"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "--stage", "link-wt"])
            .step(&["stash", "show", "--stat"])
            .step(&["stash", "pop"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "--stage", "link-wt"])
            .step(&["stash", "list"]),
    );

    // `apply --index` of the fixture's own symlink patch, committed, then
    // reversed. The patch does three things no other patch in this harness does:
    // it replaces a regular file with a symlink (`T`), it creates a symlink, and
    // it creates a **zero-byte** file — so step 3 must show
    // `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`, the empty blob, which is a
    // constant of the hash function a port has to be able to write rather than
    // derive.
    //
    // Step 5's `commit` is what turns the staged typechange into history and
    // prints `mode change 100644 => 120000 dir/target.txt`; step 7 reverses the
    // same patch against the tree the commit made, which is the half `apply -R`
    // is usually never asked for because nothing committed in between.
    out.push(
        Sequence::new("apply", "symlink-patch-index-commit-then-reverse", Shape::Symlinks)
            .step(&["apply", "--index", "patches/symlink.patch"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "--stage", "dir/target.txt", "later-link", "later-empty.txt"])
            .step(&["diff", "--cached", "--raw"])
            .step(&["commit", "-m", "applied-symlinks"])
            .step(&["log", "--oneline", "-2"])
            .step(&["apply", "-R", "--index", "patches/symlink.patch"])
            .step(&["status", "--porcelain"]),
    );

    // `archive`, then the archive read back **through git**. Step 1 writes a tar
    // of a tree holding six symlinks and two empty files; step 2 hashes it into
    // the object store and prints the id; step 4 stages it, so step 5 prints the
    // same id again out of the index. Two independent readings of the same bytes.
    //
    // That is what makes this a read-back rather than a write nobody checks: git
    // cannot extract a tar, so the only way to compare two implementations' tar
    // output inside this harness is to turn it into an object id. A single byte
    // of difference — a `lrwxrwxrwx` entry stored as a regular file, a mode, the
    // pinned mtime, the padding — moves the id at step 2, and `probe_storage`
    // sees the extra loose object at every step after it.
    out.push(
        Sequence::new("archive", "symlink-archive-hashed-back-in", Shape::Symlinks)
            .step(&["archive", "--format=tar", "-o", "arc.tar", "HEAD"])
            .step(&["hash-object", "-w", "arc.tar"])
            .step(&["status", "--porcelain"])
            .step(&["add", "arc.tar"])
            .step(&["ls-files", "--stage", "arc.tar"])
            .step(&["commit", "-m", "archived"])
            .step(&["log", "--oneline", "-2"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// commit-graph: a cache that is written by one command and trusted by the next
// ---------------------------------------------------------------------------
//
// [`Shape::CommitGraph`] ships `.git/objects/info/commit-graph` covering every
// commit but the last: `cg-late` was committed *after* the write, so the graph
// is valid and incomplete. Every traversal therefore mixes graph-supplied
// generation numbers with computed ones, and `commit-graph verify` has to accept
// it — an incomplete graph is not a corrupt one.
//
// A case can run one of these verbs. It cannot ask whether the file one verb
// wrote is the file the next one reads, which is the only thing a commit-graph
// is for.

fn commit_graph(out: &mut Vec<Sequence>) {
    // The staleness question, asked twice. Step 1 verifies the shipped graph
    // (exit 0, silent, even though `cg-late` is outside it); step 3 adds another
    // commit outside it; step 4 must *still* verify, because being behind is not
    // being wrong. Step 6 rewrites it and step 7 verifies the new one.
    //
    // `log` runs on both sides of every write, because the traversal is what
    // consumes the file: a port that rebuilds the graph incorrectly at step 6 —
    // wrong generation numbers, a commit's parents mis-recorded — reports the
    // same exit code at step 7 and a differently ordered history at step 8.
    out.push(
        Sequence::new("commit-graph", "stale-graph-verifies-then-is-rewritten", Shape::CommitGraph)
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "-4"])
            .step(&["commit", "--allow-empty", "-m", "after-graph"])
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "-4"])
            .step(&["commit-graph", "write", "--reachable", "--changed-paths"])
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "-4"])
            .step(&["rev-list", "--count", "HEAD"]),
    );

    // The **split** graph, which is a different on-disk layout for the same
    // answers: `--split` replaces `objects/info/commit-graph` with
    // `objects/info/commit-graphs/` holding a `commit-graph-chain` and one
    // `graph-<hash>.graph` per layer. `probe_storage` enumerates `objects/info`
    // rather than matching a whitelist, so the move — a file disappearing and a
    // directory appearing — is compared at the step that made it.
    //
    // Step 5 is the step under test and it is deliberately not step 1: steps 1-4
    // re-verify the shipped graph, rewrite it whole, verify that, and commit past
    // it, so four supported transitions are measured before the layout question
    // is asked. A sequence that opened with `--split` would report one fact and
    // nothing else.
    //
    // Stock writes a **two**-layer chain there, because step 2 left a full graph
    // for the split write to keep as its base layer, and step 6 has to accept the
    // chain — a chain whose layers disagree is the failure mode this layout has
    // and no other does.
    out.push(
        Sequence::new("commit-graph", "split-chain-over-a-written-graph", Shape::CommitGraph)
            .step(&["commit-graph", "verify"])
            .step(&["commit-graph", "write", "--reachable"])
            .step(&["commit-graph", "verify"])
            .step(&["commit", "--allow-empty", "-m", "more"])
            .step(&["commit-graph", "write", "--reachable", "--split"])
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "-3"])
            .step(&["rev-list", "--count", "HEAD"]),
    );

    // `gc` over a repository that has a commit-graph. `gc` repacks *and* refreshes
    // the graph, so this asks whether the port's `gc` leaves a graph that still
    // describes the objects it just moved into a pack — a stale graph pointing at
    // a repacked object store is the classic way this cache goes wrong, and it is
    // invisible to every command except the one that reads it.
    //
    // Steps 1 and 6 are the same `log -- <path>` query on either side of the
    // collect, and they must give the same one-line answer. That query is what
    // consumes the `--changed-paths` Bloom filters the fixture wrote: a `gc` that
    // rewrites the graph *without* them still verifies at step 5 and still
    // answers step 6 correctly, only slower — so this sequence deliberately does
    // not claim to measure their presence, only that the graph survives usable.
    out.push(
        Sequence::new("gc", "commit-graph-survives-a-collect", Shape::CommitGraph)
            .step(&["log", "--oneline", "--", "cg-side.txt"])
            .step(&["commit-graph", "verify"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["gc", "--prune=all", "--quiet"])
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "--", "cg-side.txt"])
            .step(&["log", "--oneline", "-3"])
            .step(&["rev-list", "--count", "--all"]),
    );

    // The harder half of the same question: `gc` after the objects the graph
    // describes have become **unreachable**. `cg-loose` is the fixture's
    // never-merged fork; deleting it and expiring the reflogs makes its commit
    // prunable, and `gc --prune=all` at step 3 both removes it and must rewrite
    // the graph without it.
    //
    // Step 4's `verify` is where a graph that still names a pruned commit is
    // caught, step 5's count must be 9 rather than 10, and step 7's `fsck
    // --no-dangling` must be silent — a graph referencing a missing commit is
    // exactly the corruption `verify` exists to find, and a port that prunes the
    // object while leaving the file alone passes every other probe in this
    // harness.
    out.push(
        Sequence::new("gc", "commit-graph-rewritten-after-a-prune", Shape::CommitGraph)
            .step(&["branch", "-D", "cg-loose"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["gc", "--prune=all", "--quiet"])
            .step(&["commit-graph", "verify"])
            .step(&["rev-list", "--all", "--count"])
            .step(&["log", "--oneline", "--all"])
            .step(&["fsck", "--no-progress", "--no-dangling"]),
    );
}
