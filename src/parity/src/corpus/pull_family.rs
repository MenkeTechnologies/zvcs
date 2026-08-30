//! `pull` as a *composite*: which integration mode it picks, what it refuses to
//! do without being told, and what the fetch half hands the merge half.
//!
//! `pull` is `fetch` followed by `merge` or `rebase`, and it is the verb a
//! working day is made of. That composition is the thing under test here. Both
//! halves can be individually correct and the seam between them still wrong:
//! the mode selection lives in `builtin/pull.c` and nowhere else, the
//! divergence check is computed from `orig_head` against the *set* of merge
//! heads the fetch produced, and the order in which the two halves run decides
//! whether a bad `--cleanup` value is caught before or after `FETCH_HEAD` is
//! written. None of that is reachable by testing `fetch` and `merge` apart, and
//! five of the divergences recorded below are exactly there: the two-merge-head
//! refusal, `--cleanup=bogus`'s ordering, `--deepen=1` changing what the merge
//! half is handed, `--set-upstream`'s post-fetch write, and the annotated tag
//! the fetch half resolves correctly and the merge half does not.
//!
//! # How this divides territory with the modules that already touch `pull`
//!
//! Read before writing a line of this file, in this order, and what each owns:
//!
//! * **`transport_local.rs`** — `pull` as *transport*: the repository used as
//!   its own remote. Its group is one argv shape, `pull [<flag>] . refs/heads/feature`
//!   on `Shape::Branched`, plus one each on `Merged`, `Dirty`, `Detached`,
//!   `Submodule` and `Conflicted`, a bare `pull` on `Linear`, an unknown remote,
//!   an unknown ref and `-h`. The flags it spells — `--no-rebase`, `--ff-only`,
//!   `--rebase`, `--autostash`, `--quiet`, `--no-ff`, `--squash`,
//!   `--allow-unrelated-histories` — are each measured **once**, against a
//!   branch that fast-forwards. Nothing there is diverged, so nothing there can
//!   reach the mode selection: on a fast-forward every mode agrees. This file
//!   uses `.` only where divergence or a config key is the thing being measured,
//!   and on shapes that file never touches.
//! * **`merge_dirty.rs`** — `pull . refs/heads/ff-{cold,hot,squat}` and
//!   `pull . refs/heads/div-{cold,hot}` on
//!   `MergeableDirty`/`MergeableStaged`, asking one question: does the pull
//!   refuse per *path* when the worktree is holding a file the merge wants. It
//!   spells `--no-rebase` and `--ff-only` only, and never `--autostash`, which
//!   is the option that exists for precisely that situation. Its bare
//!   `pull . refs/heads/div-cold` rows do reach the 2.34 refusal, which is why
//!   this file adds no refusal row on that shape; they are not strict, so what
//!   is unmeasured there is the message, and the message is measured here on
//!   `CrissCross` and `Cherry` instead. The autostash group below is on
//!   `MergeableDirty` for the reason above and shares no argv with it.
//! * **`fetch_clone.rs`** — `fetch`/`clone`/`bundle`/`ls-remote` against
//!   `BehindRemote`'s bare peer, including `--depth`/`--deepen`/`--unshallow`
//!   and the four `--prune` spellings *for `fetch`*. `pull` re-parses those
//!   options itself and forwards them to a `fetch` child; the forwarding is the
//!   seam, and `--deepen` below is where it breaks.
//! * **`refspec_algebra.rs`** — the `[+]<src>[:<dst>]` matcher, asked through
//!   `fetch` and `push`. Nothing here re-asks the grammar; the refspec group
//!   below is only about which head the *merge* then gets, which is the half
//!   the matcher tests cannot see.
//! * **`branch_remote.rs`** — `push`, `branch`, `remote`, and the upstream
//!   configuration read *by* those verbs. `--set-upstream` on `pull` writes the
//!   same two keys from a different code path and is unmeasured there.
//! * **`merge_family.rs` / `merge_strategies.rs`** — `git merge` itself:
//!   strategies, `-X`, `--squash`, `--no-commit`, unrelated histories. Every one
//!   of those is reached here only *through* `pull`, where the option has to
//!   survive being re-parsed and forwarded, and where `pull.twohead` and
//!   `pull.octopus` — two keys `merge` does not read — choose the strategy.
//! * **`rebase_engine.rs`** — `git rebase` itself, including `--rebase-merges`
//!   and the autostash. `pull --rebase=merges` is the only way to reach the
//!   merge-preserving replay *from a pull*, and it is where the port abandons a
//!   half-finished rebase (see the octopus group).
//! * **`sequences.rs`** — two multi-step pulls: `checkout div` then a diverged
//!   dirty pull on `BehindRemote`, and the unrelated-histories refuse-then-allow
//!   pair on `Unrelated`. Both are single-head merges. No sequence covers mode
//!   selection, `pull.rebase`, `pull.ff`, or a pull with two merge heads.
//! * **`wire_protocol.rs`**, **`submodule_deep.rs`** — the protocol dimension
//!   and `--recurse-submodules=bogus`. `--recurse-submodules=on-demand` on a
//!   repository with *no* submodules is here because it is a pull-side parse,
//!   not a submodule walk.
//!
//! # The divergence check, and why it needs a shape nobody else pulls on
//!
//! Git 2.34 turned the "you have divergent branches" advice into a refusal.
//! `builtin/pull.c` computes `can_ff` from `orig_head` against the merge heads,
//! calls anything that is neither a fast-forward nor already-up-to-date
//! *divergent*, and — when no mode was selected by argv or by config — prints a
//! twelve-line hint and dies. It is the message users see most often from this
//! verb, and it is **not suppressible**: `--no-advice`, `GIT_ADVICE=0` and
//! `advice.diverging=false` leave it intact, because pull.c writes it with plain
//! `advise()` rather than `advise_if_enabled()`. Verified on stock 2.55.0, each
//! spelling separately, twelve `hint:` lines and exit 128 with an empty stdout
//! every time.
//!
//! **The port suppresses it for one of the three.** Bare, under `--no-advice`
//! and under `GIT_ADVICE=0` the port reproduces all twelve lines byte for byte;
//! under `advice.diverging=false` it prints none of them and goes straight to
//! `fatal:`. So the port routes this message through the advice gate git
//! deliberately does not use, and the user who once ran
//! `git config advice.diverging false` stops being told how to answer the
//! refusal that is still stopping them. That is what the three-spelling group
//! below exists to say, and it could not be said by measuring one spelling.
//!
//! Reaching it needs a *checked-out branch that has diverged*, and no shape any
//! existing pull case uses has one. `BehindRemote`'s `main` is strictly behind
//! its upstream and its `div` — the diverged branch — is not checked out, and a
//! case is one argv against a pristine copy, so it cannot check it out first.
//! The shapes that do have one, none of which is pulled on anywhere else:
//!
//! * [`Shape::CrissCross`] — `cc-left` checked out, `cc-right` diverged from it
//!   with **two** merge bases and a conflicting path. The mode selection matrix
//!   lives here: merge conflicts, rebase succeeds, and the two answers are far
//!   enough apart that a mode chosen wrongly cannot look right.
//! * [`Shape::Cherry`] — `topic` checked out, `main` diverged, and the two share
//!   a *patch* (`cherry: shared patch`) under different ids. Merge carries it
//!   through; rebase drops it and says `warning: skipped previously applied
//!   commit 7a4b88a`. The one shape where merge and rebase differ in what ends
//!   up in the history rather than only in its shape.
//! * [`Shape::Octopus`] — `main` is a four-parent merge, `oct-side` diverged. The
//!   only shape where `--rebase=merges` has a real merge to replay.
//! * [`Shape::Unrelated`] — two roots, so a two-head pull has no merge base at
//!   all and the octopus strategy has to say so.
//! * [`Shape::MergeableDirty`] — diverged *and* dirty, which is the autostash
//!   premise.
//!
//! The second route to divergence needs no diverged branch at all: **more than
//! one merge head**. `get_can_ff` returns 0 whenever `merge_heads->nr > 1`, so
//! `pull <remote> <a> <b>` is divergent by construction even when both heads are
//! descendants — which is how `BehindRemote` reaches the refusal from `main`.
//!
//! # Determinism
//!
//! Every case below that merges or rebases **creates commits**, so its result is
//! an object id and the case is only meaningful if that id is reproducible.
//! `env::harden` pins author and committer identity and both dates, which is
//! what makes it so. Nothing here spells `--date`, `--amend --reset-author` or
//! a relative date, which are the three ways a pinned clock is escaped.
//!
//! What that claim actually rests on: the harness runs every case a second time
//! on the stock side and reports a case stock cannot reproduce as
//! `NONDETERMINISTIC` rather than scoring it. The whole `pull` corpus comes back
//! with none — its `unmeasurable + flaky` section is empty — and each case added
//! to this file was additionally run twice by hand against stock 2.55.0 in two
//! fresh copies of its shape and compared on stdout, stderr, exit code,
//! `rev-parse HEAD`, `for-each-ref`,
//! `cat-file --batch-all-objects --batch-check`, `status --porcelain` and
//! `FETCH_HEAD`.
//!
//! Two things are deliberately *not* here because they cannot be measured:
//!
//! * **`pull --progress`'s progress rendering.** The port paints a cursor-up
//!   animation onto a non-TTY stderr where stock prints nothing at all, and it
//!   paints a *different* animation each run — three runs of the identical argv
//!   on `BehindRemote` gave 260, 186 and 186 bytes of stderr, two distinct byte
//!   streams. A strict case would be a coin flip, so the case below is not
//!   strict and the finding is recorded here instead.
//! * **A pull into an unborn branch.** Every shape's `build` commits before the
//!   shape's own construction starts, so no fixture has an unborn `HEAD`, and a
//!   case cannot create one. Unreachable, not skipped.
//!
//! # The two findings that are not attached to any one group
//!
//! Both were reproduced by hand, stock and port side by side, and both are
//! invisible in stdout and in the exit code — they are only in the repository
//! the pull leaves behind, which is why the state digest is what catches them.
//!
//! * **The conflict marker names the wrong thing.** Every case here that
//!   reaches a conflicted merge — the whole mode-selection family on
//!   `CrissCross`, `--no-rebase --ff-only`, `pull.ff=false` — writes
//!   `clash.txt` with a closing marker. Stock writes the merge head's object id
//!   (`>>>>>>> 9aaef3afaec39230ab1e4e993beb31aa2a16c2ce`); the port writes the
//!   literal `>>>>>>> FETCH_HEAD`. That lands in the worktree file the user
//!   resolves *and* in the `AUTO_MERGE` blob and tree, so the two repositories
//!   hold different objects; a `diff -rq` of two finished fixtures shows that
//!   line and nothing else. `merge.conflictStyle=diff3` and `zdiff3` are here
//!   for the same reason: the port reproduces the nested
//!   `<<<<<<<<< Temporary merge branch 1` block that two merge bases produce,
//!   byte for byte, and still gets that one token wrong in all three styles.
//! * **`ORIG_HEAD` on a refusal.** `pull --ff-only . cc-right` refuses and
//!   *leaves `ORIG_HEAD` alone* — the fixture's own `27e7a99118…` survives.
//!   The port overwrites it with the current `HEAD` (`9efbd8fe2f…`) on the way
//!   out, so `git reset --hard ORIG_HEAD` after a failed pull no longer names
//!   what it did before. Reached by three of the refusal rows below.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

