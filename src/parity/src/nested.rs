//! Nested-option coverage: one declared matrix per subcommand, expanded to every
//! combination of the flags that interact.
//!
//! # Why this exists apart from the hand-written corpus
//!
//! The corpus is a list of invocations someone thought of. That works for a
//! command whose flags are independent — `rev-parse --short` and `rev-parse
//! --abbrev-ref` cannot break each other — and fails for the commands where the
//! flags are the semantics. `stash push -u -k -- <path>` is four decisions about
//! *what moves where*, and each pair of them has its own answer; `pull --rebase
//! --autostash` over a dirty tree is a different command from either flag alone.
//! Those are exactly the commands whose bugs kept reaching a human instead of the
//! harness, which is the whole reason for this module.
//!
//! A matrix is a base argv plus a list of *slots*. Each slot is a set of
//! alternatives, one of which is usually "nothing at all", and the expansion is
//! their cartesian product. Twelve flags in four slots is 40-ish cases rather
//! than the 4096 an unstructured product would give, because the slots are chosen
//! to hold the flags that actually interact and to leave out the ones that do not.
//!
//! # Why these cases compare stderr
//!
//! Every command here can *refuse*, and for a refusal the message is the whole
//! observable behaviour: `git pull` over a dirty tree, `git stash pop` on an empty
//! stack, `git merge` that would overwrite a local edit. Each of those shipped
//! with the wrong message at least once while stdout, exit code and resulting
//! state all agreed — so these cases opt into byte-comparing stderr
//! ([`Case::strict`]). The rest of the corpus keeps the old policy, so no
//! previously reported score moves because of this file.
//!
//! # Why every matrix states its size
//!
//! Crossing every flag with every other spends the whole budget on combinations
//! git rejects at parse time, before any behaviour runs: `commit`'s option
//! parser alone would give thousands of rows that all print one of four
//! `cannot be used together` lines. So a slot holds the alternatives that *one
//! decision* reads — which message source, which author identity, which paths
//! are committed — and two slots are crossed only when one decision's answer
//! changes the other's. Each matrix below names its size and the decision that
//! bounds it, and `no_matrix_explodes` fails the build if any one of them grows
//! past the 36 rows of the largest, [`STASH_PUSH`] — the point at which a slot
//! has stopped being a decision and started being a flag list.

use crate::fixture::Shape;
use crate::runner::Case;

/// One subcommand's option matrix.
struct Matrix {
    /// Scoring bucket, e.g. `stash`.
    cmd: &'static str,
    /// Argv every expansion starts with, e.g. `["stash", "push"]`.
    base: &'static [&'static str],
    /// The interacting flags, grouped so that one alternative per group is chosen.
    /// An empty alternative means "this group contributes nothing".
    slots: &'static [&'static [&'static [&'static str]]],
    /// Argv appended after every expansion — a pathspec, usually, which has to
    /// stay last.
    tail: &'static [&'static str],
    /// Whole argvs this matrix must *not* emit, because the curated corpus
    /// already carries that exact invocation against that exact shape.
    ///
    /// A case's identity is `<shape>::<cmd>::<argv>`, so a matrix whose
    /// all-empty combination reproduces a hand-written case does not add a
    /// measurement — it adds a second row with the same id, and both the
    /// denominator and any failure it reports are then counted twice. The
    /// corpus keeps the case; this list keeps the matrix from restating it.
    ///
    /// Every entry is checked against the product below: an entry the matrix
    /// could not have produced is a typo silently filtering nothing, which is
    /// exactly how a bounded matrix quietly stops covering what it claims to.
    omit: &'static [&'static [&'static str]],
    shape: Shape,
}

impl Matrix {
    /// Expand to one case per combination, less the rows [`Matrix::omit`] names
    /// as already curated.
    fn expand(&self, out: &mut Vec<Case>) {
        let mut argvs: Vec<Vec<&'static str>> = vec![self.base.to_vec()];
        for slot in self.slots {
            let mut next = Vec::with_capacity(argvs.len() * slot.len());
            for argv in &argvs {
                for alt in *slot {
                    let mut one = argv.clone();
                    one.extend_from_slice(alt);
                    next.push(one);
                }
            }
            argvs = next;
        }
        for mut argv in argvs {
            argv.extend_from_slice(self.tail);
            if self.omit.iter().any(|skip| skip == &argv.as_slice()) {
                continue;
            }
            out.push(Case::strict(self.cmd, &argv, self.shape));
        }
    }
}

/// `git stash`'s decisions about what moves: which kinds of change are taken, what
/// is left behind, and whether a pathspec narrows it.
///
/// `-u`/`-a` (untracked, then ignored too), `-k` (leave the index), `-S` (stage
/// only) and a pathspec each change the answer, and they change it *together*:
/// `-k -S` is a contradiction git resolves one way, `-u -- <path>` another.
const STASH_PUSH: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "push"],
    slots: &[
        &[&[], &["-u"], &["-a"]],
        &[&[], &["-k"], &["--no-keep-index"]],
        &[&[], &["-S"]],
        &[&[], &["-q"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Stashed,
};

/// The same decisions with a pathspec, which git treats as a different code path
/// (`--` forces the push form and changes which flags are legal).
const STASH_PUSH_PATHS: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "push"],
    slots: &[
        &[&[], &["-u"]],
        &[&[], &["-k"]],
        &[&[], &["-m", "msg"]],
    ],
    tail: &["--", "counter.txt"],
    omit: &[],
    shape: Shape::Stashed,
};

