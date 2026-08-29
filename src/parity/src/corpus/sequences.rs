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
//!  * **The record one command leaves for the next.** Not every stateful thing
//!    git writes is an operation in progress. A rerere resolution, a
//!    `.git/shallow` graft, a pack's `.promisor` mark, a hook's own output, a
//!    lock file under `.git/worktrees/`, a notes tree, an intent-to-add entry —
//!    each is written by one invocation and consulted by a later one, and none
//!    of them is reachable by a case, which is one argv against a pristine copy.
//!    The families at the end of this file (hooks, rerere, shallow, promisor,
//!    notes/replace, worktree locks, tag chains, intent-to-add and pending
//!    renames) are all of that kind: the interesting step is never the first
//!    one, because its premise has to be built and then agreed on.
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
    hooks_fail(&mut s);
    rerere_family(&mut s);
    shallow(&mut s);
    promisor(&mut s);
    notes_replace(&mut s);
    worktree_locked(&mut s);
    tag_chain(&mut s);
    intent_to_add(&mut s);
    maintenance_workflow(&mut s);
    reflog_as_a_resource(&mut s);
    worktree_across_commands(&mut s);
    config_drives_the_next_step(&mut s);
    update_ref_transactions(&mut s);
    side_records(&mut s);
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

/// The transaction from [`UPDATE_REF_TXN_SYMREF`], thrown away at `abort`.
///
/// `prepare` has already taken every lock by the time `abort` arrives, so this
/// is the one shape that separates "took the locks and released them" from
/// "never took them": a port that applies at `prepare` leaves both refs behind,
/// and a port that never locks at all prints the same three `ok` lines as one
/// that did.
const UPDATE_REF_TXN_ABORTED: &[u8] = b"start\n\
create refs/heads/txn-sym-a HEAD\n\
symref-create refs/heads/txn-sym refs/heads/main\n\
prepare\n\
abort\n";

/// The same two creations, committed — one ordinary ref and one **symbolic**
/// ref, inside a single transaction.
///
/// `symref-create` is the half a port is most likely to write as an ordinary
/// ref: `for-each-ref` prints the same `%(objectname)` either way, and only
/// `%(symref)` and `symbolic-ref` tell them apart.
const UPDATE_REF_TXN_SYMREF: &[u8] = b"start\n\
create refs/heads/txn-sym-a HEAD\n\
symref-create refs/heads/txn-sym refs/heads/main\n\
prepare\n\
commit\n";

/// A transaction whose `verify` is a *no-such-ref* assertion against a ref that
/// exists, so the whole thing must fail after its `update` was accepted.
///
/// `verify <ref> <zero-oid>` means "this ref must not exist". `refs/heads/main`
/// does, so stock dies with `cannot lock ref 'refs/heads/main': reference
/// already exists` at exit 128 and `refs/heads/txn-sym-a` must **not** have
/// moved — which is the all-or-nothing property, measured on a command that was
/// individually legal and came first.
const UPDATE_REF_TXN_VERIFY_FAILS: &[u8] = b"start\n\
update refs/heads/txn-sym-a refs/heads/feature\n\
verify refs/heads/main 0000000000000000000000000000000000000000\n\
commit\n";

/// The same transaction with the assertion inverted so it holds, which is the
/// half that proves the failure above was the `verify` and not the `update`.
const UPDATE_REF_TXN_VERIFY_HOLDS: &[u8] = b"start\n\
update refs/heads/txn-sym-a refs/heads/feature\n\
verify refs/heads/main refs/heads/main\n\
commit\n";

/// The NUL-delimited form of a committing transaction.
///
/// `-z` is not a formatting flag: it changes the *grammar*, because a value and
/// its ref name are separated by NUL rather than by SP, and `delete` takes a
/// trailing empty field where the line form takes nothing. A port that
/// implements `--stdin` by splitting on whitespace parses this as one giant
/// argument and either creates nothing or creates a ref whose name contains a
/// NUL.
const UPDATE_REF_TXN_NUL: &[u8] =
    b"start\0create refs/heads/z-a\0HEAD\0create refs/heads/z-b\0refs/heads/feature\0prepare\0commit\0";

/// The NUL-delimited transaction that must not land: a legal `delete` with its
/// empty old-value field, followed by a `create` whose new value names a ref
/// that does not exist.
///
/// `refs/heads/z-a` must survive, which is what says the delete was staged
/// rather than applied.
const UPDATE_REF_TXN_NUL_BAD: &[u8] =
    b"start\0delete refs/heads/z-a\0\0create refs/heads/z-c\0refs/heads/nope\0commit\0";

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

// ---------------------------------------------------------------------------
// hooks that refuse: the control-flow edge `Shape::Hooked` deliberately omits
// ---------------------------------------------------------------------------
//
// [`Shape::Hooked`] ships `exit 0` hooks, so "the hook ran" and "the hook was
// skipped" produce the same repository and `--no-verify` was unmeasurable.
// [`Shape::HooksFail`] ships hooks that *refuse* and hooks that *record their
// arguments into the worktree*, which is what makes a sequence the right unit:
// the refusal is one step, the bypass is the next, and the file the hook wrote
// is read back by the step after that. `probe_worktree_content` enumerates
// untracked files, so a `hook-<name>.txt` that one side wrote and the other did
// not is a state difference at the step that ran the hook — no `status` needed.
//
// Which hooks refuse, verified against stock 2.55.0 in a copy of the shape:
// `pre-commit`, `pre-push`, `pre-rebase` and `pre-auto-gc` exit 1;
// `post-commit` exits 1 and git ignores it. `prepare-commit-msg` is **not**
// skipped by `--no-verify` and `commit-msg` is, which is the pair that separates
// "the gate was bypassed" from "the message rewrite still happened".