pub fn cases(out: &mut Vec<Case>) {
    divergent_branches_refusal(out);
    integration_mode_selection(out);
    fast_forward_policy(out);
    strategy_selection(out);
    merge_commit_shape(out);
    autostash(out);
    fetch_half_forwarding(out);
    remote_and_refspec_forms(out);
    annotated_tag_heads(out);
}

/// A pull whose entire contract is its refusal: stdout stays empty, so a
/// non-strict case would compare nothing against nothing.
fn refuse(args: &[&str], shape: Shape) -> Case {
    Case::strict("pull", args, shape)
}

fn p(out: &mut Vec<Case>, args: &[&str], shape: Shape) {
    out.push(Case::new("pull", args, shape));
}

/// The 2.34 refusal, in every spelling a user reaches it by.
///
/// Observed on stock 2.55.0, `Shape::CrissCross`, `git pull . cc-right`, with an
/// empty stdout and exit **128**:
///
/// ```text
/// From .
///  * branch            cc-right   -> FETCH_HEAD
/// hint: You have divergent branches and need to specify how to reconcile them.
/// hint: You can do so by running one of the following commands sometime before
/// hint: your next pull:
/// hint:
/// hint:   git config pull.rebase false  # merge
/// hint:   git config pull.rebase true   # rebase
/// hint:   git config pull.ff only       # fast-forward only
/// hint:
/// hint: You can replace "git config" with "git config --global" to set a default
/// hint: preference for all repositories. You can also pass --rebase, --no-rebase,
/// hint: or --ff-only on the command line to override the configured default per
/// hint: invocation.
/// fatal: Need to specify how to reconcile divergent branches.
/// ```
///
/// Three cases exist only to pin that the hint is **not** advice-gated: on
/// stock, `--no-advice`, `advice.diverging=false` and `GIT_ADVICE=0` each
/// produce the block above byte for byte. `GIT_ADVICE` is deliverable because
/// `env::harden` clears the environment and does not pin it, so setting it is
/// additive and symmetric. The port matches two of the three and drops all
/// twelve `hint:` lines under `advice.diverging=false` — the divergence the
/// group was written to find.
///
/// `--ff-only` and `pull.ff=only` take a *different* exit from the same state —
/// `die_ff_impossible`, which is advice-gated, so the pair with and without
/// `advice.diverging` is the one place a suppressible and a non-suppressible
/// hint can be compared side by side:
///
/// ```text
/// hint: Diverging branches can't be fast-forwarded, you need to either:
/// hint:
/// hint: 	git merge --no-ff
/// hint:
/// hint: or:
/// hint:
/// hint: 	git rebase
/// hint:
/// hint: Disable this message with "git config set advice.diverging false"
/// fatal: Not possible to fast-forward, aborting.
/// ```
///
/// The last three reach the refusal without a diverged branch, through
/// `merge_heads->nr > 1`. `pull origin main div` on `BehindRemote` is a
/// fast-forwardable branch and two heads that are both its descendants, and
/// stock still refuses with the twelve-line block; the port never asks the
/// question and runs the octopus merge instead — stdout
/// `Fast-forwarding to: 91bfcd8dad…` then
/// `Trying simple merge with 79f764eb81…`, stderr
/// `error: Entry 'clash.txt' not uptodate. Cannot merge.` and
/// `Merge with strategy octopus failed.`, exit **2** against stock's 128, and
/// `AUTO_MERGE` left behind where stock leaves only `FETCH_HEAD` and
/// `ORIG_HEAD`.
///
/// `pull . alien alien-clash` on `Unrelated` is the width case, and it is a
/// divergence of its own: stock pads the ref-name column to the longest name it
/// is reporting and writes ` * branch            alien      -> FETCH_HEAD`,
/// while the port writes one space more (`alien       -> FETCH_HEAD`) and gets
/// the `alien-clash` line right. One ref cannot show it — the column is only
/// computed once there is a set to take a maximum over.
fn divergent_branches_refusal(out: &mut Vec<Case>) {
    out.push(refuse(&["pull", ".", "cc-right"], Shape::CrissCross));
    out.push(
        refuse(&["pull", ".", "cc-right"], Shape::CrissCross).with_globals(&[&["--no-advice"]]),
    );
    out.push(
        refuse(&["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("advice.diverging", "false")]),
    );
    out.push(
        refuse(&["pull", ".", "cc-right"], Shape::CrissCross).with_env(&[("GIT_ADVICE", "0")]),
    );

    // The same refusal from the second diverged shape, where a merge would have
    // succeeded — so the refusal is not standing in for a merge that could not
    // run anyway.
    out.push(refuse(&["pull", ".", "main"], Shape::Cherry));

    // `--squash` selects no integration mode, so it does not answer the
    // question and the refusal still fires. A port that treats any merge-side
    // option as "the user asked for a merge" passes every other case here.
    out.push(refuse(&["pull", "--squash", ".", "main"], Shape::Cherry));

    // The fast-forward-only exit from the identical state, and its advice gate.
    out.push(refuse(&["pull", "--ff-only", ".", "cc-right"], Shape::CrissCross));
    out.push(
        refuse(&["pull", "--ff-only", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("advice.diverging", "false")]),
    );
    out.push(
        refuse(&["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.ff", "only")]),
    );

    // Divergence by head *count* rather than by topology: both heads are
    // descendants of the checked-out branch and stock refuses anyway.
    out.push(refuse(&["pull", "origin", "main", "div"], Shape::BehindRemote));
    // Two heads with no merge base at all. The stderr also pins the width of
    // the ref-name column in the fetch report, which is computed from the
    // longest name in the set and is therefore only visible with two refs.
    out.push(refuse(&["pull", ".", "alien", "alien-clash"], Shape::Unrelated));
    out.push(refuse(&["pull", ".", "oct-side", "oct-a"], Shape::Octopus));
}

/// Which of merge / rebase / rebase-merges / rebase-interactive a pull runs, as
/// chosen by argv, by `pull.rebase`, and by `branch.<name>.rebase`.
///
/// `Shape::CrissCross` is the discriminator: from `cc-left`, a merge of
/// `cc-right` **conflicts** on `clash.txt` and exits 1 with `MERGE_HEAD` left
/// behind; a plain rebase flattens `cc-left` onto `cc-right`, replays the one
/// non-merge commit and lands `252bced`; `--rebase=merges` replays *two* — the
/// `cc-left merge` commit as a merge and the tip above it — and lands
/// `4d7b486`. Three modes, three different resulting HEADs, and the third keeps
/// a merge in the history the second flattened away, so no mode can be mistaken
/// for another.
/// `Shape::Cherry` asks the same question where the *content* differs: rebase
/// drops the duplicate patch and warns `skipped previously applied commit
/// 7a4b88a`, merge keeps it.
///
/// The two `interactive` spellings are a port defect, not a corner: with
/// `GIT_SEQUENCE_EDITOR=true` — which `env::harden` pins, so the todo list is
/// accepted unedited — stock runs an ordinary rebase and lands the same
/// `252bced` a plain `--rebase` does. The port refuses outright with
/// `zvcs: pull: --rebase=interactive is not supported (interactive todo editing
/// needs a TTY editor loop)`, exit 1, `HEAD` still on `cc-left`'s `9efbd8fe`.
///
/// `preserve` was superseded by `merges` and is rejected by the parser
/// (`error: preserve: 'preserve' superseded by 'merges'`, exit 129); an invalid
/// `pull.rebase` value dies 128 with `fatal: invalid value for 'pull.rebase':
/// 'bogus'`. Neither writes a `FETCH_HEAD`, on either side, so both really are
/// rejected before the fetch — unlike `--cleanup=bogus`, which is the one
/// invalid value the port validates *after* fetching. Both rows are `strict`,
/// because their stdout is empty and the message is the entire behaviour, and
/// the port reproduces both byte for byte. An *empty* `pull.rebase` is not
/// invalid:
/// stock reads it as false, merges, and conflicts, which is the third answer the
/// value parser can give and the one neither of the other two rows reaches.
///
/// `merge.conflictStyle` is here rather than in a merge module because the
/// conflicted merge these modes produce is the only place this file writes
/// conflict markers, and the marker's closing label is where the port is wrong
/// (see the module header). Three styles, one wrong token in each.
fn integration_mode_selection(out: &mut Vec<Case>) {
    // Argv spellings, on the shape where the three modes land three HEADs.
    for args in [
        &["pull", "--rebase", ".", "cc-right"][..],
        &["pull", "-r", ".", "cc-right"][..],
        &["pull", "--rebase=true", ".", "cc-right"][..],
        &["pull", "--rebase=false", ".", "cc-right"][..],
        &["pull", "--no-rebase", ".", "cc-right"][..],
        &["pull", "--rebase=merges", ".", "cc-right"][..],
        &["pull", "--rebase=interactive", ".", "cc-right"][..],
    ] {
        p(out, args, Shape::CrissCross);
    }
    // Rejected by the option parser before anything runs: empty stdout, so the
    // one-line `error:` is the whole of it.
    out.push(refuse(&["pull", "--rebase=preserve", ".", "cc-right"], Shape::CrissCross));

    // The same values delivered as `pull.rebase`, which is how a user actually
    // answers the refusal above.
    for value in ["false", "true", "merges", "interactive", ""] {
        out.push(
            Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross)
                .with_config(&[("pull.rebase", value)]),
        );
    }
    // An unparseable value dies before the fetch, with an empty stdout.
    out.push(
        Case::strict("pull", &["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.rebase", "bogus")]),
    );

    // The conflicted merge under the two non-default marker styles. Both
    // reproduce git's nested two-base rendering exactly and both name the merge
    // head `FETCH_HEAD` where stock names its object id.
    for style in ["diff3", "zdiff3"] {
        out.push(
            Case::new("pull", &["pull", "--no-rebase", ".", "cc-right"], Shape::CrissCross)
                .with_config(&[("merge.conflictStyle", style)]),
        );
    }

    // `branch.<name>.rebase` is read for the *checked-out* branch and outranks
    // `pull.rebase`. Both directions of the override, so a port that reads only
    // one of the two keys fails one of them whichever it picked.
    out.push(
        Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("branch.cc-left.rebase", "true")]),
    );
    out.push(
        Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.rebase", "true"), ("branch.cc-left.rebase", "false")]),
    );
    out.push(
        Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.rebase", "false"), ("branch.cc-left.rebase", "merges")]),
    );

    // Argv outranks config, in both directions.
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.rebase", "true")]),
    );
    out.push(
        Case::new("pull", &["pull", "--rebase", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.rebase", "false")]),
    );

    // The same key from the two scopes a user really sets it in. `-c` proves the
    // value is honoured; a file proves it is *parsed* — and `pull.rebase` in
    // `~/.gitconfig` is the single most common way this setting exists at all.
    out.push(
        Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Global, "pull.rebase", "true"),
        ]),
    );
    out.push(
        Case::new("pull", &["pull", ".", "cc-right"], Shape::CrissCross).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Global, "pull.rebase", "true"),
            ConfigEntry::set(ConfigScope::Repo, "pull.rebase", "false"),
        ]),
    );
    out.push(
        Case::new("pull", &["pull", ".", "main"], Shape::Cherry).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "branch.topic.rebase", "true"),
        ]),
    );

    // Where merge and rebase differ in *content*, not only in shape: the shared
    // patch is dropped by one and kept by the other.
    p(out, &["pull", "--rebase", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--rebase=merges", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--rebase", "--no-verify-signatures", ".", "main"], Shape::Cherry);

    // A rebase that has to replay a real merge commit. Stock lands `7418eabd34…`
    // on `main`. The port prints `Rebasing (1/13)` … `Rebasing (13/13)`, then
    // dies:
    //
    //   zvcs: pull: octopus `merge` is not ported (git runs `git merge -s
    //   octopus` for more than one merge head); the rebase is still resumable
    //   with `git rebase --abort`
    //
    // and leaves the rebase unfinished — `HEAD` detached at `ec5d0d9933…`, four
    // `refs/rewritten/*` still present, `main` unmoved at `dc580741a2…`.
    p(out, &["pull", "--rebase=merges", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--rebase", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--rebase", ".", "cg-loose"], Shape::CommitGraph);
}