/// Taking an entry back out: which entry, whether the index comes with it, and
/// whether the entry survives.
const STASH_RESTORE: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["pop"], &["apply"]],
        &[&[], &["--index"]],
        &[&[], &["-q"]],
        &[&[], &["stash@{1}"], &["stash@{9}"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Stashed,
};

/// Reading the stack. These cannot refuse much, but they are where a wrong entry
/// order or a wrong `--include-untracked` rendering shows up.
const STASH_READ: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["list"], &["show"]],
        &[&[], &["-p"], &["--stat"], &["--name-only"]],
        &[&[], &["stash@{1}"], &["stash@{2}"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Stashed,
};

/// Dropping and branching, which mutate the stack and so are judged on the
/// resulting state as much as on the message.
const STASH_DROP: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["drop"], &["branch", "off-stash"]],
        &[&[], &["stash@{1}"], &["stash@{9}"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Stashed,
};

/// `git pull`'s integration policy over a dirty worktree: fast-forward or merge or
/// rebase, whether the dirt is stashed for the duration, and whether a
/// fast-forward is required.
///
/// `main` is behind by three commits that do not touch the dirty file, so the
/// fast-forward has to succeed; `div` has diverged and rewrites a file the
/// worktree is holding dirty, so that one has to refuse — and the refusal is the
/// message this measures.
const PULL_MAIN: Matrix = Matrix {
    cmd: "pull",
    base: &["pull"],
    slots: &[
        &[&[], &["--rebase"], &["--no-rebase"], &["--ff-only"]],
        &[&[], &["--autostash"], &["--no-autostash"]],
        &[&[], &["-q"], &["-v"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::BehindRemote,
};

/// The same policies against the diverged branch, where every one of them has to
/// stop on the dirty path.
const PULL_DIV: Matrix = Matrix {
    cmd: "pull",
    base: &["pull", "origin", "div"],
    slots: &[
        &[&[], &["--rebase"], &["--no-rebase"], &["--ff-only"]],
        &[&[], &["--autostash"]],
        &[&[], &["--no-commit"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::BehindRemote,
};

/// `git merge` from the tracking ref, which is the same integration machinery
/// reached without the fetch — so a difference here localises a pull failure to
/// one half or the other.
const MERGE_TRACKING: Matrix = Matrix {
    cmd: "merge",
    base: &["merge"],
    slots: &[
        &[&["origin/main"], &["origin/div"]],
        &[&[], &["--ff-only"], &["--no-ff"], &["--squash"]],
        &[&[], &["--no-commit"]],
        &[&[], &["-q"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::BehindRemote,
};

/// `git fetch` against the fixture's own remote: the flags that decide what is
/// written, over a repository that is genuinely behind.
const FETCH_REMOTE: Matrix = Matrix {
    cmd: "fetch",
    base: &["fetch"],
    slots: &[
        &[&[], &["origin"], &["origin", "main"]],
        &[&[], &["--dry-run"]],
        &[&[], &["--tags"], &["--no-tags"]],
        &[&[], &["--prune"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::BehindRemote,
};


// ---------------------------------------------------------------------------
// `commit`: the flags that decide *what is committed and who by*.
//
// Every one of these pairs is a `die()` in `parse_and_validate_options()`
// (builtin/commit.c) that runs before a single object is written, which is
// exactly why a port can miss it and still look like it works: the message it
// does not print is the only evidence, and the commit it then creates is the
// damage. The corpus reached each of these flags alone; none of it crossed two.
// ---------------------------------------------------------------------------

/// Where the commit message comes from — and what happens when two sources are
/// named at once.
///
/// `-m`, `-F` and `-C` are three answers to one question, and git rejects any
/// two of them together at 128 before touching the index. `--squash` and
/// `--amend` are the two flags that change *which* sources are legal rather
/// than being sources themselves, so they are the second slot.
///
/// 4 × 4 = 16 rows, 15 emitted: one slot per source (plus "none"), crossed
/// with the slots that change the rule. `--fixup` is left out on purpose — it
/// reads the same `use_message`/`logfile` state `--squash` does, so it would
/// half again the matrix to re-measure one branch.
const COMMIT_SOURCE: Matrix = Matrix {
    cmd: "commit",
    base: &["commit"],
    slots: &[
        &[&[], &["-m", "one"], &["-F", "README.md"], &["-C", "HEAD"]],
        &[&[], &["-m", "two"], &["--squash", "HEAD"], &["--amend"]],
    ],
    tail: &[],
    // The bare `commit` on a dirty tree is a curated case already.
    omit: &[&["commit"]],
    shape: Shape::Dirty,
};

/// The same sources with unmerged entries in the index, which is a question of
/// *ordering*: git's option check runs first and dies at 128 with
/// `options '-m' and '-F' cannot be used together`, and only a command line
/// that survives it reaches the `unmerged files` refusal at exit 1.
///
/// A port that checks the index first reports the second message for a command
/// line git never got that far with — and one that checks neither writes the
/// commit. Both were reachable only by crossing the two.
///
/// 4 × 2 = 8 rows: the same source slot as [`COMMIT_SOURCE`], crossed with
/// `--amend` alone. The rest of the source pairs are already expanded on
/// [`Shape::Dirty`]; what this shape adds is which check speaks first.
const COMMIT_SOURCE_MERGE: Matrix = Matrix {
    cmd: "commit",
    base: &["commit"],
    slots: &[
        &[&[], &["-m", "one"], &["-F", "README.md"], &["-C", "HEAD"]],
        &[&[], &["--amend"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Conflicted,
};

/// Who the commit is attributed to. `--author` names an identity, and
/// `--reset-author` says to *stop* using the one being reused — so the two
/// together are a contradiction git rejects, and `--reset-author` alone is
/// meaningless unless something is being reused, which is what `-C` and
/// `--amend` do.
///
/// git's two refusals here are different sentences and one of them names a
/// third flag: `--reset-author can be used only with -C, -c or --amend.` A
/// port that collapses them to one message passes a case that only checks the
/// exit code.
///
/// 3 × 2 × 2 = 12 rows, 11 emitted: an identity slot (none, well-formed, malformed),
/// crossed with the flag that contradicts it and with the flag that makes it
/// legal. The malformed value is in the same slot rather than a fourth
/// dimension because `--author bogus` is decided by the same parse.
const COMMIT_AUTHOR: Matrix = Matrix {
    cmd: "commit",
    base: &["commit", "-m", "msg"],
    slots: &[
        &[&[], &["--author", "A U Thor <author@example.invalid>"], &["--author", "bogus"]],
        &[&[], &["--reset-author"]],
        &[&[], &["--amend"]],
    ],
    tail: &[],
    // `commit -m msg` on this shape is curated; only the identity rows are new.
    omit: &[&["commit", "-m", "msg"]],
    shape: Shape::Dirty,
};

/// Which paths the commit takes: everything tracked (`-a`), only the named
/// paths (`-o`), the named paths *plus* the index (`-i`), and the pair git
/// refuses. Each answer is a different tree, and each is only decidable
/// together with whether a pathspec was given — `-o` with no paths is an error,
/// `-a` with paths is a different error, and `-i -o` is a third.
///
/// 5 × 2 = 10 rows: one slot holding the three scope flags, the illegal pair,
/// and "none", crossed with the pathspec that changes which of them are legal.
/// [`Shape::Dirty`] carries a staged file, an unstaged edit and an untracked
/// one, so the three scopes produce three different commits rather than three
/// spellings of the same one.
const COMMIT_SCOPE: Matrix = Matrix {
    cmd: "commit",
    base: &["commit", "-m", "msg"],
    slots: &[
        &[&[], &["-a"], &["-o"], &["-i"], &["-i", "-o"]],
        &[&[], &["--", "README.md"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Dirty,
};

// ---------------------------------------------------------------------------
// `checkout` / `switch` / `restore`: three verbs over one worktree-writing
// machine, and the flags that decide whether it is allowed to write.
//
// The interesting fact about this family is that the same `-f` means different
// things in each: git lets `checkout -f` leave a conflicted merge and *refuses*
// `switch -f` from the same state, because `switch` checks for an in-progress
// operation before it looks at the force flag at all. A port that treats `-f`
// as one gate-lifter gets one of the two wrong, and the one it gets wrong
// discards `MERGE_HEAD`.
// ---------------------------------------------------------------------------

/// Creating a branch while switching to it: the three creation flags are one
/// slot because git rejects any two of them together, and `--detach` is the
/// fourth answer to the same question.
///
/// 4 rows, one per creation mode, all against a start point — which is the
/// second half of the refusal for `--orphan`, whose message is
/// `'--orphan' cannot take <start-point>` rather than an option-conflict line.
const SWITCH_CREATE: Matrix = Matrix {
    cmd: "switch",
    base: &["switch"],
    slots: &[&[&["-c", "nb"], &["-C", "nb"], &["--orphan", "nb"], &["-d"]]],
    tail: &["feature"],
    omit: &[],
    shape: Shape::Branched,
};

/// `switch` out of a conflicted merge. git refuses all of these at 128 with
/// `cannot switch branch while merging` — including the two that are supposed
/// to lift a gate — because the in-progress check is not a gate `-f` can lift.
///
/// This is the case where a lifted `-f` costs data: the merge's `MERGE_HEAD`,
/// `MERGE_MSG` and the conflicted index are gone the moment the switch happens,
/// and nothing prints. Compared on stderr, because the refusal *is* the
/// behaviour, and on state, because "did it switch anyway" is the rest of it.
///
/// 4 × 2 = 8 rows, 5 emitted: the three flags that could plausibly lift the
/// gate plus none, crossed with whether the destination is a branch or a
/// detached head —
/// the second is a different code path in `switch` and reaches the same check.
const SWITCH_MERGE_GATE: Matrix = Matrix {
    cmd: "switch",
    base: &["switch"],
    slots: &[
        &[&[], &["-f"], &["-m"], &["--discard-changes"]],
        &[&["theirs"], &["--detach", "theirs"]],
    ],
    tail: &[],
    // The three single-flag switches out of a merge are curated; the `--detach`
    // half of the cross and `--discard-changes` are not.
    omit: &[&["switch", "theirs"], &["switch", "-f", "theirs"], &["switch", "-m", "theirs"]],
    shape: Shape::Conflicted,
};

/// The same state through `checkout`, where the answer is genuinely different:
/// plain `checkout` fails at 1 with `you need to resolve your current index
/// first`, `-m` fails the same way, and `-f` *succeeds* and switches.
///
/// Pinning the asymmetry is the point. A port that copies its `switch` gate
/// into `checkout` refuses a command git performs; one that copies its
/// `checkout` gate into `switch` performs one git refuses.
///
/// 3 × 2 = 6 rows, 5 emitted: the force/merge/none slot crossed with whether a
/// pathspec narrows it, since `checkout -- <path>` never reaches the
/// branch-switch gate at all.
const CHECKOUT_MERGE_GATE: Matrix = Matrix {
    cmd: "checkout",
    base: &["checkout"],
    slots: &[
        &[&[], &["-f"], &["-m"]],
        &[&["theirs"], &["--", "conflict.txt"]],
    ],
    tail: &[],
    // Curated in the checkout corpus.
    omit: &[&["checkout", "--", "conflict.txt"]],
    shape: Shape::Conflicted,
};

/// What `restore` writes to: the index (`-S`), the worktree (`-W`), or both —
/// spelled short, long, and as one bundled short option.
///
/// `-SW` and `--staged --worktree` are the same request and git answers both
/// with exit 0; they are two rows here because a port that parses short options
/// one at a time rejects the bundle while accepting the pair, and no case that
/// spells only one of them can see that. The unmerged path is what makes the
/// answers differ at all: `-W` alone fails with `path 'conflict.txt' is
/// unmerged` while `-S` alone succeeds.
///
/// 5 × 2 = 10 rows: one slot per spelling of the destination, crossed with
/// whether the content comes from `HEAD` or from the index — the flag that
/// decides what `-W` alone even means.
const RESTORE_TARGET: Matrix = Matrix {
    cmd: "restore",
    base: &["restore"],
    slots: &[
        &[&[], &["-S"], &["-W"], &["-SW"], &["--staged", "--worktree"]],
        &[&[], &["--source", "HEAD"]],
    ],
    tail: &["conflict.txt"],
    omit: &[],
    shape: Shape::Conflicted,
};

/// `checkout` against a cone-mode sparse checkout, where `--no-overlay` decides
/// whether paths *missing from the worktree* are deletions to record.
///
/// Every sparse-excluded path is missing from the worktree by construction, so
/// an implementation that reads `--no-overlay` as "delete what the pathspec
/// covers and the worktree lacks" stages a deletion for every file outside the
/// cone — from one command that printed nothing. `--overlay` (the default) and
/// `--ignore-skip-worktree-bits` are the two flags that change which side of
/// that decision a path lands on, so they are the other two slots.
///
/// 3 × 2 × 2 = 12 rows: the overlay decision, the skip-worktree decision, and
/// two pathspecs — one that covers the whole tree and one that names only the
/// excluded directory, which git refuses as unmatched.
const CHECKOUT_SPARSE: Matrix = Matrix {
    cmd: "checkout",
    base: &["checkout"],
    slots: &[
        &[&[], &["--no-overlay"], &["--overlay"]],
        &[&[], &["--ignore-skip-worktree-bits"]],
        &[&["HEAD", "--", "."], &["HEAD", "--", "outside"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Sparse,
};


// ---------------------------------------------------------------------------
// `reset`: five modes over one in-progress operation.
//
// The mode is not a rendering choice, it is which of `HEAD`, the index and the
// worktree move — and two of the five are *refused* while a merge is
// unresolved, by two adjacent `die()`s in `cmd_reset()` that name the mode in
// the message. Performing one of those instead is not a wrong message, it is a
// merge silently thrown away.
// ---------------------------------------------------------------------------

/// The five modes with unmerged entries in the index. git performs `--mixed`,
/// `--hard` and `--merge` and refuses `--soft` and `--keep` at 128 with
/// `Cannot do a soft reset in the middle of a merge.` — the mode's own name
/// inside the sentence, so the message is the only thing that distinguishes the
/// two refusals from each other.
///
/// 5 × 2 = 10 rows, 8 emitted: one row per mode, crossed with whether a commit
/// is named.
/// The target matters because it decides whether the refusal is even reached:
/// resetting to the current commit and to another branch take the same path to
/// the mid-merge check but different ones after it.
const RESET_MERGE: Matrix = Matrix {
    cmd: "reset",
    base: &["reset"],
    slots: &[
        &[&["--soft"], &["--mixed"], &["--hard"], &["--merge"], &["--keep"]],
        &[&[], &["theirs"]],
    ],
    tail: &[],
    // Both refusals are curated one-liners in the reset corpus; what is new here
    // is the other three modes and the same five against a named commit.
    omit: &[&["reset", "--soft"], &["reset", "--keep"]],
    shape: Shape::Conflicted,
};

/// The same five modes over a *staged* change, which is the state that
/// separates `--keep` from `--merge`: both are documented as preserving local
/// work, and they disagree about whether a staged change survives as staged.
/// git's `--keep` resets the index to the target and leaves the change in the
/// worktree only; a port that keeps the index entry instead reports a clean
/// `M.` where git reports `.M`, with no message on either side.
///
/// 5 × 2 = 10 rows: the mode slot again, crossed with resetting in place versus
/// to a diverged branch — the second is what makes `--keep` and `--merge` have
/// anything to disagree about.
const RESET_KEEP: Matrix = Matrix {
    cmd: "reset",
    base: &["reset"],
    slots: &[
        &[&["--soft"], &["--mixed"], &["--hard"], &["--merge"], &["--keep"]],
        &[&[], &["div-cold"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::MergeableStaged,
};

// ---------------------------------------------------------------------------
// `branch`: the one gate `-f` must not lift.
//
// git refuses to move or delete a branch that a worktree has checked out, and
// it refuses it *for the force spellings too*: `-f`, `-M` and `-C` all reach
// the same `cannot force update the branch '%s' used by worktree at '%s'`.
// This is the class where a lifted gate is not a wrong message but a checkout
// whose `HEAD` now names a commit its index and worktree were never built
// from.
// ---------------------------------------------------------------------------

/// The seven operations that write a ref, against the branch this worktree has
/// checked out and against one it does not.
///
/// The pair is the whole point: `-f main feature` has to refuse and
/// `-f feature main` has to succeed, and an implementation with no
/// worktree check at all passes the second while destroying the first.
/// `-m`/`-M` and `-c`/`-C` are in the same slot as `-f` because they are the
/// same decision — is this ref allowed to move — reached through three
/// different subcommands of one builtin.
///
/// 7 × 2 = 14 rows, 12 emitted: one per operation, crossed with which of the
/// two branches is the target. Not crossed with `--force` separately: for
/// `branch` the force spelling *is* the operation, which is why they share a
/// slot.
const BRANCH_FORCE: Matrix = Matrix {
    cmd: "branch",
    base: &["branch"],
    slots: &[
        &[&["-f"], &["-M"], &["-m"], &["-c"], &["-C"], &["-d"], &["-D"]],
        &[&["main", "feature"], &["feature", "main"]],
    ],
    tail: &[],
    // The two renames away from the checked-out branch are curated; the force
    // updates *onto* it, which are the refusals this matrix exists for, are not.
    omit: &[&["branch", "-m", "feature", "main"], &["branch", "-M", "feature", "main"]],
    shape: Shape::Branched,
};

/// The same seven against a branch checked out in a *linked* worktree, which is
/// the case a `HEAD`-only check cannot see: `linked` is not this worktree's
/// branch, and git still refuses because some worktree holds it.
///
/// The two arguments make the row's own answer: `-f`/`-d`/`-D` act on `linked`
/// and are refused for the *other* worktree, while `-m`/`-M`/`-c`/`-C` write
/// `main` and are refused for this one. Each refusal prints the path of the
/// worktree that holds the branch, so the message carries the fact that the
/// implementation had to look somewhere other than `.git/HEAD` to know — and
/// which somewhere.
///
/// 7 rows, one per operation. The second slot from [`BRANCH_FORCE`] would only
/// re-measure the "not in use" half that shape already covers.
const BRANCH_WORKTREE: Matrix = Matrix {
    cmd: "branch",
    base: &["branch"],
    slots: &[&[&["-f"], &["-M"], &["-m"], &["-c"], &["-C"], &["-d"], &["-D"]]],
    tail: &["linked", "main"],
    omit: &[],
    shape: Shape::Worktree,
};

/// `worktree add`'s three answers to "what is checked out in the new worktree":
/// an existing ref, a new branch, or a new *unborn* branch — crossed with the
/// two flags that spell the second one.
///
/// git rejects `--orphan --detach` and `--detach -b` at 128 with two different
/// `cannot be used together` lines and accepts `--orphan -b`, which is the
/// combination that makes the three-way slot worth expanding rather than
/// listing: two of the three pairs are errors and the third is not, and no
/// ordering of "check for conflicts" gets all three right by accident.
///
/// 3 × 3 = 9 rows: the mode slot crossed with the branch-creation slot, with
/// the path last. `--force` is deliberately absent — it lifts the "already
/// checked out" gate, which is [`BRANCH_FORCE`]'s question, not this one.
const WORKTREE_ADD: Matrix = Matrix {
    cmd: "worktree",
    base: &["worktree", "add"],
    slots: &[
        &[&[], &["--detach"], &["--orphan"]],
        &[&[], &["-b", "nb"], &["-B", "nb"]],
    ],
    tail: &["w2"],
    omit: &[],
    shape: Shape::Worktree,
};

/// `rebase` over a worktree with unstaged edits: the flag that says what to do
/// about the dirt, crossed with the two ways of naming what to replay.
///
/// `--autostash` is the only thing standing between the refusal
/// (`cannot rebase: You have unstaged changes.`) and a rebase that stashes,
/// replays and re-applies — three writes to the worktree that a case comparing
/// only stdout cannot see. `--onto` changes which commits are replayed, so the
/// same autostash decision has a different amount of work to survive.
///
/// 3 × 2 = 6 rows: the autostash slot (default, on, explicitly off) crossed
/// with the two forms of the argument.
const REBASE_DIRTY: Matrix = Matrix {
    cmd: "rebase",
    base: &["rebase"],
    slots: &[
        &[&[], &["--autostash"], &["--no-autostash"]],
        &[&["div-cold"], &["--onto", "div-cold", "main"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::MergeableDirty,
};

/// The five ways to speak to a rebase that is not running. All five are
/// refusals, all five exit 128, and `--edit-todo` is the one whose message a
/// port is most likely to get from a different code path than the other four.
///
/// 5 rows, one per verb. Nothing to cross: these take no other option, which is
/// exactly why they are their own matrix rather than a slot in
/// [`REBASE_DIRTY`], where every row would carry a dead second flag.
const REBASE_STATE: Matrix = Matrix {
    cmd: "rebase",
    base: &["rebase"],
    slots: &[&[
        &["--abort"],
        &["--continue"],
        &["--skip"],
        &["--quit"],
        &["--edit-todo"],
    ]],
    tail: &[],
    omit: &[],
    shape: Shape::MergeableDirty,
};

/// `cherry-pick`'s three independent decisions: whether to commit the result,
/// whether a pick that could fast-forward may, and whether the message records
/// where the patch came from.
///
/// `-n --ff` is the pair git refuses (`--no-commit cannot be used with --ff`),
/// and it is only reachable by crossing two flags that are individually
/// harmless. `-x` is crossed in because it decides the *message* of the commit
/// `-n` says not to make — the one combination where two flags that look
/// orthogonal are not.
///
/// 2 × 2 × 2 = 8 rows over [`Shape::Cherry`], where the pick's patch is already
/// present on the other branch, so the successful rows have a real three-way
/// merge to do rather than a trivial apply.
const CHERRY_PICK: Matrix = Matrix {
    cmd: "cherry-pick",
    base: &["cherry-pick"],
    slots: &[&[&[], &["-n"]], &[&[], &["--ff"]], &[&[], &["-x"]]],
    tail: &["main"],
    omit: &[],
    shape: Shape::Cherry,
};

/// `revert`'s mainline selection, which is only meaningful for a merge: without
/// `-m` git refuses a merge at 128 (`is a merge but no -m option was given`),
/// with `-m 3` it refuses because the parent does not exist, and the same
/// flags against a non-merge commit take a third path.
///
/// 4 × 2 = 8 rows: the mainline slot (absent, first parent, second parent,
/// out of range) crossed with a merge and a non-merge target. Both halves are
/// needed — `-m` is an error on one and a requirement on the other, and a port
/// that ignores it entirely passes whichever half is tested alone.
const REVERT_MAINLINE: Matrix = Matrix {
    cmd: "revert",
    base: &["revert"],
    slots: &[
        &[&[], &["-m", "1"], &["-m", "2"], &["-m", "3"]],
        &[&["HEAD"], &["HEAD^"]],
    ],
    tail: &[],
    omit: &[],
    shape: Shape::Merged,
};

/// The sequencer's four state verbs, plus a fresh pick, against a conflicted
/// *merge* — a state the sequencer did not create.
///
/// Each answer is different and none of them is obvious: `--quit` succeeds and
/// clears the merge state, `--abort` and `--continue` fail at 128 with
/// `no cherry-pick or revert in progress`, and naming a commit fails with the
/// unmerged-files refusal. A port that keys all of these off "is there a
/// sequencer directory" gets `--quit`'s effect on `MERGE_HEAD` wrong, which
/// leaves the repository claiming a merge that no longer has an index.
///
/// 5 rows. The same five through `revert` are [`REVERT_GATE`]: they are two
/// matrices rather than one slot because the scoring bucket is the subcommand,
/// and a `revert` failure filed under `cherry-pick` is a failure a reader looks
/// for in the wrong place.
const CHERRY_PICK_GATE: Matrix = Matrix {
    cmd: "cherry-pick",
    base: &["cherry-pick"],
    slots: &[&[
        &["--quit"],
        &["--abort"],
        &["--continue"],
        &["--skip"],
        &["theirs"],
    ]],
    tail: &[],
    omit: &[],
    shape: Shape::Conflicted,
};

/// [`CHERRY_PICK_GATE`]'s five verbs through `revert`, which shares the
/// sequencer and not the messages: `revert --skip` says `no revert in progress`
/// while `revert --continue` says `no cherry-pick or revert in progress`, from
/// the same state. 5 rows.
const REVERT_GATE: Matrix = Matrix {
    cmd: "revert",
    base: &["revert"],
    slots: &[&[
        &["--quit"],
        &["--abort"],
        &["--continue"],
        &["--skip"],
        &["HEAD"],
    ]],
    tail: &[],
    omit: &[],
    shape: Shape::Conflicted,
};


// ---------------------------------------------------------------------------
// Deepening the four families this module started with, at the pairs the
// original matrices left uncrossed.
// ---------------------------------------------------------------------------

/// `stash push` narrowed to a path that is *ignored*, which is where `-u` and
/// `-a` stop being the same question. git's `-u` leaves an ignored file alone
/// and reports `No local changes to save`; `-a` stashes it; neither treats the
/// pathspec as unmatched, because an ignored file is still a file the walk
/// found. A port that matches the pathspec against tracked entries only
/// refuses all three with `did not match any file(s) known to git`.
///
/// 3 × 2 = 6 rows: the untracked slot crossed with `--keep-index`, which
/// decides whether the index half of the push happens at all. Narrower than
/// [`STASH_PUSH_PATHS`] on purpose — that one expands a *tracked* path, and
/// this one exists for the pathspec that only the untracked flags can reach.
const STASH_PATHSPEC_IGNORED: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "push"],
    slots: &[&[&[], &["-u"], &["-a"]], &[&[], &["-k"]]],
    tail: &["--", "ignored.txt"],
    omit: &[],
    shape: Shape::Stashed,
};

/// Reading the untracked half of an entry that has one. `stash@{1}` was pushed
/// with `-u`, so it carries a second tree that only these three flags render:
/// `-u`/`--include-untracked` show it beside the tracked diff and
/// `--only-untracked` shows it alone.
///
/// 3 rows, one per spelling, with the entry named. No empty alternative in the
/// slot: a row without one of these flags is already in [`STASH_READ`], and a
/// duplicate id would be counted twice in the denominator.
const STASH_SHOW_UNTRACKED: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "show"],
    slots: &[&[&["-u"], &["--include-untracked"], &["--only-untracked"]]],
    tail: &["stash@{1}"],
    omit: &[],
    shape: Shape::Stashed,
};

/// `merge --squash` against the flag that contradicts it. git refuses
/// `--squash --commit` at 128 and *accepts* `--squash --no-commit`, from one
/// slot of what looks like a single tri-state: whether the merge is committed.
/// A port that models it as one enum has no way to tell the two apart.
///
/// 2 × 3 = 6 rows, 5 emitted. [`MERGE_TRACKING`] holds `--squash` and
/// `--no-commit` in *different* slots so it does produce that pair, but it has
/// no `--commit` alternative at all — the refusing half of the tri-state is
/// what this matrix adds.
const MERGE_SQUASH: Matrix = Matrix {
    cmd: "merge",
    base: &["merge"],
    slots: &[
        &[&[], &["--squash"]],
        &[&[], &["--commit"], &["--no-commit"]],
    ],
    tail: &["origin/main"],
    // The bare merge of the tracking ref is `MERGE_TRACKING`'s first row.
    omit: &[&["merge", "origin/main"]],
    shape: Shape::BehindRemote,
};

/// `pull.rebase`'s value space, which is not a boolean: `merges` and
/// `interactive` select different rebase backends and `invalid` is rejected by
/// the option parser at 129 — a third exit code from the same flag.
///
/// 5 rows, one per value, over a dirty worktree so every legal value reaches
/// the same `cannot pull with rebase: You have unstaged changes.` refusal and
/// only the illegal one exits differently. [`PULL_MAIN`] spells `--rebase` and
/// `--no-rebase` only, so no value below is a duplicate of a row it produces.
const PULL_REBASE_VALUES: Matrix = Matrix {
    cmd: "pull",
    base: &["pull"],
    slots: &[&[
        &["--rebase=false"],
        &["--rebase=true"],
        &["--rebase=merges"],
        &["--rebase=interactive"],
        &["--rebase=invalid"],
    ]],
    tail: &[],
    omit: &[],
    shape: Shape::BehindRemote,
};

/// The shallow-history flags against a repository that is not shallow, where
/// two of the five are refusals with different reasons: `--unshallow` alone
/// dies with `--unshallow on a complete repository does not make sense` and
/// `--depth --unshallow` dies as an option conflict before that check runs.
/// The ordering is the measurement — the second message is only reachable if
/// the option check comes first.
///
/// 5 rows, 3 emitted, one per flag against the fixture's own remote. The row
/// with no flag at all is [`FETCH_REMOTE`]'s and bare `--unshallow` is curated,
/// so this matrix contributes the depth flags and the option conflict.
const FETCH_DEPTH: Matrix = Matrix {
    cmd: "fetch",
    base: &["fetch"],
    slots: &[&[
        &[],
        &["--depth", "1"],
        &["--deepen", "1"],
        &["--unshallow"],
        &["--depth", "1", "--unshallow"],
    ]],
    tail: &["origin"],
    // `fetch --unshallow origin` is curated in the fetch corpus, and bare
    // `fetch origin` is `FETCH_REMOTE`'s second row — this matrix contributes
    // only the depth flags.
    omit: &[&["fetch", "--unshallow", "origin"], &["fetch", "origin"]],
    shape: Shape::BehindRemote,
};

const MATRICES: &[&Matrix] = &[
    &COMMIT_SOURCE,
    &COMMIT_SOURCE_MERGE,
    &COMMIT_AUTHOR,
    &COMMIT_SCOPE,
    &SWITCH_CREATE,
    &SWITCH_MERGE_GATE,
    &CHECKOUT_MERGE_GATE,
    &RESTORE_TARGET,
    &CHECKOUT_SPARSE,
    &RESET_MERGE,
    &RESET_KEEP,
    &BRANCH_FORCE,
    &BRANCH_WORKTREE,
    &WORKTREE_ADD,
    &REBASE_DIRTY,
    &REBASE_STATE,
    &CHERRY_PICK,
    &REVERT_MAINLINE,
    &CHERRY_PICK_GATE,
    &REVERT_GATE,
    &STASH_PUSH,
    &STASH_PUSH_PATHS,
    &STASH_RESTORE,
    &STASH_READ,
    &STASH_DROP,
    &PULL_MAIN,
    &PULL_DIV,
    &MERGE_TRACKING,
    &FETCH_REMOTE,
    &STASH_PATHSPEC_IGNORED,
    &STASH_SHOW_UNTRACKED,
    &MERGE_SQUASH,
    &PULL_REBASE_VALUES,
    &FETCH_DEPTH,
];

pub fn cases(out: &mut Vec<Case>) {
    for m in MATRICES {
        m.expand(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The product is the product: one case per combination, and every case keeps
    /// its tail last. A slot list that silently dropped an alternative would
    /// under-measure exactly the interaction this module exists to cover.
    #[test]
    fn expansion_is_the_full_product_with_the_tail_last() {
        let mut cases = Vec::new();
        STASH_PUSH_PATHS.expand(&mut cases);
        assert_eq!(cases.len(), 2 * 2 * 2, "three two-way slots");
        for c in &cases {
            assert_eq!(&c.args[..2], &["stash".to_string(), "push".to_string()]);
            assert_eq!(
                &c.args[c.args.len() - 2..],
                &["--".to_string(), "counter.txt".to_string()],
                "the pathspec has to stay last: {:?}",
                c.args
            );
            assert!(c.compare_stderr, "a refusal's message is its behaviour");
        }
        // The all-empty combination is the bare base plus the tail.
        assert!(cases.iter().any(|c| c.args.len() == 4));
    }

    /// The rows of one matrix, before `omit` is applied. The tests below need
    /// the raw product: one asks how big it is, the other asks whether an
    /// omitted row is in it.
    fn product(m: &Matrix) -> Vec<Vec<&'static str>> {
        let mut argvs: Vec<Vec<&'static str>> = vec![m.base.to_vec()];
        for slot in m.slots {
            let mut next = Vec::new();
            for argv in &argvs {
                for alt in *slot {
                    let mut one = argv.clone();
                    one.extend_from_slice(alt);
                    next.push(one);
                }
            }
            argvs = next;
        }
        for argv in &mut argvs {
            argv.extend_from_slice(m.tail);
        }
        argvs
    }

    /// No matrix is a cartesian product of a flag list.
    ///
    /// The cap is the size of the largest matrix here rather than a round
    /// number, so the test states a fact about this file: 36 rows is
    /// [`STASH_PUSH`], four slots that are four real decisions about what a
    /// stash moves. A slot list that grew past it would be one where somebody
    /// crossed flags because they could, and the cost of that is paid in
    /// invocations git rejects at parse time before any behaviour runs.
    #[test]
    fn no_matrix_explodes() {
        for m in MATRICES {
            let rows = product(m).len();
            assert!(rows <= 36, "{:?} expands to {rows} rows, past the cap of 36", m.base);
            // And what it emits is that product less exactly the omitted rows:
            // an `omit` entry that matched two rows, or a slot alternative that
            // repeated another, would quietly shrink the matrix below the size
            // its doc comment claims.
            let mut emitted = Vec::new();
            m.expand(&mut emitted);
            assert_eq!(
                emitted.len(),
                rows - m.omit.len(),
                "{:?}: {rows} rows less {} omitted is not what it emitted",
                m.base,
                m.omit.len()
            );
        }
    }

    /// Every omitted row is one the matrix would otherwise have produced.
    ///
    /// `omit` exists to keep a matrix from restating a curated case, and it can
    /// only do that if it names rows that exist. An entry with a typo in it
    /// filters nothing and reads as though a collision had been handled — the
    /// duplicate id it was supposed to prevent would be back in the
    /// denominator, and nothing would say so.
    #[test]
    fn omitted_rows_are_rows_the_matrix_produces() {
        let mut total = 0;
        for m in MATRICES {
            let rows = product(m);
            for skip in m.omit {
                assert!(
                    rows.iter().any(|row| row.as_slice() == *skip),
                    "{:?} omits {skip:?}, which it never produces",
                    m.base
                );
                total += 1;
            }
        }
        assert!(total > 0, "the omit lists cannot all be empty while the corpus overlaps");
    }

    /// Each family this module covers contributes cases, under the subcommand a
    /// reader would look for them under.
    ///
    /// `scripts/split_failures.pl` files a failure by the `<shape>::<cmd>::`
    /// head of its id, so a matrix that scored its rows under the wrong bucket
    /// would hide them from the per-command brief rather than from the total.
    #[test]
    fn every_covered_subcommand_is_reachable() {
        let mut all = Vec::new();
        super::cases(&mut all);
        for cmd in [
            "commit", "switch", "checkout", "restore", "reset", "branch", "worktree", "rebase",
            "cherry-pick", "revert", "stash", "pull", "merge", "fetch",
        ] {
            assert!(
                all.iter().any(|c| c.cmd == cmd),
                "no nested case is scored under {cmd}"
            );
        }
    }

    /// Every matrix contributes, and no two cases share an id — a duplicate would
    /// be counted twice in the denominator.
    #[test]
    fn ids_are_unique_across_every_matrix() {
        let mut all = Vec::new();
        super::cases(&mut all);
        let mut ids: Vec<String> = all.iter().map(Case::id).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate case ids");
        assert!(
            total > 100,
            "the matrices should expand to a real number of cases, got {total}"
        );
    }
}