fn hooks_fail(out: &mut Vec<Sequence>) {
    // The `--no-verify` pair, over an index staged by hand at step 1 so the
    // refusal at step 2 has nothing to roll back. Stock: step 2 exits 1 with
    // empty stdout and writes `hook-pre-commit.txt`; step 4 commits, runs
    // `prepare-commit-msg` (which `--no-verify` does not skip) and not
    // `commit-msg`, so step 5's `%B` is `hooks: bypassed\n\nprepared-by-hook\n`.
    //
    // Step 5 is `log -1 --format=%B` rather than a `status`, because the whole
    // question is *which* hooks a bypassed commit still went through, and the
    // message body is the only place that shows up on stdout.
    out.push(
        Sequence::new("commit", "hooks-pre-commit-refuses-then-no-verify", Shape::HooksFail)
            .step(&["add", "side-base.txt"])
            .step(&["commit", "-m", "hooks: refused"])
            .step(&["status", "--porcelain"])
            .step(&["commit", "-m", "hooks: bypassed", "--no-verify"])
            .step(&["log", "-1", "--format=%B"])
            .step(&["log", "--oneline", "-1"])
            .step(&["status", "--porcelain"]),
    );

    // The same refusal reached through `commit -a`, which is a different
    // question: `-a` stages the worktree *before* the hook runs, so a refused
    // `commit -a` has to leave the index where it found it. Step 1 records the
    // premise (` M side-base.txt`, unstaged) and step 3 re-asks it after the
    // refusal — stock answers identically, because it rolls the implicit staging
    // back.
    out.push(
        Sequence::new("commit", "hooks-commit-a-refused-leaves-the-index-alone", Shape::HooksFail)
            .step(&["status", "--porcelain"])
            .step(&["commit", "-am", "hooks: refused over -a"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--name-status"])
            .step(&["log", "--oneline", "-1"])
            .step(&["commit", "-am", "hooks: bypassed over -a", "--no-verify"])
            .step(&["log", "-1", "--format=%B"]),
    );

    // The merge hooks, which are a different set from the commit hooks: a
    // `merge --no-ff` runs `pre-merge-commit`, `prepare-commit-msg`,
    // `commit-msg` and `post-merge`, and never `pre-commit` — so this is the one
    // path in the corpus where `commit-msg` runs at all (every `commit` here is
    // stopped by `pre-commit` or bypassed with `--no-verify`, and `--no-verify`
    // skips `commit-msg`).
    //
    // Step 1 is a read, so the merge at step 2 is compared against a premise both
    // sides have already agreed on. Stock's step 3 prints
    // `hooks: merge side\nprepared-by-hook\n\ncommit-msg-trailer\n`: two hooks
    // rewrote the message in order, and the order is visible in the body.
    out.push(
        Sequence::new("merge", "hooks-no-ff-runs-the-merge-hooks", Shape::HooksFail)
            .step(&["log", "--oneline", "-1"])
            .step(&["merge", "--no-ff", "-m", "hooks: merge side", "hf-side"])
            .step(&["log", "-1", "--format=%B"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "--graph", "-4"]),
    );

    // `pre-push` refuses, `push --no-verify` does not. The `ls-remote` on either
    // side of each attempt is the assertion: a refusal that still moved the
    // remote ref, or a bypass that did not, is invisible in the exit code.
    //
    // The port has `--no-verify` and `--dry-run` **inverted** on `push`, so step 3
    // is expected to diverge: stock pushes `main` and the port runs the hook again
    // and refuses. Steps 4-6 are written for the day that is fixed; step 1 and 2
    // measure today — both sides run the hook, refuse, and write an identical
    // `hook-pre-push.txt` naming the remote and the ref update it was handed.
    out.push(
        Sequence::new("push", "hooks-pre-push-refuses-then-no-verify", Shape::HooksFail)
            .step(&["push", "origin", "main"])
            .step(&["ls-remote", "origin"])
            .step(&["push", "--no-verify", "origin", "main"])
            .step(&["ls-remote", "origin"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"]),
    );

    // The refusal `--no-verify` cannot bypass, because it does not run on this
    // side at all: the peer's `update` hook declines `refs/heads/veto` and
    // accepts everything else. Stock's step 2 exits 1 with the remote's own
    // `remote: update refuses …` prefix on stderr and leaves the peer untouched;
    // step 3 must show `veto` absent from `ls-remote` while `main` moved.
    //
    // Parked behind the same `--no-verify` inversion as the sequence above — the
    // port refuses at step 2 for the *local* hook's reason — and left in place
    // rather than weakened, because the peer-side refusal is a kind of failure
    // nothing else in this corpus reaches.
    out.push(
        Sequence::new("push", "hooks-peer-update-hook-vetoes-one-ref", Shape::HooksFail)
            .step(&["ls-remote", "origin"])
            .step(&["push", "--no-verify", "origin", "veto"])
            .step(&["ls-remote", "origin"])
            .step(&["push", "--no-verify", "origin", "main"])
            .step(&["ls-remote", "origin"]),
    );

    // `pre-rebase`, which has no `--no-verify` to bypass it — `git rebase` has no
    // such option — so the only thing to measure is that the refusal is total.
    //
    // The stash at step 1 exists because `rebase` checks the worktree *before* it
    // runs the hook: the shape is dirty, and without the stash step 3 would be
    // refused for being dirty and the hook would never run. Step 6 is
    // `rebase --abort` and must find nothing in progress; step 7 restores the
    // dirt, so the sequence ends where it started plus the hook's own record.
    //
    // No `strict`: the refusal at step 6 is mid-sequence, and comparing its
    // message would stop the workflow before the `stash pop` that proves the
    // refused rebase left the stash reachable.
    out.push(
        Sequence::new("rebase", "hooks-pre-rebase-refuses-and-nothing-moves", Shape::HooksFail)
            .step(&["stash", "push", "-m", "hooks-rebase"])
            .step(&["status", "--porcelain"])
            .step(&["rebase", "hf-side"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["rebase", "--abort"])
            .step(&["stash", "pop"])
            .step(&["status", "--porcelain"]),
    );

    // `pre-auto-gc`, the hook whose whole contract is that a *veto stops the
    // collect*. Reaching it needs `gc --auto` to decide a collect is due, and
    // the decision is `too_many_packs || too_many_loose_objects`; the loose-object
    // estimate samples `objects/17` and cannot be steered by a fixture this size,
    // so steps 1-3 manufacture the pack count instead: `gc` packs everything into
    // one, `stash push` writes fresh loose objects, and `repack` puts those into a
    // second pack. With `gc.autoPackLimit=1` two packs is over the limit.
    //
    // `stash push` rather than a `commit` for the loose objects, because every
    // `commit` in this shape runs `prepare-commit-msg` and would put a hook
    // marker into the worktree three steps before the step under test.
    //
    // Stock's step 5 runs the hook, is refused, and collects nothing — step 6 still
    // reports two packs. A port that ignores `pre-auto-gc` repacks anyway, which
    // is a write the user's hook forbade.
    out.push(
        Sequence::new("gc", "hooks-pre-auto-gc-vetoes-the-collect", Shape::HooksFail)
            .with_config(&[("gc.autoPackLimit", "1")])
            .step(&["gc"])
            .step(&["stash", "push", "-m", "hooks-gc"])
            .step(&["repack"])
            .step(&["count-objects", "-v"])
            .step(&["gc", "--auto"])
            .step(&["count-objects", "-v"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"]),
    );

    // `post-checkout`, whose arguments are the finding: git hands it the old HEAD,
    // the new HEAD and a `1` for a branch switch, and the hook writes all three
    // into `hook-post-checkout.txt`. That file is untracked, so the state probe
    // reads it and step 3's `status` names it — which means a port that runs the
    // hook with the *wrong* arguments is caught by content and one that does not
    // run it at all by absence.
    //
    // Step 4 switches back, so the marker is overwritten with the reverse pair:
    // the second checkout's arguments are not the first one's, and a hook invoked
    // once for two checkouts scores a difference here.
    out.push(
        Sequence::new("checkout", "hooks-post-checkout-records-its-args", Shape::HooksFail)
            .step(&["log", "--oneline", "-1"])
            .step(&["checkout", "hf-side"])
            .step(&["status", "--porcelain"])
            .step(&["checkout", "main"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"]),
    );

    // `post-rewrite`, reached through `commit --amend` — the one rewrite in this
    // shape that `pre-rebase` does not block. The hook reads its stdin, which git
    // fills with `<old-sha> <new-sha>`, and appends it to its marker file, so the
    // pair of ids the port believed it rewrote is compared rather than assumed.
    //
    // `--no-verify` to get past `pre-commit`; `--no-edit` because `GIT_EDITOR` is
    // pinned to `true` and an amend that opened one would take the
    // unchanged-message path anyway. Stock still runs `prepare-commit-msg` here —
    // `--no-verify` does not skip it — so the amended message gains
    // `prepared-by-hook` and the commit id moves.
    out.push(
        Sequence::new("commit", "hooks-amend-runs-post-rewrite", Shape::HooksFail)
            .step(&["log", "--oneline", "-1"])
            .step(&["commit", "--amend", "--no-verify", "--no-edit"])
            .step(&["log", "--oneline", "-1"])
            .step(&["log", "-1", "--format=%B"])
            .step(&["status", "--porcelain"])
            .step(&["reflog", "-3"]),
    );
}

// ---------------------------------------------------------------------------
// rerere: a resolution recorded by one merge and replayed by the next
// ---------------------------------------------------------------------------
//
// [`Shape::Rerere`] is parked mid-merge with `rerere.enabled` in the repository
// config, a populated `.git/rr-cache` and a `.git/MERGE_RR` — the only shape in
// the corpus that has any of them. Two of its three conflicts (`rr.txt`,
// `other.txt`) were resolved once at build time and have already been replayed
// into the worktree, and the third (`fresh.txt`) has never been seen, so one
// `rerere remaining` separates the replayed from the unreplayed.
//
// What no single case can ask is the *round trip*: resolve, commit, undo,
// conflict again, and see the resolution come back. That needs four invocations
// against one repository, and it is the only thing rerere is for.
//
// `probe_rr_cache` compares the cache byte for byte, so a step that records a
// preimage or a postimage is measured at the step that recorded it rather than
// at whatever later step happens to read it.

fn rerere_family(out: &mut Vec<Sequence>) {
    // The round trip. Steps 1-2 resolve the one conflict rerere has not seen
    // (`checkout --theirs` is the only resolution a step can perform, since a
    // step cannot write a file) and stage all three; step 4 commits, which is
    // where stock records the resolution for `fresh.txt`. Step 6 undoes the merge
    // commit and step 7 re-creates all three conflicts — and now every one of
    // them is in the cache, so stock resolves all three from it and steps 9-10
    // report nothing remaining and nothing to diff.
    //
    // The `Resolved '<path>' using previous resolution.` lines are stderr, so
    // what stdout carries at step 7 is the ordinary `Auto-merging` / `CONFLICT`
    // block: the replay is visible in the *worktree*, which the state probe
    // compares, and in steps 9-10 being empty.
    out.push(
        Sequence::new("rerere", "replay-a-resolution-across-a-recreated-merge", Shape::Rerere)
            .step(&["checkout", "--theirs", "--", "fresh.txt"])
            .step(&["add", "fresh.txt", "rr.txt", "other.txt"])
            .step(&["rerere", "remaining"])
            .step(&["commit", "--no-edit"])
            .step(&["log", "--oneline", "-1"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["merge", "rr-side"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "remaining"])
            .step(&["rerere", "diff"]),
    );

    // `rerere forget`, which is the inverse of the replay: it drops one path's
    // record *and puts the conflict markers back* in the worktree for it. Step 3
    // must therefore name `fresh.txt` and `rr.txt` where the pristine shape names
    // only `fresh.txt`, and step 4's `rerere diff` must print two hunks where the
    // pristine shape prints one.
    //
    // Steps 5-7 abort and re-create the merge, which asks the harder half: a
    // forget that only edited the worktree — and left the cache entry in place —
    // is indistinguishable from a real one until the same conflict comes back and
    // is silently resolved again. Stock's step 9 still names both.
    out.push(
        Sequence::new("rerere", "forget-then-recreate-does-not-replay", Shape::Rerere)
            .step(&["rerere", "forget", "rr.txt"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "remaining"])
            .step(&["rerere", "diff"])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "rr-side"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "remaining"]),
    );

    // `merge --abort` over a repository with a populated cache. The contract is
    // asymmetric and that is the whole point: `MERGE_RR` is operation state and
    // goes, `rr-cache` is a *record* and stays — an abort that clears the cache
    // throws away resolutions the user made in earlier merges, and nothing
    // reports it until the next time one of those conflicts recurs.
    //
    // Steps 3-7 are that next time: the same merge, re-run, with the two recorded
    // conflicts resolved from the cache and only `fresh.txt` left. Step 5 naming
    // one path rather than three is the assertion that the cache survived.
    out.push(
        Sequence::new("rerere", "abort-keeps-the-cache-and-drops-merge-rr", Shape::Rerere)
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["merge", "rr-side"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "remaining"])
            .step(&["rerere", "status"])
            .step(&["rerere", "diff"]),
    );

    // `rerere gc`, which expires cache entries by age. `gc.rerereResolved=0` and
    // `gc.rerereUnresolved=0` are days, so every entry the fixture wrote is past
    // its cutoff and the collect has something to do — deterministic because the
    // records are older than the run rather than because a clock was read.
    //
    // The measurement is at step 6, not step 3: a `gc` that deletes the directory
    // and one that deletes only the postimages leave different repositories and
    // the same silent exit. Re-creating the merge afterwards is what tells them
    // apart — with the postimages gone, stock replays nothing and `rerere
    // remaining` names all three paths where the pristine shape names one.
    out.push(
        Sequence::new("rerere", "gc-expires-the-cache-then-the-conflict-returns", Shape::Rerere)
            .with_config(&[("gc.rerereResolved", "0"), ("gc.rerereUnresolved", "0")])
            .step(&["merge", "--abort"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "gc"])
            .step(&["merge", "rr-side"])
            .step(&["status", "--porcelain"])
            .step(&["rerere", "remaining"])
            .step(&["merge", "--abort"])
            .step(&["rerere", "clear"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// shallow: a repository whose history is grafted, and the verbs that ungraft it
// ---------------------------------------------------------------------------
//
// [`Shape::Shallow`] is a `--depth=2 --no-single-branch` clone of a peer that
// lives inside the fixture, so `.git/shallow` has two lines and every deepening
// verb has somewhere real to fetch from without a network. The property that
// makes it worth a sequence rather than a case is that `.git/shallow` is
// *written by one command and obeyed by the next*: `fetch --deepen` rewrites it,
// and whether the walk, `fsck` and `gc` afterwards honour the new boundary is a
// second invocation's question.
//
// Deliberately not the first step of anything here: `rev-parse
// --is-shallow-repository`, which the port answers with an empty stdout and exit
// 1 where stock prints `true`. A sequence that opened with it would report that
// one fact and stop.

fn shallow(out: &mut Vec<Sequence>) {
    // `--deepen=1` moves the graft one commit further back. Step 1 records the
    // premise — four commits reachable across both branches — and step 3 must
    // show two more; step 4's count goes 2 -> 3 and step 5's first-parent walk
    // gains `shallow: deep 3`. Step 6 is the one that would catch a deepening
    // that fetched the commit without rewriting `.git/shallow`: `fsck` over a
    // grafted repository is silent only while the boundary matches the objects.
    out.push(
        Sequence::new("fetch", "shallow-deepen-then-the-walk-reaches-further", Shape::Shallow)
            .step(&["log", "--oneline", "--all"])
            .step(&["fetch", "--deepen=1"])
            .step(&["log", "--oneline", "--all"])
            .step(&["rev-list", "--count", "HEAD"])
            .step(&["log", "--oneline"])
            .step(&["fsck", "--no-progress"]),
    );

    // `--unshallow`, which is the same rewrite taken to the end: `.git/shallow`
    // is *removed*, not shortened. Step 4's count is 6 rather than 3, and step 5's
    // `fsck` is the assertion that matters — a repository that fetched the
    // history but kept the graft file still walks correctly and is no longer
    // consistent with its own boundary.
    //
    // Steps 6-8 collect afterwards, because `gc` is where a stale `shallow` does
    // damage: the parents that were unreachable a moment ago are now reachable,
    // and a `gc` that still believes the graft prunes them.
    out.push(
        Sequence::new("fetch", "shallow-unshallow-then-fsck-and-collect", Shape::Shallow)
            .step(&["log", "--oneline", "--all"])
            .step(&["fetch", "--unshallow"])
            .step(&["log", "--oneline", "--all"])
            .step(&["rev-list", "--count", "HEAD"])
            .step(&["fsck", "--no-progress"])
            .step(&["gc"])
            .step(&["log", "--oneline", "--all"])
            .step(&["count-objects", "-v"]),
    );

    // Cloning *from* a shallow repository, which produces a second shallow
    // repository with a graft of its own — `sh-copy/.git/shallow` holds one line
    // where the parent holds two, because the copy cannot be deeper than what the
    // parent could serve.
    //
    // The clone lands inside the worktree so both sides' copies stay self
    // contained. `probe_worktree_content` walks into it — the destination is an
    // ordinary directory, and only its `.git` is recorded as `<git directory>`
    // and left unread — so the checked-out files are compared and the copy's own
    // object store is not; what is compared about *that* is what the `-C sh-copy`
    // steps print. Step 4 is the finding: `fsck` in the copy is silent under
    // stock and names `missing commit …` under a port that does not read the
    // copy's own graft file.
    out.push(
        Sequence::new("clone", "shallow-clone-of-a-shallow-repository", Shape::Shallow)
            .step(&["clone", "--no-hardlinks", ".", "sh-copy"])
            .step(&["-C", "sh-copy", "log", "--oneline", "--all"])
            .step(&["-C", "sh-copy", "rev-list", "--count", "HEAD"])
            .step(&["-C", "sh-copy", "fsck", "--no-progress"])
            .step(&["-C", "sh-copy", "fetch", "--unshallow"])
            .step(&["-C", "sh-copy", "log", "--oneline", "--all"])
            .step(&["-C", "sh-copy", "rev-list", "--count", "HEAD"]),
    );

    // `repack -a -d` over a graft. `-a` means "pack everything reachable", and
    // the whole question is whether "reachable" stops at `.git/shallow`: a
    // repacker that walks past the boundary asks for the grafted parents and
    // fails, and one that stops at it produces the same object set in one pack.
    //
    // Steps 3-5 re-read afterwards, so a repack that succeeded and dropped an
    // object is separated from one that succeeded and kept them all.
    out.push(
        Sequence::new("repack", "shallow-repack-then-read-back", Shape::Shallow)
            .step(&["log", "--oneline", "--all"])
            .step(&["repack", "-a", "-d"])
            .step(&["log", "--oneline", "--all"])
            .step(&["rev-list", "--count", "HEAD"])
            .step(&["count-objects", "-v"])
            .step(&["fsck", "--no-progress"]),
    );

    // `gc --prune=now` over a graft, which is the same walk with a *delete* at the
    // end of it. Steps 3-5 prove the collect kept the history it was allowed to
    // keep, and step 6 then deepens — a deepening after a collect is where a `gc`
    // that quietly dropped the boundary shows up, because the fetch negotiates
    // against what the repository claims to have.
    out.push(
        Sequence::new("gc", "shallow-collect-then-deepen", Shape::Shallow)
            .step(&["log", "--oneline", "--all"])
            .step(&["gc", "--prune=now"])
            .step(&["log", "--oneline", "--all"])
            .step(&["rev-list", "--count", "HEAD"])
            .step(&["count-objects", "-v"])
            .step(&["fetch", "--deepen=1"])
            .step(&["log", "--oneline", "--all"])
            .step(&["fsck", "--no-progress"]),
    );
}

// ---------------------------------------------------------------------------
// promisor: objects that are absent on purpose, and the verbs that must not
// mistake that for damage
// ---------------------------------------------------------------------------
//
// [`Shape::Promisor`] is a `--filter=blob:none` clone: three of `hist.txt`'s
// four blobs are genuinely not in the object store, the packs that came from the
// server carry `.promisor` marks, and `remote.origin.promisor=true` says where
// the missing ones can be got. Everything here turns on that distinction — an
// absence a promisor remote covers is not corruption, and every verb that walks
// objects has to know which one it is looking at.
//
// A sequence is the unit because the *lazy fetch* is a side effect: the command
// that needed the blob fetched it, and whether the repository is different
// afterwards — one more pack, one fewer missing object, a reverse index written
// or not — is only visible to the command after it.
//
// `promisor::blame` is recorded elsewhere as ZVCS-NONDETERMINISTIC, so nothing
// below depends on the object set a `blame` leaves behind.

fn promisor(out: &mut Vec<Sequence>) {
    // The lazy fetch itself. Step 1 is a read that needs nothing missing, so the
    // premise is agreed before step 2 asks for a blob that is not there. Stock's
    // step 2 fetches it from the peer, prints `hist v1`, and leaves one more pack
    // — with its `.idx`, its `.promisor` mark and its `.rev` reverse index — in
    // `objects/pack`, which `probe_storage` enumerates.
    //
    // Step 3 is the same read again and must not fetch anything: the object is
    // local now, and a port that re-fetches on every read is correct on stdout and
    // wrong about the repository.
    out.push(
        Sequence::new("cat-file", "promisor-lazy-fetch-then-read-back", Shape::Promisor)
            .step(&["log", "--oneline"])
            .step(&["cat-file", "-p", "HEAD~3:hist.txt"])
            .step(&["cat-file", "-p", "HEAD~3:hist.txt"])
            .step(&["count-objects", "-v"])
            .step(&["cat-file", "-t", "HEAD~3:hist.txt"])
            .step(&["rev-list", "--missing=print", "--objects", "--all"]),
    );

    // A whole-history diff, which needs every one of the three missing blobs at
    // once. `log -p -- hist.txt` walks four commits and reconstructs three
    // diffs, so it is the densest lazy-fetch demand in the corpus, and step 4
    // then asks what is still missing — the answer must be nothing.
    //
    // Step 5's `fsck` is the second half: once every blob has been backfilled the
    // repository is complete, and a `fsck` that still reports the promisor
    // absences is reading a stale list rather than the object store.
    out.push(
        Sequence::new("log", "promisor-history-diff-drives-the-lazy-fetch", Shape::Promisor)
            .step(&["log", "--oneline"])
            .step(&["log", "--oneline", "-p", "--", "hist.txt"])
            .step(&["count-objects", "-v"])
            .step(&["rev-list", "--missing=print", "--objects", "--all"])
            .step(&["fsck", "--no-progress"])
            .step(&["diff", "HEAD~3", "HEAD", "--stat"]),
    );

    // `gc` over a partial clone, which is where the two ideas collide: a collect
    // walks every reachable object, and three of them are deliberately absent. The
    // contract is that `gc` repacks the promisor packs *as* promisor packs and
    // fetches nothing — stock's step 3 reports one pack and 14 objects, the same
    // 14 it started with.
    //
    // A port whose `gc` treats "missing" as "go and get it" backfills the whole
    // history, which is not a smaller failure than losing an object: it is the
    // partial clone silently becoming a full one, and the marker files that said
    // otherwise being dropped along the way. Step 4 is what names that — after a
    // faithful collect, three objects are still `?`-prefixed.
    out.push(
        Sequence::new("gc", "promisor-collect-must-not-backfill", Shape::Promisor)
            .step(&["log", "--oneline", "--all"])
            .step(&["gc", "--no-prune"])
            .step(&["count-objects", "-v"])
            .step(&["rev-list", "--missing=print", "--objects", "--all"])
            .step(&["cat-file", "-p", "HEAD~3:hist.txt"])
            .step(&["log", "--oneline", "--all"])
            .step(&["fsck", "--no-progress"]),
    );

    // `repack --filter-to`, which re-filters an already-filtered repository:
    // `-a -d --filter=blob:none` rewrites every pack into one and `--filter-to`
    // names where the filtered-out objects go. With nothing left to filter out,
    // stock writes no such directory at all — and, crucially, keeps the
    // `.promisor` mark on the pack it wrote.
    //
    // Step 5 is the consequence and the reason this is a sequence: `fsck` is
    // silent over a partial clone *because* the packs are marked, so a repack
    // that produced a correct pack and forgot the mark turns a healthy repository
    // into one that reports missing objects. That damage is invisible at the step
    // that caused it and obvious at the next one.
    out.push(
        Sequence::new("repack", "promisor-refilter-keeps-the-promisor-mark", Shape::Promisor)
            .step(&["log", "--oneline"])
            .step(&["repack", "-a", "-d", "--filter=blob:none", "--filter-to=.filtered"])
            .step(&["count-objects", "-v"])
            .step(&["rev-list", "--missing=print", "--objects", "--all"])
            .step(&["fsck", "--no-progress"])
            .step(&["cat-file", "-p", "HEAD~3:hist.txt"])
            .step(&["status", "--porcelain"]),
    );
}

// ---------------------------------------------------------------------------
// notes and replace over a repository that already has both
// ---------------------------------------------------------------------------
//
// [`Shape::NotesReplace`] ships three notes refs and two `refs/replace/*`
// entries, so every verb here changes how an *existing* record is read rather
// than creating the first one. Two properties need more than one invocation.
//
// A replacement is a substitution the object layer performs on every read, and
// the interesting question is what happens on the read *after* it is removed:
// `replace -d` must change the next walk's answer and nothing else, because the
// replacement and the original are both still in the object store and every id
// in the repository is unchanged either way.
//
// `notes prune` drops notes whose annotated object is gone, which needs the
// object to actually go — three invocations of setup (`reset --hard`, `reflog
// expire`, `gc --prune=now`) that no case can perform.

fn notes_replace(out: &mut Vec<Sequence>) {
    // The substitution, switched off two ways and then removed. Steps 1-2 are the
    // same walk with and without `--no-replace-objects`, and they differ in one
    // subject line: stock prints `notes: replacement for commit 1` under the
    // replacement and `notes: commit 1` without it. Steps 3-4 do the same through
    // the blob replacement, which reaches the substitution by a different door —
    // `cat-file -p HEAD:README.md` is `# replaced readme` and `# fixture`.
    //
    // Step 5 deletes the commit replacement, and steps 6-8 are the assertion:
    // the walk now prints the original's subject *without* the flag, and the blob
    // replacement — untouched — still applies. A `replace -d` that dropped both,
    // or that dropped the ref and left the substitution cached, is caught there.
    //
    // `HEAD~2` rather than the id it resolves to, so the sequence does not carry
    // a fixture hash that goes stale silently the day the shape's history moves.
    out.push(
        Sequence::new("replace", "read-with-and-without-then-delete-one", Shape::NotesReplace)
            .step(&["log", "--oneline"])
            .step(&["--no-replace-objects", "log", "--oneline"])
            .step(&["cat-file", "-p", "HEAD:README.md"])
            .step(&["--no-replace-objects", "cat-file", "-p", "HEAD:README.md"])
            .step(&["replace", "-d", "HEAD~2"])
            .step(&["log", "--oneline"])
            .step(&["replace", "-l"])
            .step(&["cat-file", "-p", "HEAD:README.md"]),
    );

    // `notes merge` between two refs that annotate the same commit with different
    // text, driven twice with two different strategies. `-s theirs` takes the
    // incoming note whole; `-s union` concatenates, so step 5's note is both
    // paragraphs with a blank line between them and step 6 shows that the *other*
    // commit `review` annotates was carried across untouched.
    //
    // Both strategies resolve without stopping, which is deliberate: the
    // conflicted path is the sequence below, and this one exists to measure the
    // notes tree a successful merge produces — step 7's `notes list` is three
    // entries where the shape started with two.
    out.push(
        Sequence::new("notes", "merge-strategies-across-three-refs", Shape::NotesReplace)
            .step(&["notes", "merge", "-s", "theirs", "other"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "list"])
            .step(&["notes", "merge", "-s", "union", "review"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "show", "HEAD~2"])
            .step(&["notes", "list"]),
    );

    // The conflicted `notes merge`, committed rather than aborted. Stock stops
    // with `CONFLICT (add/add)` — both refs *add* a note to a commit the merge
    // base has none for — and parks the half-merged tree in
    // `.git/NOTES_MERGE_WORKTREE`; `notes merge --commit` then takes whatever is
    // in that directory, conflict markers and all, and writes it as the note.
    //
    // Step 4 is what proves the commit read the worktree rather than re-running
    // the merge: the note stock stores is the *marked-up* text, with
    // `<<<<<<< refs/notes/commits` and `>>>>>>> refs/notes/other` around the two
    // paragraphs, which is not something either input ref contains.
    //
    // The port labels the same stop `CONFLICT (content)`, so this diverges at
    // step 2 today; steps 3-6 are written for the day the label is fixed and are
    // not weakened to pass in the meantime.
    out.push(
        Sequence::new("notes", "merge-conflict-committed-keeps-the-markers", Shape::NotesReplace)
            .step(&["notes", "list"])
            .step(&["notes", "merge", "other"])
            .step(&["notes", "merge", "--commit"])
            .step(&["notes", "show", "HEAD"])
            .step(&["notes", "list"])
            .step(&["log", "--oneline"]),
    );

    // `notes prune`, which needs the annotated object to be *gone* rather than
    // unreachable — a note is a tree entry whose name is the object's id, and
    // nothing about it stops being valid when the commit becomes unreachable.
    // Steps 1-4 are the three invocations that actually remove it: move `main`
    // back, expire every reflog so nothing else holds it, and collect with
    // `--prune=now`.
    //
    // Step 5 is the pointed one: after the commit is gone the note is *still
    // listed*, because `notes list` reads the notes tree and never looks the
    // object up. Step 6's `-n` names exactly what step 7 will remove, so a prune
    // that removes more than it announced is caught between the two.
    out.push(
        Sequence::new("notes", "prune-after-the-annotated-commit-is-gone", Shape::NotesReplace)
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["notes", "list"])
            .step(&["reflog", "expire", "--expire=now", "--all"])
            .step(&["gc", "--prune=now"])
            .step(&["notes", "list"])
            .step(&["notes", "prune", "-n"])
            .step(&["notes", "prune"])
            .step(&["notes", "list"])
            .step(&["log", "--oneline"]),
    );
}

// ---------------------------------------------------------------------------
// linked worktrees that are locked, open and gone at once
// ---------------------------------------------------------------------------
//
// [`Shape::WorktreeLocked`] registers three linked worktrees with
// `--relative-paths`: `wt` locked with a reason, `wt-open` unlocked, and
// `wt-gone` whose directory has been deleted. Every verb below is a *transition*
// between those states, and a case cannot make one because a case is one argv
// against a pristine copy — `unlock` could only ever be asked about "not
// locked", `remove` about "nothing objects".
//
// `probe_worktrees` reads `.git/worktrees/**`, so the administrative files are
// compared at the step that wrote them rather than inferred from a later `list`.

fn worktree_locked(out: &mut Vec<Sequence>) {
    // The lock protocol end to end: refuse, unlock, remove. Step 1 must fail with
    // the tree intact — a `remove` that deletes the directory and *then* notices
    // the lock passes an exit-code check and has already done the damage — and
    // step 4 is what confirms `wt` is gone while `wt-open` and `wt-gone` are not.
    //
    // The tail refuses on purpose: `wt-open` was never locked, so `unlock` has
    // nothing to do and says so. `strict` is on the sequence for that tail, where
    // the message is the entire contract: exit 128 with empty stdout cannot tell
    // `'wt-open' is not locked` from any other `die()`. The mid-sequence refusal
    // at step 1 is covered by the same flag, and its message is stable on both
    // sides (`cannot remove a locked working tree, lock reason: …`).
    out.push(
        Sequence::new("worktree", "locked-remove-refused-then-unlock-remove", Shape::WorktreeLocked)
            .strict()
            .step(&["worktree", "remove", "wt"])
            .step(&["worktree", "unlock", "wt"])
            .step(&["worktree", "remove", "wt"])
            .step(&["worktree", "list"])
            .step(&["worktree", "unlock", "wt-open"]),
    );

    // `prune` with a live locked tree, a live open tree and a registered-but-gone
    // tree in the same repository — the only configuration where "prune what is
    // prunable" and "prune everything" produce different repositories, and the
    // shape registers with `--relative-paths` because that is the layout where a
    // gitdir string resolved against the wrong directory makes every tree look
    // absent.
    //
    // Step 1 is `-n`, so the decision is stated *before* the write: `prune` prints
    // `Removing worktrees/wt-gone: gitdir file points to non-existent location` on
    // **stderr** under `--verbose`, which `strict` compares, so a `prune` that
    // names one tree and removes three is caught between steps 1 and 2 rather than
    // discovered later. Step 3's `list` is the assertion that `wt` and `wt-open`
    // survived.
    //
    // The tail is `branch -D` of a branch a live worktree holds, which must be
    // refused; `strict`, because that refusal names the worktree holding it and
    // an exit code alone would not.
    out.push(
        Sequence::new("worktree", "prune-the-gone-one-and-keep-the-live-ones", Shape::WorktreeLocked)
            .strict()
            .step(&["worktree", "prune", "-n", "--verbose"])
            .step(&["worktree", "prune", "--verbose"])
            .step(&["worktree", "list"])
            .step(&["branch", "-D", "wt-open"])
            .step(&["branch", "-D", "wt-held"]),
    );

    // `repair`, `lock` and `move` over the same three trees. `repair` is first
    // because it is the verb that rewrites every administrative file it thinks is
    // wrong: run against a healthy registration it must change nothing, and step 2
    // — a full `--porcelain` listing including the `locked` reason and the
    // `prunable` line — is where a repair that "fixed" a relative gitdir into an
    // absolute one, or dropped a lock file, shows up.
    //
    // Step 3 locks an already-locked tree and is refused; step 4 locks the open
    // one with a different reason, so step 5 prints two `locked` lines with
    // different text and a port that stores the flag without the reason produces
    // one. Steps 6-8 are the same refuse/unlock/act shape as `remove`, applied to
    // `move`, which has its own copy of the lock check.
    //
    // No `strict`: both refusals are mid-sequence, and comparing them would stop
    // the workflow before the move that is the point of it.
    out.push(
        Sequence::new("worktree", "repair-then-lock-and-move-a-locked-tree", Shape::WorktreeLocked)
            .step(&["worktree", "repair"])
            .step(&["worktree", "list", "--porcelain"])
            .step(&["worktree", "lock", "wt"])
            .step(&["worktree", "lock", "--reason", "second", "wt-open"])
            .step(&["worktree", "list", "--porcelain"])
            .step(&["worktree", "move", "wt", "wt-moved"])
            .step(&["worktree", "unlock", "wt"])
            .step(&["worktree", "move", "wt", "wt-moved"])
            .step(&["worktree", "list"]),
    );
}

// ---------------------------------------------------------------------------
// a tag chain: peeling that is more than one step deep
// ---------------------------------------------------------------------------
//
// [`Shape::TagChain`] has `outermost` -> `outer` -> `inner` -> commit, a
// lightweight `light-to-tag` pointing at the same tag object as `inner`, and
// tags whose targets are a blob and a tree. Every other tag in the corpus points
// straight at a commit, so an implementation that peels once scored the same as
// one that peels to the end.
//
// What a sequence adds is *deletion*: peeling a chain is one invocation, but
// what the chain peels to after a link is removed — and what `describe` calls
// the commit afterwards — is the next one's question, and the objects stay in
// the store either way so nothing about the answer is forced by what exists.

fn tag_chain(out: &mut Vec<Sequence>) {
    // Deleting the outermost link. Steps 1-2 record the premise: `describe` says
    // `inner-2-g<abbrev>` and `outermost^{}` peels three deep to the commit
    // `inner` annotates. Step 3 removes `outermost`, and step 4 must now fail to
    // resolve it at all — the tag object is still in the store, and only the ref
    // is gone, so a port that peels through a cached object answers here.
    //
    // Step 5 re-asks `describe`, which must be unchanged: `describe` names the
    // commit after the nearest *annotated* tag, and `inner` is still that tag.
    // Step 7's `fsck` then reports `dangling tag <id>` for the orphaned object,
    // which is the only place the deletion is visible in the object store.
    //
    // The refusal at step 4 is mid-sequence, so no `strict`: comparing its
    // message would stop the workflow three steps before the `fsck`.
    out.push(
        Sequence::new("tag", "delete-the-outermost-link-then-peel-and-describe", Shape::TagChain)
            .step(&["describe"])
            .step(&["rev-parse", "outermost^{}"])
            .step(&["tag", "-d", "outermost"])
            .step(&["rev-parse", "outermost^{}"])
            .step(&["describe"])
            .step(&["tag", "-l"])
            .step(&["fsck", "--no-progress"]),
    );

    // Deleting the link `describe` was *using*. `inner` is the annotated tag two
    // commits back; `light-to-tag` is a lightweight ref at the same tag object.
    // Once `refs/tags/inner` is gone, the tag object is still reachable through
    // `light-to-tag`, and stock keeps calling it `inner` — the name is inside the
    // tag object, not in the ref — while warning that
    // `tag 'light-to-tag' is externally known as 'inner'`.
    //
    // That is the finding: a port that reads the name off the ref it found the
    // object through answers `light-to-tag-2-g<abbrev>` and is self-consistent,
    // correct about the commit, and wrong about what the tag is called.
    //
    // Steps 4-6 continue past it: `outer` still peels to the same commit and is
    // still a tag object, and `fsck` is silent because nothing became unreachable.
    out.push(
        Sequence::new("describe", "delete-inner-and-describe-still-names-it", Shape::TagChain)
            .step(&["describe"])
            .step(&["tag", "-d", "inner"])
            .step(&["describe"])
            .step(&["rev-parse", "outer^{}"])
            .step(&["cat-file", "-t", "outer"])
            .step(&["fsck", "--no-progress"])
            .step(&["show-ref", "-d"]),
    );

    // The chain over the wire. There is no peer in this shape, so steps 1-2 make
    // one *inside the worktree* — `.remote.git` is where `probe_peer` looks, so
    // the bare repository the push lands in is itself compared afterwards.
    //
    // Step 3 pushes with `--tags`, which sends all six tags including the two
    // whose targets are not commits, and step 5's `ls-remote --tags` shows every
    // one of them with its `^{}` peel line — the peer resolved the chain on its
    // own side, so a port that pushed the tag objects without their targets, or
    // peeled to the wrong thing, differs there rather than in the push report.
    //
    // Step 6 deletes one tag *on the remote*, which is the finding: stock removes
    // `refs/tags/outermost` and the port answers `unable to delete 'outermost':
    // remote ref does not exist`, having looked under `refs/heads/` only.
    out.push(
        Sequence::new("push", "tag-chain-pushed-to-a-fresh-peer-then-deleted", Shape::TagChain)
            .step(&["init", "-q", "--bare", "-b", "main", ".remote.git"])
            .step(&["remote", "add", "origin", "./.remote.git"])
            .step(&["push", "--tags", "origin", "main"])
            .step(&["ls-remote", "origin"])
            .step(&["ls-remote", "--tags", "origin"])
            .step(&["push", "origin", "--delete", "outermost"])
            .step(&["ls-remote", "--tags", "origin"])
            .step(&["tag", "-l"]),
    );
}

// ---------------------------------------------------------------------------
// intent-to-add and pending renames: index entries with no content behind them
// ---------------------------------------------------------------------------
//
// [`Shape::IntentToAdd`] carries three `add -N` entries — one with content, one
// below the top level, one whose file was then deleted — beside a genuinely
// staged add that was edited afterwards, which is the `AM` rendering an ITA is
// most often confused with. [`Shape::PendingRename`] carries five staged renames
// at four similarity indices plus one expressed *only* in the worktree, through
// an intent-to-add on the destination.
//
// Both shapes describe an index that no commit could produce, and the sequences
// below ask what survives a round trip through the verbs that rewrite it. Two of
// them find that `stash` destroys what stock refuses to touch.

fn intent_to_add(out: &mut Vec<Sequence>) {
    // What a `commit` does with intent-to-add entries: nothing. Stock commits the
    // two real staged paths and leaves all three ITA entries in the index, so
    // step 2's `status` is the shape's own minus the two that landed, and step 4's
    // `ls-files -s` still lists them against the empty blob.
    //
    // Steps 5-8 then finish the job: `add -A` turns every ITA entry into a real
    // one, and the second commit's `--name-status` is where a port that committed
    // an empty blob at step 1 — or dropped the entry entirely — differs, because
    // the file's real content only reaches history here.
    out.push(
        Sequence::new("commit", "intent-to-add-committed-then-the-rest-added", Shape::IntentToAdd)
            .step(&["commit", "-m", "ita: commit over intent-to-add entries"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1", "--name-status"])
            .step(&["ls-files", "-s"])
            .step(&["add", "-A"])
            .step(&["status", "--porcelain"])
            .step(&["commit", "-m", "ita: the rest"])
            .step(&["log", "--oneline", "-1", "--name-status"]),
    );

    // The three verbs that take an entry back out of the index, applied to one
    // that has no content behind it. `add -N` on the shape's untracked file makes
    // a fourth ITA entry; `restore --staged` on one turns it back into `??`;
    // `rm --cached -f` on the nested one removes it and leaves `?? sub/`, an
    // untracked *directory* where there was a tracked path.
    //
    // `ls-files -s` at steps 3 and 8 is the assertion the `status` renderings
    // cannot make: every ITA entry is stage 0 against `e69de29…`, the empty blob,
    // so what separates them from real entries is not visible in the index without
    // knowing which blob to look for.
    out.push(
        Sequence::new("add", "intent-to-add-marked-then-unstaged-and-removed", Shape::IntentToAdd)
            .step(&["add", "-N", "untracked.txt"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "-s"])
            .step(&["restore", "--staged", "ita-new.txt"])
            .step(&["status", "--porcelain"])
            .step(&["rm", "--cached", "-f", "sub/ita-nested.txt"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "-s"]),
    );

    // `stash` over intent-to-add entries, which stock **refuses**: an ITA entry is
    // an index entry whose blob does not match the file, so the merge `stash`
    // performs cannot proceed and it stops with `error: Entry 'ita-new.txt' not
    // uptodate. Cannot merge.` / `Cannot save the current worktree state`, exit 1,
    // having changed nothing. Step 3 is that "nothing" — the shape's own `status`,
    // unchanged.
    //
    // A port that stashes anyway loses the entries: the pop at step 5 brings back
    // an index in which the ITA paths are ordinary untracked files, `ita-gone.txt`
    // — an ITA whose file was deleted — is absent from both the index and the
    // worktree, and `both.txt`'s staged blob has been replaced by its worktree
    // content, collapsing the staged/unstaged distinction the user had. Step 6's
    // `ls-files -s` is where that is plainly visible: entries that were there
    // before the stash are simply gone.
    //
    // Step 1 is a read so the refusal at step 2 is the first thing compared,
    // rather than the sequence opening on it.
    out.push(
        Sequence::new("stash", "intent-to-add-stash-is-refused-and-nothing-moves", Shape::IntentToAdd)
            .step(&["status", "--porcelain"])
            .step(&["stash", "push", "-m", "ita"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "list"])
            .step(&["stash", "pop"])
            .step(&["ls-files", "-s"])
            .step(&["status", "--porcelain"]),
    );

    // A staged rename, committed, and then followed backwards. Step 1 is the
    // index's own view (`R060 far.txt far-renamed.txt`, `R100` for the three that
    // were not edited); step 2 commits it, and step 3 must produce the same
    // pairing out of the *tree* diff rather than out of the index's rename
    // records — the two are computed by different code and only a commit puts
    // them side by side.
    //
    // Steps 4-5 are `log --follow`, which is the only reader that walks *through*
    // a rename: two commits for a path that has existed under its new name for
    // exactly one. `pkg/deep-renamed.txt` is the same question below the top
    // level, where a follow that compares basenames rather than paths differs.
    out.push(
        Sequence::new("commit", "pending-rename-committed-then-followed", Shape::PendingRename)
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["commit", "-m", "pending-rename: land the staged renames"])
            .step(&["log", "--oneline", "-1", "--name-status", "-M"])
            .step(&["log", "--follow", "--oneline", "--", "pure-renamed.txt"])
            .step(&["log", "--follow", "--oneline", "--", "pkg/deep-renamed.txt"])
            .step(&["show", "--stat", "--oneline", "-M", "HEAD"]),
    );

    // Unstaging every rename and staging them again, which asks whether rename
    // detection is a property of the index's records or of the content. It is the
    // content: after `reset` and `add -A` stock reports the same pairs — and two
    // *different* similarity indices, because the re-add takes the worktree's
    // current bytes.
    //
    // `near.txt` moves `R100` -> `R096` (it was staged at R100 and then edited
    // again in the worktree, and the re-add stages that edit), and `wt.txt` ->
    // `wt-renamed.txt` appears for the first time — it was a worktree-only rename
    // held together by an intent-to-add, and staging it makes it an index rename
    // like the others. Step 6 asks the same question with `-C` so a copy
    // candidate is in play, and must not change any of the pairs.
    out.push(
        Sequence::new("reset", "pending-rename-unstaged-then-re-added", Shape::PendingRename)
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["reset", "-q"])
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["add", "-A"])
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["diff", "--cached", "--name-status", "-M50", "-C"])
            .step(&["commit", "-m", "pending-rename: re-added"])
            .step(&["log", "--oneline", "-1", "--name-status", "-M"]),
    );

    // Renaming a rename, and un-staging one. Step 2 moves `pure-renamed.txt`
    // again, and step 3 must report the pair against the *original* name —
    // `R100 pure.txt pure-twice.txt` — because the index's source is HEAD and not
    // the previous staging.
    //
    // Step 4 restores both halves of the `far` pair from HEAD, which is the
    // asymmetric one: `far.txt` comes back to the index and the worktree does not
    // have it, while `far-renamed.txt` leaves the index and stays on disk. Stock's
    // step 6 therefore reports ` D far.txt` and `?? far-renamed.txt` at once, and
    // the pair is gone from step 5's staged diff.
    //
    // The tail is `ls-files -s` and deliberately not a worktree-column reader.
    // Both `status --porcelain` and plumbing `diff-files --name-status -M` were
    // tried here and stock did not reproduce its own post-state at that step
    // under load, so the harness excluded it and it measured nothing — the race
    // is between when `restore` writes the index and the mtimes the fixture copy
    // carries, and no argv from this corpus can settle it. The rendering those
    // steps were reaching for (stock pairing `wt.txt` with the intent-to-add
    // `wt-renamed.txt`, the port leaving them unpaired) is already measured by
    // this shape's own single-invocation `status` cases and by step 5 of
    // `pending-rename-unstaged-then-re-added`, which stages the pair and reads it
    // out of the index instead.
    out.push(
        Sequence::new("mv", "pending-rename-moved-again-then-unstaged", Shape::PendingRename)
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["mv", "pure-renamed.txt", "pure-twice.txt"])
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["restore", "--staged", "far-renamed.txt", "far.txt"])
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["ls-files", "-s"]),
    );

    // `stash` over a pending rename, which stock refuses for the same reason as
    // the intent-to-add sequence above — `wt-renamed.txt` is an ITA entry — and
    // which therefore leaves five staged renames intact where a port that stashes
    // anyway does not.
    //
    // What a stash/pop round trip costs, measured on the port: every rename is
    // dissolved into an add and a delete, the sources are back in the index while
    // the worktree no longer has them, `near-renamed.txt`'s staged blob becomes
    // the worktree's edited one rather than the `R100` content that was staged,
    // and `wt-renamed.txt` is demoted to untracked. Step 6's `ls-files -s` is the
    // whole finding in one listing: eleven entries before, fifteen after, and not
    // one of the staged renames among them.
    out.push(
        Sequence::new("stash", "pending-rename-stash-is-refused-and-the-index-survives", Shape::PendingRename)
            .step(&["diff", "--cached", "--name-status", "-M"])
            .step(&["stash", "push", "-m", "pending-rename"])
            .step(&["stash", "list"])
            .step(&["status", "--porcelain"])
            .step(&["stash", "pop"])
            .step(&["ls-files", "-s"])
            .step(&["diff", "--cached", "--name-status", "-M"]),
    );
}

// ---------------------------------------------------------------------------
// maintenance, gc and repack as a workflow rather than as one invocation
// ---------------------------------------------------------------------------
//
// Every housekeeping verb in this family is judged the same way by a case: it
// exits 0. That is the weakest possible assertion about a command whose entire
// job is to *rewrite the object store while preserving what it means*, and it
// is the assertion under which the two worst outcomes — an object silently
// dropped, and an accelerator left describing a store that has moved — both
// pass.
//
// A sequence can ask the question a case cannot: run the housekeeping, then
// **read the repository back**. `fsck` says whether the store is still valid,
// `rev-list --count --all` and `log` say whether the history is still walkable,
// `prune-packed -n` says which loose objects are now redundant, and
// `runner::probe_storage` enumerates the packs, the multi-pack-index and the
// commit-graph by name at every step, so a structure that appeared or vanished
// is attributed to the step that did it.
//
// No `count-objects -v` anywhere below, for the reason `gc::prune-packed-expire
// -collect` already gives: its `size` and `size-pack` are pack *byte* counts,
// and two correct implementations are not obliged to agree on how many bytes a
// pack of the same objects takes. `prune-packed -n` names paths instead, which
// is the same information without the number that is allowed to differ.
//
// [`Shape::Packed`] is the substrate for most of them: two packs, eight loose
// objects of which five duplicate packed ones, and one commit (`9c4078d`) that
// no ref reaches. That unreachable commit is what makes the destroy/keep
// distinction visible — it is the object every one of these verbs has an
// opportunity to lose.

fn maintenance_workflow(out: &mut Vec<Sequence>) {
    // `maintenance run --task=loose-objects` is two operations git does not
    // document as two: it prunes the loose objects that are already packed, and
    // *then* packs a batch of what is left into a new `loose-<hash>.pack`. Steps
    // 1 and 3 are the same `prune-packed -n` on either side of it, and they must
    // print **different** listings — five paths before, three after, and not one
    // path in common, because the five it removed are gone and the three it
    // packed are newly redundant.
    //
    // That inversion is the assertion. A port that implements the task as
    // "repack everything" prints an empty listing at step 3; one that implements
    // it as "prune only" prints the same five paths again; one that does nothing
    // prints the identical five. All three exit 0.
    //
    // Step 4's `incremental-repack` then writes the multi-pack-index over the
    // packs the step before it left, and steps 5-9 are the read-back: the midx
    // must verify, `fsck` must still find `9c4078d` dangling rather than gone,
    // and the object listing at step 9 must still hold every object the
    // repository started with.
    out.push(
        Sequence::new("maintenance", "loose-objects-then-incremental-repack-then-read-back", Shape::Packed)
            .step(&["prune-packed", "-n"])
            .step(&["maintenance", "run", "--task=loose-objects", "--no-detach"])
            .step(&["prune-packed", "-n"])
            .step(&["maintenance", "run", "--task=incremental-repack", "--no-detach"])
            .step(&["multi-pack-index", "verify"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"]),
    );

    // The `gc` task, which is `gc` reached through `maintenance`'s dispatcher
    // rather than through its own verb. The two are the same collector and are
    // allowed to differ in nothing, so this is worth a sequence only because of
    // what follows it: after the collect, `9c4078d` is still dangling.
    //
    // That is the whole point. `maintenance run --task=gc` uses the *default*
    // prune horizon, so the unreachable commit is kept, and a port whose
    // maintenance dispatcher forwards to a `gc --prune=now` — or to a `gc` that
    // ignores the horizon — deletes an object stock keeps and says so at step 4
    // by printing nothing where stock prints `dangling commit`.
    out.push(
        Sequence::new("maintenance", "gc-task-packs-the-loose-and-keeps-the-unreachable", Shape::Packed)
            .step(&["prune-packed", "-n"])
            .step(&["maintenance", "run", "--task=gc", "--no-detach", "--quiet"])
            .step(&["prune-packed", "-n"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["log", "--oneline", "-2"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"]),
    );

    // `prefetch`, the one maintenance task whose effect is a **ref namespace**.
    // It fetches every remote into `refs/prefetch/remotes/<remote>/*` rather than
    // into the tracking refs, so `refs/remotes/origin/*` must be untouched and
    // four refs must become six — a port that implements it as a plain `fetch`
    // moves the tracking refs and produces the same exit code.
    //
    // Step 5's `pack-refs` task then packs all six into `.git/packed-refs`, and
    // step 6 asks `for-each-ref` for the same listing it printed at step 4: a
    // port whose ref reader does not consult `packed-refs` answers with an empty
    // listing there, having just made every ref in the repository unreadable
    // while exiting 0 twice.
    out.push(
        Sequence::new("maintenance", "prefetch-writes-its-own-namespace-then-pack-refs", Shape::BehindRemote)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["maintenance", "run", "--task=prefetch", "--no-detach"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["log", "--oneline", "--all"])
            .step(&["maintenance", "run", "--task=pack-refs", "--no-detach"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"]),
    );

    // `gc` writes a commit-graph as part of collecting (`gc.writeCommitGraph`
    // defaults on), which is a side effect no case can see: the file lands under
    // `.git/objects/info/`, `gc` says nothing about it, and the next command that
    // reads it gives the same answer whether it is there or not.
    //
    // `runner::probe_storage` enumerates that directory, so step 2 is where the
    // graph has to appear. Steps 3-4 then verify it and re-derive it through
    // `maintenance run --task=commit-graph`, and step 5 verifies it again: a
    // graph that verifies before a refresh and not after describes a store the
    // refresh moved without telling it, which is the failure this cache has and
    // the reason it is worth writing at all.
    out.push(
        Sequence::new("gc", "collect-writes-a-commit-graph-then-maintenance-refreshes-it", Shape::Packed)
            .step(&["commit-graph", "verify"])
            .step(&["gc", "--quiet"])
            .step(&["commit-graph", "verify"])
            .step(&["maintenance", "run", "--task=commit-graph", "--no-detach"])
            .step(&["commit-graph", "verify"])
            .step(&["log", "--oneline", "-3"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["fsck", "--no-progress"]),
    );

    // A cruft pack is the mechanism by which `repack -a -d` is allowed to drop
    // every loose object without losing the unreachable ones: they go into a
    // second pack carrying a `.mtimes` file that records when each was last
    // seen. `repack -a -d` **without** `--cruft` would delete them outright.
    //
    // So step 3 is the finding in one line: after the repack, `fsck` must still
    // say `dangling commit 9c4078d…`. A port that ignores `--cruft` and repacks
    // only what is reachable has destroyed an object here while exiting 0, and
    // the object listing at step 7 is where the loss is enumerated rather than
    // merely named.
    //
    // Step 4's `prune --expire=now` is the second half and it must do **nothing**:
    // `prune` only ever removes *loose* objects, and after the repack there are
    // none, so an unreachable object inside a cruft pack survives a pruning that
    // names it. A port whose `prune` walks packs as well removes it there and
    // fails at step 5 rather than at step 3.
    out.push(
        Sequence::new("repack", "cruft-keeps-the-unreachable-and-prune-cannot-reach-it", Shape::Packed)
            .step(&["fsck", "--no-progress"])
            .step(&["repack", "-a", "-d", "--cruft"])
            .step(&["fsck", "--no-progress"])
            .step(&["prune", "--expire=now", "-v"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["log", "--oneline", "-1"]),
    );

    // The other direction, on the same fixture: the collect that is *supposed*
    // to drop it. `reflog expire --expire=all` removes the last thing that could
    // be holding `9c4078d`, and `gc --prune=now` then has both the permission and
    // the horizon to delete it out of the cruft pack the step before wrote.
    //
    // The pair is what makes either half meaningful. Alone, "the object is gone"
    // and "the object is kept" are each satisfiable by a port that always does
    // one of them; together they are only satisfiable by a port that reads the
    // horizon. Step 6's `fsck` must be silent where the sequence above's step 3
    // must not be.
    out.push(
        Sequence::new("repack", "cruft-then-a-pruning-collect-finally-drops-it", Shape::Packed)
            .step(&["fsck", "--no-progress"])
            .step(&["repack", "-a", "-d", "--cruft"])
            .step(&["fsck", "--no-progress"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["gc", "--prune=now", "--quiet"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"])
            .step(&["log", "--oneline", "-1"]),
    );

    // `--write-midx` folds the multi-pack-index write into the repack itself, so
    // the index is written over packs that are being replaced in the same
    // invocation — the one ordering in which a midx can end up describing a pack
    // that no longer exists. Step 3 is the check that it does not.
    //
    // `multi-pack-index expire` at step 4 then removes the packs the midx has
    // made redundant, which is a *delete* driven entirely by an accelerator: a
    // port that expires a pack still referenced by the index destroys objects,
    // and step 5's re-verify plus step 6's `fsck` are the two independent readers
    // that would notice.
    out.push(
        Sequence::new("repack", "write-midx-then-expire-then-verify-again", Shape::Packed)
            .step(&["prune-packed", "-n"])
            .step(&["repack", "-a", "-d", "--write-midx"])
            .step(&["multi-pack-index", "verify"])
            .step(&["multi-pack-index", "expire"])
            .step(&["multi-pack-index", "verify"])
            .step(&["fsck", "--no-progress"])
            .step(&["rev-list", "--count", "--all"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"]),
    );

    // What a notes ref does **not** do: keep the commit it annotates alive. A
    // note is a blob filed under a tree whose entry *name* is the annotated
    // commit's id, so nothing in `refs/notes/commits` points at that commit as
    // an object, and stock's `gc --prune=now` deletes it — verified by hand:
    // after step 6 `cat-file -t 77494d6…` is `fatal: git cat-file: could not get
    // object info`.
    //
    // That makes this the sharpest reachability question in the family, because
    // every plausible wrong answer is a *different* wrong answer:
    //
    //  * a port that treats `refs/notes/*` as a root **keeps** the commit, and
    //    step 10's object listing holds one more object than stock's;
    //  * a port that treats the note tree's entry names as object references
    //    keeps it for the same reason and diverges the same way;
    //  * a port whose collect drops the notes ref instead prints an empty
    //    `notes list` at step 7 and loses the surviving annotation as well;
    //  * a port that gets the object right and the bookkeeping wrong still has
    //    to print, at step 7, a note pointing at a commit that is not there —
    //    which stock does, and which is the line most likely to be "cleaned up".
    //
    // Steps 8-9 are the proof that the surviving half is intact: `notes show
    // HEAD` must still be `note-on-head` and the history must still walk. Step 9
    // is `fsck`, and it must be **silent**: the deleted commit leaves nothing
    // dangling, because the only thing that referred to it was a path name.
    out.push(
        Sequence::new("gc", "a-pruning-collect-drops-the-commit-its-note-still-names", Shape::Branched)
            .step(&["notes", "add", "-m", "note-on-head", "HEAD"])
            .step(&["commit", "--allow-empty", "-m", "soon-unreachable"])
            .step(&["notes", "add", "-m", "note-on-doomed", "HEAD"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["gc", "--prune=now", "--quiet"])
            .step(&["notes", "list"])
            .step(&["notes", "show", "HEAD"])
            .step(&["log", "--oneline", "-2"])
            .step(&["fsck", "--no-progress"])
            .step(&["cat-file", "--batch-all-objects", "--batch-check"]),
    );
}

// ---------------------------------------------------------------------------
// the reflog as a resource that later commands resolve against
// ---------------------------------------------------------------------------
//
// `reflog` already has one sequence in this file, and it asks whether the *file*
// survives being edited: `delete`, then `expire --expire=all`, with
// `runner::probe_reflogs` comparing `.git/logs/**` line for line.
//
// These ask the other half, which no probe reaches, because it is not a file
// comparison: **can the next command still resolve against what is left**.
// `HEAD@{2}`, `@{-3}` and `stash@{1}` are all reflog lookups performed by the
// revision parser rather than by `reflog`, and each of them has its own failure
// mode — an off-by-one after a deletion, a `@{-n}` that counts entries instead
// of counting *branch switches*, a stash stack held somewhere other than the
// reflog of `refs/stash`. All three are invisible until something asks.

fn reflog_as_a_resource(out: &mut Vec<Sequence>) {
    // The two expiry horizons, separated. `--expire=never` pins the age-based
    // pass off entirely so the only thing that can drop an entry is
    // `--expire-unreachable=now`, and the result is exact rather than
    // approximate: of `HEAD`'s seven entries, the two naming `cdf39f4` — the
    // commit step 2 reset away from — go, and the entry naming `07e86d1` stays,
    // because `refs/heads/feature` still reaches it.
    //
    // Written this way on purpose. Leaving `--expire` at its default would make
    // the sequence depend on the wall clock: the fixture's committer date is
    // pinned to 2023, `gc.reflogExpire` is 90 days, and every entry is therefore
    // *always* expired — which empties the log, hides the unreachable pass
    // completely, and would have measured nothing while looking like it measured
    // something.
    //
    // Steps 7-9 are the read-back: the surviving `HEAD@{1}` must resolve to
    // `07e86d1`, `fsck` must report `cdf39f4` as dangling now that nothing holds
    // it, and the `gc` at step 9 must then collect it.
    out.push(
        Sequence::new("reflog", "expire-unreachable-drops-only-the-unreachable-entries", Shape::Branched)
            .step(&["commit", "--allow-empty", "-m", "unreachable-soon"])
            .step(&["reset", "--hard", "HEAD~1"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["reflog", "expire", "--expire=never", "--expire-unreachable=now", "--all"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["reflog", "show", "main"])
            .step(&["rev-parse", "HEAD@{1}"])
            .step(&["fsck", "--no-progress"])
            .step(&["gc", "--prune=now", "--quiet"])
            .step(&["fsck", "--no-progress"])
            .step(&["log", "--oneline", "-2"]),
    );

    // What `HEAD@{n}` means once the log is empty, which is not what an empty
    // log looks like from `reflog show`. Stock's answers are asymmetric and both
    // halves matter: `HEAD@{0}` still resolves — to `HEAD` itself, because index
    // zero falls back to the ref — while `HEAD@{1}` is `fatal: log for HEAD is
    // empty` at exit 128.
    //
    // A port that implements `@{n}` as "index into the log" fails the first; one
    // that implements it as "resolve the ref and ignore the log" fails the
    // second, silently, by answering a commit id where stock refuses. The two
    // steps are adjacent so neither can be satisfied by guessing.
    //
    // Steps 3-4 are the same lookup *before* the expiry, so the failure at step
    // 8 is attributable to the expiry rather than to `@{n}` never having worked.
    out.push(
        Sequence::new("reflog", "expire-all-then-the-at-brace-lookup-splits-in-two", Shape::Branched)
            .step(&["reflog", "show", "HEAD"])
            .step(&["rev-parse", "HEAD@{2}"])
            .step(&["reflog", "delete", "HEAD@{1}"])
            .step(&["rev-parse", "HEAD@{2}"])
            .step(&["reflog", "show", "HEAD"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["rev-parse", "HEAD@{0}"])
            .step(&["rev-parse", "HEAD@{1}"])
            .step(&["log", "--oneline", "-2"])
            .step(&["fsck", "--no-progress", "--no-dangling"]),
    );

    // `@{-n}` counts **branch switches recorded in `HEAD`'s reflog**, not reflog
    // entries and not branches, so it is only meaningful after several checkouts
    // have happened in one repository — which is the definition of a sequence.
    //
    // Four switches, then the three lookups read back in one direction: after
    // `checkout -` has returned to `main`, `@{-1}` is `third`, `@{-2}` is `main`
    // and `@{-3}` is `feature`. `main` appearing at `-2` is the part that catches
    // a port counting distinct branches instead of switches, and `checkout -`
    // itself being a switch — so it *shifts* the window it just used — is the
    // part that catches one counting from the wrong end.
    //
    // Step 8 then uses `@{-3}` as a checkout target rather than as a name to
    // print, which is a different code path in git (`checkout` resolves it
    // before the reflog gains its own entry) and the one that would leave `HEAD`
    // detached if the resolution came back as an id instead of a branch. Step 9
    // is what says it did not.
    out.push(
        Sequence::new("checkout", "previous-branch-dance-then-at-brace-minus-three", Shape::Branched)
            .step(&["checkout", "feature"])
            .step(&["checkout", "main"])
            .step(&["checkout", "-b", "third"])
            .step(&["checkout", "-"])
            .step(&["rev-parse", "--abbrev-ref", "@{-1}"])
            .step(&["rev-parse", "--abbrev-ref", "@{-2}"])
            .step(&["rev-parse", "--abbrev-ref", "@{-3}"])
            .step(&["checkout", "@{-3}"])
            .step(&["rev-parse", "--abbrev-ref", "HEAD"])
            .step(&["reflog", "-8"])
            .step(&["status", "--porcelain"]),
    );

    // The stash stack **is** the reflog of `refs/stash`, and that identity is the
    // whole design: `stash@{1}` is a reflog lookup, `stash drop` is a reflog
    // delete that renumbers everything below it, and `stash list` is `reflog
    // show` with a different format.
    //
    // Steps 2 and 4 print the same stack through `reflog show` and through
    // `stash list` on either side of a push, so a port that keeps the stack in a
    // side file agrees with stock on `stash list` and disagrees on `reflog show
    // stash` — which is the step it would otherwise never be asked.
    //
    // Step 5 drops the *middle* entry, which is where renumbering is observable:
    // the entry that was `stash@{2}` must become `stash@{1}`, and step 8's
    // `rev-parse refs/stash@{1}` must resolve to it under the fully-qualified
    // name as well as under the short one.
    out.push(
        Sequence::new("stash", "the-stack-is-the-reflog-of-the-stash-ref", Shape::Stashed)
            .step(&["stash", "list"])
            .step(&["reflog", "show", "stash"])
            .step(&["stash", "push", "-m", "second"])
            .step(&["reflog", "show", "stash"])
            .step(&["stash", "drop", "stash@{1}"])
            .step(&["reflog", "show", "stash"])
            .step(&["rev-parse", "stash@{0}"])
            .step(&["rev-parse", "refs/stash@{1}"])
            .step(&["stash", "list"])
            .step(&["status", "--porcelain"]),
    );

    // The consequence of that identity, which is destructive and which nothing
    // in the corpus asked before: `reflog expire --expire=all --all` includes
    // `refs/stash`, so it **empties the entire stash stack**. Stock's `stash
    // list` at step 7 prints nothing, and the three stashed states are gone.
    //
    // This is the sequence most likely to find a port that is *more* careful
    // than stock. Special-casing `refs/stash` out of `--all` looks like data
    // protection and is a divergence: step 7 then lists three entries where
    // stock lists none, and steps 8-9 disagree about what the repository holds.
    //
    // Step 5's `stash show --name-only` is the premise being proven live rather
    // than assumed: the entry named there is one of the three the expiry then
    // removes.
    out.push(
        Sequence::new("stash", "expiring-every-reflog-empties-the-whole-stack", Shape::Stashed)
            .step(&["stash", "list"])
            .step(&["fsck", "--no-progress", "--no-dangling"])
            .step(&["gc", "--prune=now", "--quiet"])
            .step(&["stash", "list"])
            .step(&["stash", "show", "--name-only", "stash@{2}"])
            .step(&["reflog", "expire", "--expire=all", "--all"])
            .step(&["stash", "list"])
            .step(&["reflog", "show", "stash"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-2"]),
    );
}

// ---------------------------------------------------------------------------
// a worktree that other commands then have to account for
// ---------------------------------------------------------------------------
//
// The three worktree sequences already in this file administer worktrees:
// `add`/`move`/`remove`/`prune`, `lock`, and a commit made inside one. These ask
// what a linked worktree does to **commands that are not `worktree`** — the
// branch-in-use guard that `checkout` and `branch -D` owe it, and the second
// worktree on one branch that `--force` allows.
//
// Every one of those guards lives in a different command and reads the same
// `.git/worktrees/*/HEAD` files, which is exactly the arrangement where a port
// implements the bookkeeping once and the guard nowhere.

fn worktree_across_commands(out: &mut Vec<Sequence>) {
    // A branch checked out in a linked worktree is *claimed*, and two commands in
    // the main worktree have to notice: `checkout feature` is `fatal: 'feature'
    // is already used by worktree at …` at exit 128, and `branch -D feature` is
    // `error: cannot delete branch 'feature' used by worktree at …` at exit 1.
    // Two different exit codes for the same claim, which is why both are asked.
    //
    // The claim is not permanent, and steps 8-12 are what prove the port models
    // it as state rather than as a rule: after `worktree remove`, the identical
    // `checkout` and `branch -D` must succeed. A port that never registers the
    // claim passes steps 6-7 by doing the wrong thing and then passes 9-12 by
    // accident; a port that registers it and never releases it fails at step 9.
    //
    // Step 3 commits *inside* the linked tree, so step 5's `log feature` in the
    // main worktree is reading a branch another worktree moved — the reason the
    // claim exists at all.
    out.push(
        Sequence::new("worktree", "branch-in-use-refuses-checkout-and-delete-until-removed", Shape::Branched)
            .step(&["worktree", "add", "wt2", "feature"])
            .step(&["worktree", "list"])
            .step(&["-C", "wt2", "commit", "--allow-empty", "-m", "inside-linked"])
            .step(&["-C", "wt2", "log", "--oneline", "-2"])
            .step(&["log", "--oneline", "-1", "feature"])
            .step(&["checkout", "feature"])
            .step(&["branch", "-D", "feature"])
            .step(&["worktree", "remove", "wt2"])
            .step(&["worktree", "list"])
            .step(&["checkout", "feature"])
            .step(&["log", "--oneline", "-1"])
            .step(&["checkout", "main"])
            .step(&["branch", "-D", "feature"])
            .step(&["worktree", "prune", "-v"]),
    );

    // `--force` is the escape hatch from that guard, and what it produces is the
    // state the guard exists to prevent: two working trees on one branch. Step 2
    // is the refusal and step 3 is the same command overriding it, so the pair
    // measures the flag rather than the guard.
    //
    // Steps 5-7 are why the state is worth reaching. A commit made in `wt3` moves
    // `refs/heads/wtb`, and `wt2` — whose index and worktree still describe the
    // old tip — resolves `HEAD` through the same branch and therefore *reports
    // the new commit it does not contain*. Both `log` steps must print
    // `in-wt3`. A port that gives each worktree its own resolution of a shared
    // branch prints two different answers.
    out.push(
        Sequence::new("worktree", "force-adds-a-second-tree-on-one-branch", Shape::Branched)
            .step(&["worktree", "add", "-b", "wtb", "wt2"])
            .step(&["worktree", "add", "wt3", "wtb"])
            .step(&["worktree", "add", "--force", "wt3", "wtb"])
            .step(&["worktree", "list"])
            .step(&["-C", "wt3", "commit", "--allow-empty", "-m", "in-wt3"])
            .step(&["-C", "wt2", "log", "--oneline", "-1"])
            .step(&["-C", "wt3", "log", "--oneline", "-1"])
            .step(&["worktree", "remove", "--force", "wt3"])
            .step(&["worktree", "remove", "--force", "wt2"])
            .step(&["worktree", "prune", "-v"])
            .step(&["branch", "--format=%(refname:short)"])
            .step(&["log", "--oneline", "-1", "wtb"]),
    );
}

// ---------------------------------------------------------------------------
// a config write, and the step it changes
// ---------------------------------------------------------------------------
//
// `Case::with_config` can deliver a key from any scope, which covers the whole
// question of *reading* configuration. It cannot cover the other half: a key
// **written by one invocation and consulted by the next**, where the write is
// itself the thing being measured.
//
// Those are two different failures. A port that parses `core.bare` and ignores
// it fails a case. A port whose `config set core.bare true` writes the key to a
// file nothing later reads — or writes it under a name that differs in case, or
// into `config.worktree` when it should go to `config` — passes every case in
// the corpus and fails here, at the step that reads it back through behaviour
// rather than through `config get`.
//
// Each sequence below therefore has the same three-part shape: prove the
// behaviour before, write the key, prove the behaviour changed — and, where the
// key can be withdrawn, prove it changes back. The last part is what separates
// "honours the setting" from "always does the second thing".

fn config_drives_the_next_step(out: &mut Vec<Sequence>) {
    // `--worktree` is not a scope that always exists: it is gated on
    // `extensions.worktreeConfig`, and with the extension off git does not
    // refuse — it **silently writes to `--local` instead**. Step 2 is that
    // fallback and step 7 is where it becomes visible, as a `local demo.key`
    // line in `config list --show-scope` that the sequence never asked for.
    //
    // Step 3 turns the extension on and step 4 repeats the identical write,
    // which now lands in `.git/config.worktree`. So after step 4 the same key
    // exists twice in two scopes with the same value, and step 6's plain `config
    // get` has to pick one: stock answers with the worktree scope, because it
    // outranks local. That precedence is the assertion a port most often gets
    // backwards, and it is only reachable after two writes that a case cannot
    // perform.
    //
    // Step 8 withdraws the worktree copy and step 9 must then fail with exit 1 —
    // not fall back to the local copy that is still there, which is what a port
    // that implements `--worktree` as "the merged view" does.
    out.push(
        Sequence::new("config", "worktree-scope-is-gated-on-the-extension", Shape::Branched)
            .step(&["config", "get", "--worktree", "demo.key"])
            .step(&["config", "set", "--worktree", "demo.key", "wt-scope"])
            .step(&["config", "set", "extensions.worktreeConfig", "true"])
            .step(&["config", "set", "--worktree", "demo.key", "wt-scope"])
            .step(&["config", "get", "--worktree", "demo.key"])
            .step(&["config", "get", "demo.key"])
            .step(&["config", "list", "--show-scope"])
            .step(&["config", "unset", "--worktree", "demo.key"])
            .step(&["config", "get", "--worktree", "demo.key"])
            .step(&["config", "list", "--show-scope"]),
    );

    // `core.bare` rewrites what the repository *is* for every command after it,
    // and it does so without moving a single file: the worktree is still on disk,
    // still full of tracked content, and now unreachable.
    //
    // The three steps after the write are chosen because they take three
    // different paths through git's setup. `status` needs a worktree and dies
    // with `fatal: this operation must be run in a work tree` at 128;
    // `rev-parse --show-toplevel` needs one for a different reason and dies with
    // the same words; `log` needs none and must keep working, printing the same
    // commit it printed before. A port that treats the flag as "refuse
    // everything" fails on `log`; one that ignores it fails on the other two;
    // one that honours it and forgets to *unset* it fails at step 9.
    //
    // Step 9 is the withdrawal, and it is the step that says the repository was
    // never actually damaged — a port that responds to `core.bare` by discarding
    // the index or the worktree cannot come back from it.
    out.push(
        Sequence::new("config", "core-bare-turns-two-verbs-into-refusals-and-back", Shape::Branched)
            .step(&["rev-parse", "--is-bare-repository"])
            .step(&["config", "set", "core.bare", "true"])
            .step(&["rev-parse", "--is-bare-repository"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["rev-parse", "--show-toplevel"])
            .step(&["config", "set", "core.bare", "false"])
            .step(&["rev-parse", "--is-bare-repository"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"]),
    );

    // Two worktrees, one key, two values — the property `extensions.worktreeConfig`
    // exists for and the only one that proves each tree reads *its own*
    // `.git/worktrees/<name>/config.worktree` rather than a single shared file.
    //
    // Steps 5-6 are the pair: the same `config get --worktree demo.where` run
    // from the two trees must answer `main-tree` and `linked-tree`. A port that
    // stores the worktree scope in one place answers the same thing twice, and
    // which of the two it answers only says which write landed last.
    //
    // Step 8's unqualified `config get` from the main tree must be `main-tree`,
    // not `linked-tree`: the merged view is per-worktree too. Step 10 is after
    // the linked tree is gone and must still be `main-tree` — a `worktree remove`
    // that takes the main tree's worktree config with it is a data loss that
    // nothing else in this corpus would see.
    out.push(
        Sequence::new("config", "per-worktree-values-diverge-between-two-trees", Shape::Branched)
            .step(&["config", "set", "extensions.worktreeConfig", "true"])
            .step(&["worktree", "add", "-b", "wtb", "wt2"])
            .step(&["config", "set", "--worktree", "demo.where", "main-tree"])
            .step(&["-C", "wt2", "config", "set", "--worktree", "demo.where", "linked-tree"])
            .step(&["config", "get", "--worktree", "demo.where"])
            .step(&["-C", "wt2", "config", "get", "--worktree", "demo.where"])
            .step(&["-C", "wt2", "config", "list", "--show-scope"])
            .step(&["config", "get", "demo.where"])
            .step(&["worktree", "remove", "--force", "wt2"])
            .step(&["config", "get", "--worktree", "demo.where"])
            .step(&["config", "list", "--show-scope"]),
    );

    // `core.hooksPath` moves the whole hook directory, and on [`Shape::HooksFail`]
    // — where `pre-commit` exits 1 — moving it is the difference between a commit
    // that cannot be made and one that can.
    //
    // Step 2 is the refusal with the hooks in place, step 4 is the identical
    // commit with `core.hooksPath` pointing at a directory that does not exist,
    // and step 8 is the refusal again after the key is withdrawn. A port that
    // never reads the key refuses all three times; one that reads it once and
    // caches the resolved path refuses at 2, allows 4, and allows 8.
    //
    // Step 6's `status` is the corroboration that the hook really ran at step 2
    // and really did not at step 4: `hook-pre-commit.txt` is written by the hook
    // itself, so it is present exactly once — a port that skips the hook at step
    // 2 for some other reason produces the right exit code and no file.
    //
    // Step 5 is `log --oneline -2` rather than `-1` because the commit at step 4
    // was made with `prepare-commit-msg` and `commit-msg` redirected away too, so
    // its message is the one the argv gave it rather than the one the hook would
    // have rewritten.
    out.push(
        Sequence::new("commit", "hooks-path-redirects-the-refusal-away-and-back", Shape::HooksFail)
            .step(&["log", "--oneline", "-1"])
            .step(&["commit", "--allow-empty", "-m", "hooked"])
            .step(&["config", "set", "core.hooksPath", ".git/no-such-hooks"])
            .step(&["commit", "--allow-empty", "-m", "unhooked"])
            .step(&["log", "--oneline", "-2"])
            .step(&["status", "--porcelain"])
            .step(&["config", "unset", "core.hooksPath"])
            .step(&["commit", "--allow-empty", "-m", "hooked-again"])
            .step(&["log", "--oneline", "-1"])
            .step(&["log", "-1", "--format=%B"]),
    );

    // `core.sparseCheckout` on a repository that has never been sparse. Setting
    // it alone changes nothing — there is no `.git/info/sparse-checkout` for it
    // to consult — and step 3's `read-tree -m -u HEAD` must therefore leave every
    // entry present, which is the step a port fails if it treats the flag as
    // "skip everything not listed" over an absent list.
    //
    // `sparse-checkout set src` at step 6 then writes the list *and* the flag
    // together, and step 7's `ls-files -t` is where the `S` bit appears on
    // `README.md`. Step 9's `disable` is the withdrawal, and step 11 is the part
    // worth the sequence: stock leaves `core.sparseCheckout` in the config with
    // the value `false` rather than removing the key, so `config get` is `false`
    // at exit 0. A port that deletes the key answers exit 1 with no output, and
    // no case can tell the two apart because no case can run `disable` first.
    out.push(
        Sequence::new("sparse-checkout", "config-first-then-set-and-disable", Shape::Branched)
            .step(&["ls-files", "-t"])
            .step(&["config", "set", "core.sparseCheckout", "true"])
            .step(&["read-tree", "-m", "-u", "HEAD"])
            .step(&["ls-files", "-t"])
            .step(&["status", "--porcelain"])
            .step(&["sparse-checkout", "set", "src"])
            .step(&["ls-files", "-t"])
            .step(&["status", "--porcelain"])
            .step(&["sparse-checkout", "disable"])
            .step(&["ls-files", "-t"])
            .step(&["config", "get", "core.sparseCheckout"])
            .step(&["status", "--porcelain"]),
    );

    // `core.logAllRefUpdates` is only consulted when a reflog has to be
    // *created*, never when one is appended to, and that asymmetry is the whole
    // sequence. Step 2 creates `off-log` with the setting off and step 3's
    // `reflog show` must be empty; step 5 creates `on-log` with it back on and
    // step 6 must print the `branch: Created from main` line.
    //
    // Step 7 repeats the first lookup after the setting is back on, and it must
    // *still* be empty: turning the setting on does not retroactively create a
    // log. Step 8 then updates `off-log`, which does create one — with a single
    // entry, not with the entry it would have had at creation. A port that
    // decides "log or not" per invocation rather than per file gets step 7 or
    // step 9 wrong, and `runner::probe_reflogs` compares `.git/logs/**` at every
    // step, so the finding is attributed to the step that created the file.
    out.push(
        Sequence::new("branch", "log-all-ref-updates-off-creates-no-reflog", Shape::Branched)
            .step(&["config", "set", "core.logAllRefUpdates", "false"])
            .step(&["branch", "off-log"])
            .step(&["reflog", "show", "off-log"])
            .step(&["config", "set", "core.logAllRefUpdates", "true"])
            .step(&["branch", "on-log"])
            .step(&["reflog", "show", "on-log"])
            .step(&["reflog", "show", "off-log"])
            .step(&["update-ref", "refs/heads/off-log", "refs/heads/feature"])
            .step(&["reflog", "show", "off-log"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"]),
    );
}

// ---------------------------------------------------------------------------
// transactions, and the reads that must see all of one or none of it
// ---------------------------------------------------------------------------
//
// One `update-ref --stdin` sequence is already here and it asks the base
// question: does a failing transaction leave a half-updated repository. These
// ask the three parts of the protocol that question does not reach — the
// explicit `abort` after `prepare`, `symref-create` inside a transaction, and
// `verify` as a precondition that fails *after* a legal update was accepted —
// plus the `-z` grammar, which is a different parser rather than a different
// format.

fn update_ref_transactions(out: &mut Vec<Sequence>) {
    // `prepare` takes every lock; `abort` releases them and applies nothing. So
    // steps 1-2 are a transaction that reached the last moment before landing and
    // then did not, and `for-each-ref` must show exactly the two branches the
    // fixture started with.
    //
    // Steps 3-5 are the same transaction committed, which is what makes step 2
    // meaningful: the payload is provably capable of creating both refs, so an
    // empty listing at step 2 is `abort` working rather than the payload being
    // rejected. Step 5's `symbolic-ref` is the half `for-each-ref` cannot see on
    // its own — a `symref-create` written as an ordinary ref has the same
    // `%(objectname)` and only fails here.
    //
    // Steps 6-7 are the precondition. `verify <ref> <zero-oid>` asserts that a
    // ref does **not** exist; `refs/heads/main` does, so stock dies at exit 128
    // with `cannot lock ref 'refs/heads/main': reference already exists` — and
    // the `update refs/heads/txn-sym-a` that came *before* it in the same payload
    // must not have landed. Step 8 runs the same update with the assertion
    // inverted so it holds, and step 9 is where `txn-sym-a` finally moves, which
    // is what says step 7 measured the transaction rather than a broken update.
    out.push(
        Sequence::new("update-ref", "aborted-transaction-then-symref-then-a-failing-verify", Shape::Branched)
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_TXN_ABORTED)
            .step(&["for-each-ref", "--format=%(refname)", "refs/heads"])
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_TXN_SYMREF)
            .step(&["for-each-ref", "--format=%(refname) %(objectname) %(symref)", "refs/heads"])
            .step(&["symbolic-ref", "refs/heads/txn-sym"])
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_TXN_VERIFY_FAILS)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"])
            .step_stdin(&["update-ref", "--stdin"], UPDATE_REF_TXN_VERIFY_HOLDS)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"])
            .step(&["reflog", "show", "refs/heads/txn-sym-a"]),
    );

    // The `-z` grammar. It is not a formatting flag: the separator between a
    // command, its ref and its value becomes NUL, and `delete` grows a trailing
    // empty field where the line form has nothing after the ref name. A port that
    // implements `--stdin` by splitting the payload on whitespace reads the
    // NUL-delimited form as a single argument and creates either nothing or a ref
    // whose name contains a NUL — and `for-each-ref` at step 2 is where that
    // becomes a listing rather than an exit code.
    //
    // Step 3's payload opens with a legal `delete refs/heads/z-a` and then names
    // a non-existent ref as a new value, so the whole transaction must fail with
    // `z-a` intact. It is the same all-or-nothing property the line-form sequence
    // asks, deliberately repeated here, because the `-z` parser is a different
    // parser and a port can stage correctly in one and apply-as-it-reads in the
    // other.
    out.push(
        Sequence::new("update-ref", "nul-delimited-transaction-then-one-that-must-not-land", Shape::Branched)
            .step_stdin(&["update-ref", "--stdin", "-z"], UPDATE_REF_TXN_NUL)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"])
            .step_stdin(&["update-ref", "--stdin", "-z"], UPDATE_REF_TXN_NUL_BAD)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"])
            .step(&["rev-parse", "--verify", "refs/heads/z-a"])
            .step(&["rev-parse", "--verify", "refs/heads/z-c"])
            .step(&["log", "--oneline", "-1", "refs/heads/z-b"])
            .step(&["status", "--porcelain"]),
    );

    // `pack-refs` moves every ref out of `.git/refs/**` and into a single
    // `.git/packed-refs` file, and from that moment every ref *read* has to
    // consult a file it did not before and every ref *delete* has to rewrite it.
    //
    // Step 3 is the read: the identical `for-each-ref` from step 1, which must
    // print the identical four lines from a completely different storage layout.
    // A port whose reader only walks the directory tree answers with nothing
    // there, having just made every ref in the repository invisible at exit 0.
    //
    // Steps 4 and 8 are the deletes, and they are the ones that lose data in the
    // other direction. There is no loose file to unlink, so a port that
    // implements deletion as `unlink` reports success and leaves the ref in
    // `packed-refs` — where step 5's listing, step 7's `branch` and step 9's
    // final listing all still find it. `tag -d` at step 8 is asked as well as
    // `update-ref -d` at step 4 because they are different call sites onto the
    // same file, and `v0.2.0` is an annotated tag, so its `packed-refs` entry has
    // a `^`-peeled line beneath it that the rewrite has to carry.
    out.push(
        Sequence::new("pack-refs", "packed-then-deleted-must-rewrite-the-file", Shape::Branched)
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["pack-refs", "--all"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["update-ref", "-d", "refs/heads/feature"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["rev-parse", "--verify", "refs/tags/v0.1.0"])
            .step(&["branch", "--format=%(refname:short)"])
            .step(&["tag", "-d", "v0.1.0"])
            .step(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["fsck", "--no-progress", "--no-dangling"])
            .step(&["log", "--oneline", "-1"]),
    );
}

// ---------------------------------------------------------------------------
// three more records one command leaves for the next
// ---------------------------------------------------------------------------
//
// A submodule's URL lives in two files at once and a third command copies
// between them; a bundle is a repository serialized to a file that a later
// `clone` has to read back; `restore --source=@{-1}` resolves a branch out of
// the reflog and pulls one path out of it. None of the three is one invocation.

fn side_records(out: &mut Vec<Sequence>) {
    // A submodule URL is stored twice on purpose: `.gitmodules` is the *tracked*
    // value, versioned with the superproject, and `submodule.<name>.url` in
    // `.git/config` is the *local* value the fetch actually uses. `set-url`
    // writes the first; `sync` copies it to the second, absolutizing a relative
    // URL against the superproject as it goes.
    //
    // Step 3 reads `.gitmodules` back and must be the literal `./upstream-moved`
    // — unresolved, because it is a tracked file and resolving it would make the
    // superproject unclonable. Step 4 reads the local copy, which must be the
    // *absolute* form. A port that writes one value to both files fails one of
    // the two, and no case can ask, because a case sees only whichever state the
    // fixture was built in.
    //
    // Step 7's `diff --name-only` is the consequence a port is likely to miss:
    // `.gitmodules` is tracked, so rewriting it makes the superproject dirty.
    // Step 9's `deinit` then unregisters the submodule while leaving the gitlink
    // in the index, which step 12 is what proves — `160000 7c9f5d7…` still there
    // after the working copy is gone.
    out.push(
        Sequence::new("submodule", "set-url-then-sync-then-deinit-keeps-the-gitlink", Shape::Submodule)
            .step(&["submodule", "status"])
            .step(&["submodule", "set-url", "sub", "./upstream-moved"])
            .step(&["config", "get", "-f", ".gitmodules", "submodule.sub.url"])
            .step(&["config", "get", "submodule.sub.url"])
            .step(&["submodule", "sync"])
            .step(&["config", "get", "submodule.sub.url"])
            .step(&["diff", "--name-only"])
            .step(&["submodule", "status"])
            .step(&["submodule", "deinit", "-f", "sub"])
            .step(&["submodule", "status"])
            .step(&["status", "--porcelain"])
            .step(&["ls-files", "-s", "sub"]),
    );

    // A bundle is a repository in one file, and the only way to find out whether
    // a port wrote a real one is to make something else read it. `bundle verify`
    // is the cheap reader and `clone` is the expensive one, and they fail
    // differently: a bundle with the right header and a malformed pack verifies
    // and does not clone.
    //
    // Steps 5-7 are the clone and its read-back. `--no-local` forces the transport
    // path rather than a directory copy, and the clone's ref listing at step 7 is
    // where the bundle's `HEAD` line turns into `refs/remotes/origin/HEAD` — the
    // detail a port that writes refs but no `HEAD` loses, while still verifying
    // and still cloning.
    //
    // Step 8's incremental bundle is the other half of the format: a bundle with
    // *prerequisites*, whose `verify` prints `The bundle requires this ref` and
    // whose exit code depends on the repository doing the verifying. Step 9 runs
    // it where the prerequisite is present and step 10 runs it inside the clone,
    // where it is also present — so both must be `okay`, and a port that records
    // no prerequisites at all prints `The bundle records a complete history`
    // instead, at the same exit code.
    out.push(
        Sequence::new("bundle", "create-verify-clone-then-an-incremental-bundle", Shape::Branched)
            .step(&["bundle", "create", "./all.bundle", "--all"])
            .step(&["bundle", "verify", "./all.bundle"])
            .step(&["bundle", "list-heads", "./all.bundle"])
            .step(&["clone", "-q", "--no-local", "./all.bundle", "./from-bundle"])
            .step(&["-C", "from-bundle", "log", "--oneline", "--all"])
            .step(&["-C", "from-bundle", "for-each-ref", "--format=%(refname) %(objectname)"])
            .step(&["bundle", "create", "./inc.bundle", "main~1..main"])
            .step(&["bundle", "verify", "./inc.bundle"])
            .step(&["-C", "from-bundle", "bundle", "verify", "../inc.bundle"])
            .step(&["status", "--porcelain"]),
    );

    // `restore --source=@{-1}` is two features meeting: a reflog lookup for the
    // previously-checked-out branch, and a path-restricted checkout out of the
    // tree it names. Neither is reachable alone — `@{-1}` needs a switch to have
    // happened, and `--source` needs something to restore *from* that is not
    // `HEAD`.
    //
    // Steps 1-2 make `feature` the previous branch while standing on `main`.
    // Step 4 then restores `feature.txt`, which exists only on `feature`, into
    // both the index and the worktree — so step 5 must be `A  feature.txt` and
    // step 6 must be `A\tfeature.txt`. A port that resolves `@{-1}` to `HEAD`
    // fails with `pathspec 'feature.txt' did not match`; one that resolves it to
    // the *commit* rather than the branch gets the same answer here and would
    // diverge only on the name, which is why step 3 pins the clean start.
    //
    // Step 7's `switch -` is the closing symmetry: with `feature.txt` staged from
    // `feature`'s tree, switching to `feature` is not a conflict — the staged
    // content is already what that branch has — so step 8 must be clean and step
    // 10 must be back on `feature`.
    out.push(
        Sequence::new("restore", "source-at-brace-minus-one-then-switch-back", Shape::Branched)
            .step(&["switch", "feature"])
            .step(&["switch", "main"])
            .step(&["status", "--porcelain"])
            .step(&["restore", "--source=@{-1}", "--staged", "--worktree", "--", "feature.txt"])
            .step(&["status", "--porcelain"])
            .step(&["diff", "--cached", "--name-status"])
            .step(&["switch", "-"])
            .step(&["status", "--porcelain"])
            .step(&["log", "--oneline", "-1"])
            .step(&["rev-parse", "--abbrev-ref", "HEAD"]),
    );
}