/// `--ff` / `--no-ff` / `--ff-only` and `pull.ff`, on a branch that *can* fast
/// forward and on one that cannot.
///
/// `BehindRemote`'s `main` is three commits behind its upstream with an unstaged
/// edit to a file the upstream never touches, so the fast-forward has to succeed
/// **over** a dirty worktree — and `--no-ff` has to build a merge commit from
/// the same state. `CrissCross` and `Cherry` supply the other half, where no
/// fast-forward exists.
///
/// The precedence pair is the finding. `pull.ff=only` alone refuses a diverged
/// pull, but `pull.ff=only` together with an explicit `--no-rebase` does **not**:
/// stock 2.55.0 merges, landing `72d1cbb` on `Cherry` and a conflicted index on
/// `CrissCross`. The port applies the config key regardless and dies
/// `fatal: Not possible to fast-forward, aborting.` with exit 128 — a refusal
/// where stock committed, on both shapes.
fn fast_forward_policy(out: &mut Vec<Case>) {
    // Fast-forwardable, over a dirty worktree.
    p(out, &["pull"], Shape::BehindRemote);
    p(out, &["pull", "--ff-only"], Shape::BehindRemote);
    p(out, &["pull", "--ff"], Shape::BehindRemote);
    p(out, &["pull", "--no-ff"], Shape::BehindRemote);
    p(out, &["pull", "--no-ff", "--no-commit"], Shape::BehindRemote);
    for value in ["true", "false", "only"] {
        out.push(
            Case::new("pull", &["pull"], Shape::BehindRemote).with_config(&[("pull.ff", value)]),
        );
    }

    // Not fast-forwardable. `--ff` and `pull.ff=true` are the *defaults*, so
    // they still have to reach a merge rather than a refusal.
    p(out, &["pull", "--ff", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-ff", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--ff-only", ".", "cc-right"], Shape::CrissCross);
    for value in ["true", "false"] {
        out.push(
            Case::new("pull", &["pull", ".", "main"], Shape::Cherry)
                .with_config(&[("pull.ff", value)]),
        );
    }
    // Rejected before the fetch, empty stdout, so the message is the behaviour:
    // `fatal: invalid value for 'pull.ff': 'bogus'`, exit 128.
    out.push(
        Case::strict("pull", &["pull", ".", "main"], Shape::Cherry)
            .with_config(&[("pull.ff", "bogus")]),
    );

    // The precedence pair: an explicit mode on the command line suppresses
    // `pull.ff` from config, on both diverged shapes.
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "main"], Shape::Cherry)
            .with_config(&[("pull.ff", "only")]),
    );
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.ff", "only")]),
    );
    out.push(
        Case::new("pull", &["pull", "--rebase", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.ff", "only")]),
    );
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "cc-right"], Shape::CrissCross)
            .with_config(&[("pull.ff", "false")]),
    );
}

