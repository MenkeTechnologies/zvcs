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
    s
}

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
// time. `Whitespace` has six commits that all rewrite `ws/indent.c`, so picking
// a run of them onto an older one conflicts at the *first* pick with two more
// still queued.

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
    // consume the entry: it drops `stash@{0}`, and step 9 shows the two entries
    // the fixture started with. A pop that dropped on conflict leaves a
    // different list.
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
}