/// Which merge strategy a pull hands the merge half — including the two keys
/// only `pull` reads.
///
/// `pull.twohead` and `pull.octopus` exist nowhere in `git merge`: pull.c reads
/// them and turns them into `-s <strategy>` for the child, choosing between them
/// on the number of merge heads. So `merge_strategies.rs` cannot reach either,
/// and a port that hard-codes `ort` regardless passes every `merge` case in the
/// corpus.
///
/// `Shape::Octopus`, one head, `pull.twohead=resolve` versus the default `ort`,
/// is where the two strategies produce different stdout (`Trying really trivial
/// in-index merge...` and friends) from the same input. Two or more heads on the
/// same shape select `pull.octopus` instead, and `octopus` on an unrelated pair
/// (`Shape::Unrelated`) is where the strategy has to report having no merge base:
/// stock prints `Unable to find common commit with 492da05ca6…` and
/// `Automatic merge failed; fix conflicts and then commit the result.`; the port
/// prints `zvcs: pull: Could not find a merge-base between commits
/// 977f8e7e57… and 492da05ca6…` on stderr and neither stdout line, and without
/// `--allow-unrelated-histories` exits **1** where stock exits **128**.
///
/// Two of these rows leave the same `HEAD` on both sides and a different index,
/// which is the only place a strategy defect can hide once the commit id agrees:
///
/// * **`pull.twohead=resolve`** (both spellings, and the three-head row). Same
///   commit, but stock leaves a valid cache tree in `.git/index` — 613 bytes,
///   `TREE:53(<root>=7/1:d446382a3d…)` — and the port leaves the root entry
///   invalidated at 594 bytes, `TREE:34(<root>=-1/1)`. Stock re-reading the
///   port's repository has to rewrite the index on the next `write-tree`; it
///   does not have to rewrite its own.
/// * **`-s resolve` on `Cherry`.** Same commit `72d1cbbbd8…`, and the port's
///   index carries a `REUC` block recording three stages of `app.txt` that
///   stock's does not (433 bytes against 397), so the port ran a conflicting
///   merge internally and left the resolve-undo record behind. Its stderr says
///   so too: `error: Entry 'app.txt' not uptodate. Cannot merge.` where stock
///   says `error: Merge requires file-level merging`.
fn strategy_selection(out: &mut Vec<Case>) {
    for value in ["resolve", "ort", "recursive", "octopus", "bogus"] {
        out.push(
            Case::new("pull", &["pull", "--no-rebase", ".", "oct-side"], Shape::Octopus)
                .with_config(&[("pull.twohead", value)]),
        );
    }
    for value in ["resolve", "ort", "bogus"] {
        out.push(
            Case::new("pull", &["pull", "--no-rebase", ".", "oct-side", "oct-a"], Shape::Octopus)
                .with_config(&[("pull.octopus", value)]),
        );
    }
    // `pull.twohead` must *not* be consulted for a three-head merge, and
    // `pull.octopus` must not be consulted for a one-head one.
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "oct-side", "oct-a"], Shape::Octopus)
            .with_config(&[("pull.twohead", "resolve")]),
    );
    out.push(
        Case::new("pull", &["pull", "--no-rebase", ".", "oct-side"], Shape::Octopus)
            .with_config(&[("pull.octopus", "resolve")]),
    );

    // Argv strategies, which outrank both keys.
    p(out, &["pull", "--no-rebase", "-s", "resolve", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "-s", "ort", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "-X", "ours", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "-X", "theirs", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--no-rebase", "-X", "ours", ".", "oct-side"], Shape::Octopus);

    // Two unrelated heads, with and without the permission to merge them. The
    // one *with* permission is the only case in the corpus where the octopus
    // strategy has to fail on a missing merge base rather than on a conflict.
    p(out, &["pull", "--no-rebase", ".", "alien", "alien-clash"], Shape::Unrelated);
    p(
        out,
        &["pull", "--no-rebase", "--allow-unrelated-histories", ".", "alien", "alien-clash"],
        Shape::Unrelated,
    );
}

/// What the merge half is asked to build once the strategy has run: whether to
/// commit, whether to squash, and what message the commit carries.
///
/// `env::harden` pins `GIT_EDITOR=true` and `GIT_MERGE_AUTOEDIT=no`, so the
/// merge commit takes git's own default message and no editor ever opens. That
/// makes the *message* a comparable artifact rather than a prompt — and it is
/// the whole of the `--cleanup` finding, which is this file's answer to "where
/// does the port commit something different while both sides exit 0". Under
/// `--cleanup=verbatim` stock's merge commit ends
/// `… i n t o   t o p i c` and lands `bc90472571…`; the port's ends
/// `… i n t o   t o p i c \n` and lands `72d1cbbbd8…`, byte for byte the object
/// its *default*-cleanup merge produces. One trailing newline, two commit ids,
/// nothing on stdout or stderr to tell them apart, and a history that can never
/// converge afterwards. It is the same rule the port gets wrong in
/// `commit --cleanup=verbatim -F -`, reached through a completely different
/// caller — and the `Octopus` pair below shows it is not a property of
/// `Cherry`'s message either.
///
/// `--cleanup=bogus` is the composition-order case. Stock validates the mode
/// *before* the fetch runs — nothing is fetched, no fetch report is printed and
/// no `FETCH_HEAD` is written — while the port fetches first, prints
/// `From .` / ` * branch            main       -> FETCH_HEAD` and only then
/// rejects the value, leaving `FETCH_HEAD` behind. Same exit code (128), same
/// `fatal: Invalid cleanup mode bogus`, different repository.
fn merge_commit_shape(out: &mut Vec<Case>) {
    p(out, &["pull", "--no-rebase", "--squash", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-squash", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--commit", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-commit", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-commit", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--squash", "--no-rebase", ".", "oct-side"], Shape::Octopus);

    p(out, &["pull", "--no-rebase", "--edit", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-edit", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--signoff", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--log", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--log=2", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-log", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--stat", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--no-rebase", "--no-stat", ".", "main"], Shape::Cherry);

    for mode in ["verbatim", "whitespace", "strip", "scissors"] {
        let arg = format!("--cleanup={mode}");
        out.push(Case::new(
            "pull",
            &["pull", "--no-rebase", &arg, ".", "main"],
            Shape::Cherry,
        ));
    }
    // Strict. The divergence is in the state — stock leaves no `FETCH_HEAD` and
    // the port leaves `b0db3a776e…\tbranch 'main' of .` — but the *cause* is
    // only legible on stderr, where the port prints the two-line fetch report
    // above the `fatal: Invalid cleanup mode bogus` both sides share. Without
    // the strictness a reader gets a `FETCH_HEAD` line and has to guess why.
    out.push(refuse(&["pull", "--no-rebase", "--cleanup=bogus", ".", "main"], Shape::Cherry));

    // The same `verbatim` question on a second shape, so the missing terminator
    // is not a fact about `Cherry`'s merge message. `Octopus`'s one-head merge
    // writes `Merge branch 'oct-side'`: stock lands `99e11540…` under
    // `verbatim` and `95467b5c…` under `whitespace`, and the port lands
    // `95467b5c…` under both.
    p(out, &["pull", "--no-rebase", "--cleanup=verbatim", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--no-rebase", "--cleanup=whitespace", ".", "oct-side"], Shape::Octopus);
}

/// `--autostash` and the two config keys that turn it on, on the one shape that
/// is diverged *and* dirty.
///
/// `MergeableDirty` holds an unstaged edit to `hot.txt`; `div-hot` rewrites the
/// same path. So `--autostash` has to stash, integrate, and then fail to reapply
/// — the branch of the autostash path that actually has to preserve the user's
/// work. `merge_dirty.rs` owns the *refusal* on that shape and never spells
/// `--autostash`, which is the option that exists to make the refusal
/// unnecessary.
///
/// Stock stashes, rebases, reports the apply conflict, exits **0**, lands
/// `bea1f10d96…` and leaves the stash reachable at
/// `refs/stash 4127fed55265c81fb7afd9013d9af3749710c37b`. The port prints the
/// same `Created autostash: 4127fed`, lands the same `bea1f10d96…` with the same
/// exit code, adds `Auto-merging hot.txt` / `CONFLICT (content): Merge conflict
/// in hot.txt` to stdout, and tells the user on stderr:
///
/// ```text
/// Applying autostash resulted in conflicts.
/// Your changes are safe in the stash.
/// You can run "git stash pop" or "git stash drop" at any time.
/// ```
///
/// `git rev-parse refs/stash` then answers nothing at all. The stash commit
/// exists and no ref points at it, so the message names a recovery the
/// repository cannot perform. Both the `--autostash` and the `rebase.autoStash`
/// spellings reach it, which is what says the defect is in the autostash path
/// rather than in the option parse.
fn autostash(out: &mut Vec<Case>) {
    p(out, &["pull", "--rebase", "--autostash", ".", "div-hot"], Shape::MergeableDirty);
    p(out, &["pull", "--rebase", "--autostash", ".", "div-cold"], Shape::MergeableDirty);
    p(out, &["pull", "--no-rebase", "--autostash", ".", "div-cold"], Shape::MergeableDirty);
    p(out, &["pull", "--rebase", "--no-autostash", ".", "div-hot"], Shape::MergeableDirty);
    p(out, &["pull", "--rebase", ".", "div-hot"], Shape::MergeableDirty);

    for (key, args) in [
        ("rebase.autoStash", &["pull", "--rebase", ".", "div-hot"][..]),
        ("rebase.autoStash", &["pull", "--rebase", ".", "div-cold"][..]),
        ("merge.autoStash", &["pull", "--no-rebase", ".", "div-hot"][..]),
        ("merge.autoStash", &["pull", "--no-rebase", ".", "div-cold"][..]),
    ] {
        out.push(
            Case::new("pull", args, Shape::MergeableDirty).with_config(&[(key, "true")]),
        );
    }
    // `--no-autostash` on the command line against the key that turns it on.
    out.push(
        Case::new(
            "pull",
            &["pull", "--rebase", "--no-autostash", ".", "div-hot"],
            Shape::MergeableDirty,
        )
        .with_config(&[("rebase.autoStash", "true")]),
    );
    out.push(
        Case::new(
            "pull",
            &["pull", "--no-rebase", "--no-autostash", ".", "div-hot"],
            Shape::MergeableDirty,
        )
        .with_config(&[("merge.autoStash", "true")]),
    );
    // Autostash over a *fast-forward*, where nothing needs stashing but the
    // stash is taken anyway.
    p(out, &["pull", "--autostash", "--rebase"], Shape::BehindRemote);
    p(out, &["pull", "--no-autostash", "--rebase"], Shape::BehindRemote);
}

/// The options `pull` parses itself and forwards to its `fetch` child, and the
/// two places the forwarding is wrong.
///
/// `fetch_clone.rs` measures each of these against `fetch` directly, so a
/// difference here is a difference in the *forwarding*, which is the seam only a
/// pull case can see. Three of them do more than forward: `--set-upstream`
/// writes configuration after the fetch, `--depth`/`--deepen` change what the
/// merge half is then given to merge, and `--recurse-submodules` is re-parsed by
/// pull before fetch ever sees it.
///
/// * **`--deepen=1` on a complete repository.** Stock forwards it, the fetch is
///   a no-op deepening, and the pull fast-forwards `54f11d5..91bfcd8` exactly as
///   a bare `pull` does. The port instead reaches the divergence refusal and
///   exits 128 with `HEAD` unmoved — the option changed what the merge half
///   thought its merge head was.
/// * **`--set-upstream`.** Stock fast-forwards *and* rewrites
///   `branch.main.merge`; with `origin div` the two are visible separately,
///   because the rewritten upstream becomes `refs/heads/div` while the merge
///   itself aborts on the dirty `clash.txt`. The port refuses the option
///   outright — `zvcs: pull: --set-upstream is not supported (not exposed by
///   the high-level fetch)`, exit 1 — and does neither: `branch.main.merge` is
///   still `refs/heads/main` afterwards.
/// * **`--recurse-submodules=on-demand`** on a repository with no submodules at
///   all: stock fast-forwards `54f11d5..91bfcd8`, the port refuses with exit 1
///   and leaves `HEAD` on `54f11d58…`.
/// * **`--unshallow` on a complete repository** is a refusal both sides make,
///   with the identical `fatal: --unshallow on a complete repository does not
///   make sense`; stock exits **1** and the port exits **128**.
///
/// `--progress` is here without being strict. The port paints a cursor-up
/// progress animation onto a non-TTY stderr where stock prints nothing, and the
/// animation differs between runs of the same argv — so the stderr difference is
/// real and is not byte-comparable, and only the stdout and the resulting state
/// are claimed.
fn fetch_half_forwarding(out: &mut Vec<Case>) {
    for args in [
        &["pull", "--prune"][..],
        &["pull", "--tags"][..],
        &["pull", "--no-tags"][..],
        &["pull", "--depth=1"][..],
        &["pull", "--deepen=1"][..],
        &["pull", "--unshallow"][..],
        &["pull", "--update-shallow"][..],
        &["pull", "--jobs=2"][..],
        &["pull", "--progress"][..],
        &["pull", "-q"][..],
        &["pull", "-v"][..],
        &["pull", "-4"][..],
        &["pull", "-6"][..],
        &["pull", "--atomic"][..],
        &["pull", "--server-option=x"][..],
        &["pull", "--upload-pack=git-upload-pack"][..],
        &["pull", "--negotiation-tip=main"][..],
        &["pull", "--refmap=refs/heads/*:refs/remotes/other/*", "origin", "main"][..],
        &["pull", "--recurse-submodules=no"][..],
        &["pull", "--recurse-submodules=on-demand"][..],
        &["pull", "--no-recurse-submodules"][..],
        &["pull", "--set-upstream", "origin", "main"][..],
        &["pull", "--set-upstream", "origin", "div"][..],
        &["pull", "--verify-signatures"][..],
        &["pull", "--no-verify-signatures"][..],
    ] {
        p(out, args, Shape::BehindRemote);
    }

    // The same three depth options where they have something to do: a real
    // shallow clone with a second `.git/shallow` line to retire.
    p(out, &["pull", "--unshallow"], Shape::Shallow);
    p(out, &["pull", "--depth=1"], Shape::Shallow);
    p(out, &["pull", "--depth=3"], Shape::Shallow);
    p(out, &["pull", "--deepen=1"], Shape::Shallow);
    p(out, &["pull", "--rebase"], Shape::Shallow);
    p(out, &["pull", "--no-rebase", ".", "sh-side"], Shape::Shallow);
    // A partial clone, where the merge half has to fault objects in.
    p(out, &["pull", "--no-rebase", ".", "pc-side"], Shape::Promisor);

    // Signature verification on a merge that actually runs. Nothing in the
    // fixtures is signed, so this is the "no signature" path both on a head that
    // needs merging and on one that is already up to date.
    p(out, &["pull", "--no-rebase", "--verify-signatures", ".", "main"], Shape::Cherry);
    p(out, &["pull", "--rebase", "--verify-signatures", ".", "main"], Shape::Cherry);
}

/// Where the merge head comes from: the upstream configuration, a named remote,
/// an explicit refspec, or nothing at all.
///
/// The no-upstream refusals are strict because the message *is* the behaviour —
/// and because the two of them are not the same message. Stock's merge-side
/// advice names `<remote>/<branch>`; its rebase-side advice hard-codes
/// `origin/<branch>` even in a repository with no remotes at all, on the line
///
/// ```text
///     git branch --set-upstream-to=origin/<branch> main
/// ```
///
/// The port prints `<remote>/<branch>` there, so it matches the merge-side
/// refusal exactly and diverges on the rebase-side one by that single token —
/// on `Linear` and on `Branched` alike, and on no other line of the fifteen.
/// `Detached` takes a third exit (`You are not currently on a branch.`) with no
/// upstream advice at all, and both sides agree on it.
///
/// `pull origin refs/heads/nope` is strict for the same reason: the head does
/// not exist, stdout is empty, and `fatal: couldn't find remote ref
/// refs/heads/nope` is the entire result. Both sides produce it.
///
/// The refspec forms are here for what the *merge* half is then handed:
/// `origin main:refs/remotes/origin/other` fetches into a tracking ref and
/// merges the same head, `+refs/heads/div:refs/heads/copy` writes a local branch
/// as a side effect of a pull, and `origin refs/heads/nope` is the head that
/// does not exist. `refspec_algebra.rs` owns whether those specs *match*; what
/// is measured here is what ends up merged and what `FETCH_HEAD` records as
/// `not-for-merge`.
fn remote_and_refspec_forms(out: &mut Vec<Case>) {
    out.push(refuse(&["pull", "--rebase"], Shape::Linear));
    out.push(refuse(&["pull", "--rebase"], Shape::Branched));
    out.push(refuse(&["pull", "--rebase"], Shape::Detached));
    out.push(refuse(&["pull"], Shape::Branched));
    out.push(refuse(&["pull", "--ff-only"], Shape::Linear));

    p(out, &["pull", "origin"], Shape::BehindRemote);
    p(out, &["pull", "origin", "main"], Shape::BehindRemote);
    p(out, &["pull", "origin", "div"], Shape::BehindRemote);
    p(out, &["pull", "--ff-only", "origin", "div"], Shape::BehindRemote);
    p(out, &["pull", "--rebase", "origin", "main", "div"], Shape::BehindRemote);
    p(out, &["pull", "--no-rebase", "origin", "main", "div"], Shape::BehindRemote);
    p(out, &["pull", "--ff-only", "origin", "main", "div"], Shape::BehindRemote);
    p(out, &["pull", "origin", "main:refs/remotes/origin/other"], Shape::BehindRemote);
    p(out, &["pull", "origin", "+refs/heads/div:refs/heads/copy"], Shape::BehindRemote);
    p(out, &["pull", ".", "refs/heads/div:refs/heads/copy2"], Shape::BehindRemote);
    out.push(refuse(&["pull", "origin", "refs/heads/nope"], Shape::BehindRemote));

    // Two more shapes whose merge head is a plain diverged branch, so the
    // integration groups above are not all measured on the same two histories.
    p(out, &["pull", "--no-rebase", ".", "cg-loose"], Shape::CommitGraph);
    p(out, &["pull", "--no-rebase", ".", "oct-side"], Shape::Octopus);
    p(out, &["pull", "--no-rebase", ".", "oct-side", "oct-a", "oct-b"], Shape::Octopus);
}

/// A pull whose merge head is an **annotated tag**, which four different code
/// paths in the port fail to peel.
///
/// `Shape::Branched` carries `v0.2.0`, an annotated tag object whose target is
/// already reachable from `main`, and `v0.1.0`, a lightweight tag on `main`
/// itself. Stock resolves the tag object to its commit and answers
/// `Already up to date.` with exit 0 for every spelling below; the lightweight
/// tag, which needs no peeling, matches on both sides.
///
/// The port reads the tag object's own id as the merge head, and each spelling
/// then fails somewhere else:
///
/// ```text
/// git pull . v0.2.0                       fatal: Need to specify how to reconcile divergent branches.  (128)
/// git pull --ff-only . v0.2.0             fatal: refusing to merge unrelated histories                 (128)
/// git pull --verify-signatures . v0.2.0   fatal: Commit d7277ea does not have a GPG signature.         (128)
/// git pull . tag v0.2.0                   fatal: couldn't find remote ref FETCH_HEAD                   (128)
/// ```
///
/// Four distinct messages, one cause, and all four where stock exits 0 having
/// done nothing. The `FETCH_HEAD` the fetch half writes is identical on both
/// sides for the first three — `d7277ea975… tag 'v0.2.0' of .` — so the tag is
/// fetched correctly and only the merge half misreads it; the *fourth* leaves
/// `FETCH_HEAD` empty, which is why its message is about `FETCH_HEAD` rather
/// than about the tag. `ORIG_HEAD` splits three-to-one the other way: stock
/// writes `5915d79de1…` for all four, and the port writes it only for
/// `--ff-only`.
fn annotated_tag_heads(out: &mut Vec<Case>) {
    p(out, &["pull", ".", "v0.2.0"], Shape::Branched);
    p(out, &["pull", "--ff-only", ".", "v0.2.0"], Shape::Branched);
    p(out, &["pull", "--no-rebase", "--verify-signatures", ".", "v0.2.0"], Shape::Branched);
    p(out, &["pull", ".", "tag", "v0.2.0"], Shape::Branched);
    p(out, &["pull", "--no-rebase", ".", "v0.1.0"], Shape::Branched);
    p(out, &["pull", "--tags", "--no-rebase", ".", "main"], Shape::Branched);
    p(out, &["pull", "--no-tags", "--no-rebase", ".", "feature"], Shape::Branched);
}
