//! Deterministic fixture repositories, built once with *stock* git and then
//! copied per case.
//!
//! Stock git is the builder on purpose: the fixture is the shared premise of a
//! differential run, so it must not depend on the implementation under test.
//! Each shape isolates a class of repository state that porcelain has to read
//! correctly — history, refs, index/worktree divergence, conflicts, and the
//! encoding edge cases that break naive path handling.
//!
//! A case is one argv against a pristine copy, with no pre-step: whatever the
//! command needs on disk has to be in the shape already. That is what the
//! second group of shapes is for. `Attributes`, `Renamed`, `Whitespace`,
//! `Packed`, `Patches` and `Sparse` each carry an input class the first eight
//! could not express — configured rules, a rename, a whitespace-only change, a
//! pack with deltas, a patch file, a sparse worktree — and each one moves the
//! parity number *down* on arrival, because it reaches code the corpus was
//! previously unable to run.

use crate::env;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A repository shape. Every corpus case names the shape it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Shape {
    /// Single commit, clean worktree. The floor case.
    Linear,
    /// Several commits, two branches, a lightweight and an annotated tag.
    Branched,
    /// A real merge commit with two parents.
    Merged,
    /// Staged, unstaged, and untracked changes coexisting.
    Dirty,
    /// Mid-conflict merge: index has stage 1/2/3 entries.
    Conflicted,
    /// Detached HEAD, as `git submodule update` leaves things.
    Detached,
    /// Unicode, spaces, and quote-worthy bytes in tracked paths.
    AwkwardPaths,
    /// A parent repo with one real submodule — zvcs's stated target topology.
    Submodule,
    /// `.gitattributes`, `.gitignore` and `.mailmap` carrying rules that match
    /// tracked paths, plus commits by the identities the mailmap rewrites.
    ///
    /// Without this, `check-attr`, `check-ignore` and `check-mailmap` are only
    /// ever asked about a repository that configures nothing, so their scores
    /// measure argument parsing and never rule matching.
    Attributes,
    /// A pure rename, a rename-with-edit at a known similarity, a copy, and a
    /// rewrite — one per commit.
    ///
    /// `-M`/`-C`/`-B`/`--find-renames` are pinned for output shape across the
    /// diff family but no shape contained a rename, so detection quality itself
    /// was unmeasured.
    Renamed,
    /// Commits whose only change is whitespace: indentation, trailing blanks,
    /// and line endings — plus one commit mixing a real edit with whitespace
    /// churn, and an unstaged whitespace-only edit in the worktree.
    ///
    /// `-w`/`-b`/`--ignore-*` had nothing to ignore in any other shape.
    Whitespace,
    /// Delta-bearing packs, loose duplicates of packed objects, and pack files
    /// tracked in the worktree at stable paths.
    ///
    /// Makes `verify-pack`, `index-pack` and `prune-packed` reachable on their
    /// real paths rather than only on their error paths, and gives clone/repack
    /// work a pack that actually contains deltas.
    Packed,
    /// Patch, mailbox and quilt-series files on disk: valid, corrupt,
    /// context-only, whitespace-damaging, and binary.
    ///
    /// A case is one argv against a pristine copy and cannot create a file
    /// first, so `apply`, `am` and `quiltimport` had no input to consume.
    Patches,
    /// Cone-mode sparse checkout with one directory excluded from the worktree.
    Sparse,
    /// Branches that `main` can merge — some by fast-forward, some three-way —
    /// over a worktree carrying unstaged edits and an untracked file.
    ///
    /// `Dirty` has dirt but nothing to merge and `Branched` has a branch but a
    /// clean tree, so which paths a merge may write over was unmeasured: a
    /// blanket "is anything dirty" refusal scored the same as git's per-path
    /// one. The dirt is placed deliberately — one edit on a path the branches
    /// rewrite, one on a path none of them touch, and an untracked file exactly
    /// where two of them want to write.
    MergeableDirty,
    /// [`Shape::MergeableDirty`]'s history with a *staged* change instead, on a
    /// path no branch touches.
    ///
    /// The index-vs-`HEAD` gate is the half a fast-forward skips: git refuses a
    /// three-way merge over this and fast-forwards over it happily, and nothing
    /// else in the corpus separates the two.
    MergeableStaged,
    /// Stash entries that already exist, over a worktree that has more to stash.
    ///
    /// Three entries with different insides — an unstaged-only one, one carrying
    /// an untracked file, and one carrying both staged and unstaged work — plus a
    /// current worktree holding a staged change, an unstaged change, an untracked
    /// file and an ignored one. Without pre-existing entries, `stash list/show/
    /// pop/apply/drop/branch` could only ever be measured on their empty-stack
    /// error path, and the flags that decide *what* gets stashed
    /// (`-u`/`-a`/`-k`/`-S`/`--`) had nothing to sort.
    Stashed,
    /// A tracking branch behind a real remote, over a dirty worktree.
    ///
    /// The remote is a bare repository *inside* the fixture (`.remote.git`, hidden
    /// from status through `info/exclude`) reached by a relative URL, so it
    /// survives the per-case copy and every case gets its own. `main` is three
    /// commits behind `origin/main` and fast-forwardable; `div` has moved on both
    /// sides. The worktree keeps an unstaged edit to a file the remote never
    /// touches (so a fast-forward must still succeed) and one to a file `div`
    /// rewrites (so that merge must refuse per path) — the two halves of the
    /// dirty-pull question that shipped broken twice.
    BehindRemote,
    /// A second, *linked* worktree of the same repository: `wt/` beside the main
    /// worktree, with its administrative directory at `.git/worktrees/wt`.
    ///
    /// The one repository layout in which `--git-dir` and `--git-common-dir`
    /// answer differently, and the only one where `HEAD` is read from somewhere
    /// other than the common directory — `wt` is on its own branch, so a
    /// discovery path that resolves the common `HEAD` reports the wrong branch
    /// while every other shape hides the mistake. No existing shape can express
    /// it: a linked worktree has to be created by `worktree add`, which a case
    /// (one argv against a pristine copy) cannot do, and adding one to an
    /// existing shape would change what every case already using that shape
    /// sees.
    ///
    /// Two construction details are load-bearing and are explained where they
    /// are done in [`build`]: the worktree is registered with relative paths so
    /// the per-case copy points at itself rather than at the template, and its
    /// index is rewritten by `read-tree` so the shape hashes the same at two
    /// build locations.
    Worktree,
    /// A merge with four parents, with one branch left unmerged beside it.
    ///
    /// `--graph` draws a merge past the second parent with rows no two-parent
    /// merge produces — the commit row's `*---.` reach, the `|\ \ \` post-merge
    /// row, and the expansion rows that open space around a merge that is not the
    /// rightmost column. [`Shape::Merged`] is the only shape carrying a merge at
    /// all and it has exactly two parents, so every one of those rows was
    /// unmeasured by the corpus.
    ///
    /// No existing shape can express it: an octopus needs a commit with three or
    /// more parents and no shape has one (`fixture.rs` runs `merge` twice in the
    /// whole file, both two-way), and a case is one argv against a pristine copy
    /// so it cannot create the merge itself. Adding the merge to `Merged` would
    /// change what every case already on that shape sees.
    ///
    /// `oct-side` forks before the merge and is never merged, so `--all` keeps a
    /// lane to the octopus's right and the expansion rows are reached; without it
    /// the merge is the last column and git skips them.
    Octopus,
    /// Two directory trees to diff *against each other*, the two degenerate
    /// halves of that comparison, and a repository config carrying a
    /// non-default `core.abbrev`. Everything lives under `ni/`.
    ///
    /// `diff --no-index` reads both sides off the filesystem and never opens an
    /// object store, so nothing a repository *contains* can stand in for its
    /// input — and a case is one argv against a pristine copy, so it cannot
    /// write the trees first.
    ///
    /// Three queue shapes, because the blob ids `--raw` prints are a function of
    /// which one it is. `diffcore_rename()` returns before its hashing pass
    /// unless the queue holds both a source and a destination
    /// (diffcore-rename.c:1461-1462), and in no-index mode that pass is the only
    /// thing that ever fills an id in. So `da`/`db` — a modified file, a
    /// left-only file and a right-only file — print real ids for the delete and
    /// the add, while `addonly_a`/`addonly_b` and `delonly_a`/`delonly_b` are
    /// add-only and delete-only and legitimately keep zeros. A fixture carrying
    /// only the first of the three would score a port that hashes
    /// unconditionally as correct.
    ///
    /// `core.abbrev = 10` sits in the repository config so the width those ids
    /// are printed at is a *configured* value rather than the built-in default.
    /// Every other shape is small enough that git's `auto` width and the
    /// hard-coded 7 coincide, which is exactly what let an implementation that
    /// ignored the setting pass.
    ///
    /// The two empty directories are empty on purpose: an add-only queue needs a
    /// side with nothing in it, and git cannot track an empty directory, so they
    /// survive only because the per-case copy is a directory walk rather than a
    /// checkout.
    NoIndexTrees,
    /// A tracked path whose on-disk name is *decomposed*: `e` followed by
    /// U+0301 COMBINING ACUTE ACCENT, not the single code point `é`.
    ///
    /// macOS hands decomposed names out of `readdir()` and, through shell
    /// completion, into `argv`; git composes both back before anything compares
    /// them (`compat/precompose_utf8.c`, gated on `core.precomposeunicode`). No
    /// other shape carries a combining mark at all — [`Shape::AwkwardPaths`]
    /// writes `üñïçødé.txt` in the composed form — so neither the conversion nor
    /// its absence had a fixture.
    ///
    /// Portable by construction rather than by luck. The conversion is
    /// `#ifdef PRECOMPOSE_UNICODE` in git (`git-compat-util.h:167-179` supplies
    /// pass-through inlines otherwise, and `config.mak.uname:156` defines the
    /// macro inside the `ifeq ($(uname_S),Darwin)` block alone) and
    /// `cfg(target_os = "macos")` in the
    /// port (`extensions/src/precompose.rs:46,202`). On Linux both sides leave
    /// the bytes alone and agree on the decomposed answer, exactly as they agree
    /// on the composed one on macOS.
    ///
    /// One file is tracked and edited in the worktree and one is untracked, so
    /// `status` has to name the path from both directions — through the index
    /// and through the directory walk.
    DecomposedPaths,
    /// A repository that has hooks installed, and a subdirectory to run from.
    ///
    /// No shape carried a hook, so every case ran against a repository where
    /// `.git/hooks` was empty — and the mere *existence* of a hook is enough to
    /// change what several verbs do. It hid a total failure: committing from a
    /// subdirectory of a repository with any hook at all exited 1 with
    /// `No such file or directory`, because the hook's path and working
    /// directory were resolved relative to a cwd that had already moved. A
    /// `pre-commit` that does nothing but `exit 0` reproduces it, so this shape
    /// needs no clever hook to be worth its build cost — it needs only to have
    /// one, and a directory to be inside.
    Hooked,
    /// Three root commits in one repository: `main`'s, and two orphan branches
    /// that share no ancestor with it or with each other.
    ///
    /// Every other shape descends from the one `initial` commit
    /// (`edfab1b71619a22120a8da1a3d85d68e0200290a` in all of them), so a pair of
    /// revisions with **no** merge base could not be named at all. That made a
    /// whole family unreachable: `merge-base` returning exit 1 with no output,
    /// `merge`/`pull` refusing with `refusing to merge unrelated histories` and
    /// then being told `--allow-unrelated-histories`, `rev-list --not` over
    /// disjoint graphs, `format-patch` across roots, and
    /// `rev-list --max-parents=0` finding more than one root.
    ///
    /// The two orphans differ in what they collide with, because the allowed
    /// merge has two outcomes and one shape has to reach both: `alien` shares no
    /// path with `main`, so `merge --allow-unrelated-histories alien` is clean;
    /// `alien-clash` carries its own `README.md`, so the same merge is an
    /// add/add conflict on a path that has no common ancestor to diff against.
    Unrelated,
    /// A criss-cross: two branches that each merged the other, so their two
    /// merge bases are incomparable.
    ///
    /// No other shape has one, which left three things unmeasurable.
    /// `merge-base --all` could never return more than one id, so an
    /// implementation that stops at the first base scored the same as one that
    /// enumerates them; `merge-base --independent` had nothing to prune; and the
    /// recursive strategy's virtual-merge-base path — merging the bases with
    /// each other to build the base it then merges against, the most intricate
    /// path in `merge-ort` — was never entered.
    ///
    /// `clash.txt` is what forces that path to *show*: the two bases disagree on
    /// it (`a` against `b`), so the virtual base is itself a conflicted merge and
    /// stage 1 of the outer conflict holds a blob that exists in no commit —
    /// stock writes conflict markers there. A port that picks one of the two
    /// bases instead leaves `a`, `b` or `base` in stage 1 and is caught by the
    /// state probe even though its stdout matches. `cc.txt` merges cleanly
    /// through the same virtual base, and `calm.txt` is untouched by anything, so
    /// one merge exercises all three outcomes.
    CrissCross,
    /// The same patch on both sides of a fork, applied by `cherry-pick` rather
    /// than shared through history.
    ///
    /// No fixture carried one commit's patch id twice, so `cherry`'s `-` marker,
    /// `rev-list --cherry-mark`'s `=` class and `--cherry-pick`'s omission were
    /// all unreachable — every case could only ever produce `+`, `<` and `>` —
    /// and `rebase`'s `skipped previously applied commit` path was never taken.
    ///
    /// `topic` forks at `cherry: seed`, commits once so the cherry-pick lands on
    /// a different parent (without that the copy is byte-identical to the
    /// original and the two branches share the commit rather than duplicating
    /// its patch), picks `main`'s `cherry: shared patch`, then commits again. So
    /// each side holds one commit the other does not, plus one whose patch id
    /// both have.
    Cherry,
    /// Symlink entries and zero-byte blobs, tracked, untracked, and inside a
    /// patch.
    ///
    /// No shape wrote either. Mode `120000` never appeared in `ls-files
    /// --stage`, so `checkout`, `archive` and `apply` were only ever asked about
    /// regular files, and `cat-file --follow-symlinks` had nothing to follow —
    /// its four answers (a resolved blob, `dangling`, a `symlink` that leaves the
    /// tree, and resolution *through* a symlinked directory) are one shape's
    /// worth of fixture apart from unreachable. The empty blob
    /// (`e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`) is a constant of the hash
    /// function that no fixture had ever stored, so `--batch-check` never saw a
    /// zero-length object and `apply` never created one.
    ///
    /// Every symlink target is relative. An absolute one would bake the build
    /// directory into a tracked blob and the shape would stop being reproducible
    /// at a second location — `link-escape` points *out* of the worktree by
    /// relative path, which is what makes the out-of-tree answer reachable
    /// without naming a machine.
    ///
    /// `sym-pending` carries the changes `patches/symlink.patch` describes, so
    /// the patch applies to `main`'s tree: a new symlink, a new empty file, and a
    /// regular file replaced by a symlink (`T` in `--raw`). A case is one argv
    /// against a pristine copy and cannot write a patch first, which is the same
    /// reason [`Shape::Patches`] exists.
    Symlinks,
    /// A written commit-graph, with one commit deliberately left out of it.
    ///
    /// `fixture.rs` never ran `commit-graph write`, so `core.commitGraph` chose
    /// between two code paths over the same objects and could not change an
    /// answer — the setting was measurable only as an argument-parsing question.
    /// `commit-graph verify`, `--changed-paths` (the Bloom filters `log -- path`
    /// consults) and the generation-number traversal had no file to read.
    ///
    /// `cg-late` is committed *after* the write, so the graph is valid but
    /// incomplete: a traversal has to fall back to reading that commit's object
    /// and to mixing graph-supplied generation numbers with computed ones, which
    /// a graph covering every commit never asks of it. `cg-side` is merged and
    /// `cg-loose` is not, so the graph holds a merge and a fork.
    ///
    /// The file is byte-identical between two builds: it is a function of the
    /// object ids and the write options alone.
    CommitGraph,
    /// A repository with four things wrong with it, none of which stops git
    /// from operating.
    ///
    /// `fsck`, `gc`, `prune` and `rev-parse --verify` ran only on healthy
    /// repositories, so "what does the port do when the repository is damaged"
    /// had no fixture at all — and the answers are not uniform, which is what
    /// makes them worth pinning: `rev-parse --verify` *succeeds* on a ref
    /// pointing at a missing object and prints the id, while `show-ref` and
    /// `for-each-ref` fail with exit 128; `rev-parse --verify` on a dangling
    /// symref warns and fails; `branch --list` prints one of the two broken refs
    /// and hides the other. A port that treats "broken" as one condition gets
    /// several of those wrong.
    ///
    /// The four:
    ///
    /// * `refs/heads/dangling` — a well-formed id no object has.
    /// * `refs/heads/broken-symref` — a symref to a branch that does not exist.
    /// * a loose object file whose contents are not a zlib stream, at a path
    ///   whose name is a valid object id.
    /// * an empty line in `.git/objects/info/alternates`.
    ///
    /// The alternates entry is empty rather than a path that does not exist, and
    /// that is a constraint rather than a preference: a missing alternate makes
    /// git print the *absolute* path of the directory it could not find on
    /// stderr, and the two sides of a case run in two different copies, so the
    /// diagnostic would differ by construction and the case would measure the
    /// harness. An empty entry exercises the same parser and stays silent.
    Damaged,
    /// Index entries added with `add -N`: recorded as paths, with no content.
    ///
    /// No shape had one, and the intent-to-add bit is not a detail — it is a
    /// third state between tracked and untracked that half a dozen commands
    /// have a dedicated branch for. `status` renders it ` A` where a real
    /// staged add is `A ` and a staged-then-edited add is `AM`; `diff` shows an
    /// ITA path as an *addition in the worktree* and `diff --cached` hides it
    /// entirely, which is what `--ita-visible-in-index` and
    /// `--ita-invisible-in-index` exist to flip; `commit` has to decide whether
    /// a path with no content is being committed; `stash push` has to decide
    /// what to do with an entry whose blob is the empty one. With no fixture
    /// carrying an ITA entry, every one of those branches was dead code the
    /// corpus could not enter, and the two `--ita-*` flags were measured as
    /// argument parsing alone.
    ///
    /// Four subjects, because the branch taken depends on what is on disk under
    /// the entry: `ita-new.txt` has content, `sub/ita-nested.txt` has content
    /// and is below the top level, `ita-gone.txt` was added and then deleted
    /// from the worktree (` D` against an entry whose blob is empty), and
    /// `both.txt` is a *real* staged add that was then edited, which is the
    /// `AM` rendering an ITA is so often confused with. `staged.txt` and
    /// `untracked.txt` are the two unambiguous neighbours.
    IntentToAdd,
    /// A rename that has not been committed yet: staged through `git mv`, and
    /// pending in the worktree through an intent-to-add.
    ///
    /// [`Shape::Renamed`] has its renames in *history* over a clean tree, so
    /// `status` had nothing to pair and `status --porcelain=v2`'s `2` record —
    /// the rename record, half the format's grammar — has never once been
    /// produced by this corpus. `--find-renames=<n>`, `--no-renames` and
    /// `status.renames` were therefore pinned on argument parsing only: with no
    /// candidate pair in the index, every threshold produced the same output.
    ///
    /// Five pairs, chosen so a threshold sweep separates them rather than
    /// moving them all at once. Measured on stock 2.55.0:
    ///
    /// * `pure.txt` -> `pure-renamed.txt` and `pkg/deep.txt` ->
    ///   `pkg/deep-renamed.txt` — content untouched, `R100`, and the second is
    ///   below the top level.
    /// * `near.txt` -> `near-renamed.txt` — staged at `R100` and then edited
    ///   again in the worktree, which is the `2 RM` record: a rename in the
    ///   index column and a modification in the worktree column at once.
    /// * `far.txt` -> `far-renamed.txt` — 12 of 40 lines rewritten, `R060`: a
    ///   rename at `-M60` and two unrelated files at `-M70`.
    /// * `wild.txt` -> `wild-renamed.txt` — 20 of 40 lines rewritten, `R039`: a
    ///   rename at `-M30` and two unrelated files at the default `-M50`.
    /// * `wt.txt` -> `wt-renamed.txt` — renamed on disk with the destination
    ///   marked intent-to-add, which is the only way git can see a rename that
    ///   is *not* staged (`2 .R`).
    ///
    /// `copy.txt` -> `copy-two.txt` keeps the source in place, so `-C` has a
    /// copy candidate that `-M` alone must not report.
    PendingRename,
    /// Notes on three refs and two `refs/replace/*` entries, present before the
    /// case runs.
    ///
    /// Every verb in this family only changes how an **existing** note or
    /// replacement is read, and a pristine fixture had neither: `log --notes=`,
    /// `--no-notes` and `notes.displayRef` selected between empty answers,
    /// `notes merge` had one side, and `--no-replace-objects` /
    /// `GIT_NO_REPLACE_OBJECTS` turned off a substitution that was never
    /// happening. A corpus agent established that the port *writes*
    /// `refs/replace/*` correctly and then never consults it when walking; that
    /// is invisible until an ordinary read verb runs over a repository that
    /// already has one.
    ///
    /// Three notes refs rather than one, because selecting between them is the
    /// behaviour: `refs/notes/commits` (the default, two commits annotated),
    /// `refs/notes/review` (a second ref, a different pair of commits), and
    /// `refs/notes/other`, which annotates the same commit as
    /// `refs/notes/commits` with different text — so `notes merge other`
    /// conflicts rather than fast-forwarding, and the `NOTES_MERGE_*` state is
    /// reachable.
    ///
    /// Two replacements, because commits and blobs take different paths through
    /// the object layer. `notes: commit 1` is replaced by a commit with the
    /// same tree and parent and a different message, so `log --oneline` prints
    /// the replacement's subject at the original's id and
    /// `--no-replace-objects` prints the original's; `README.md`'s blob is
    /// replaced by another blob, so `cat-file -p HEAD:README.md` answers
    /// differently with and without the flag while every id in the repository
    /// stays the same.
    NotesReplace,
    /// Hooks that **refuse**, and hooks of the kinds [`Shape::Hooked`] does not
    /// install.
    ///
    /// `Hooked` ships `exit 0` hooks, deliberately, because the defect it was
    /// built for reproduces with a hook that does nothing. That leaves the
    /// other half unmeasured: a hook's *non-zero exit* is a control-flow edge
    /// in every verb that runs one, and a corpus agent recorded that
    /// `--no-verify` therefore could not be measured at all — with no hook that
    /// refuses, skipping the hooks and running them are the same outcome.
    ///
    /// What each one is for, and what it makes measurable:
    ///
    /// * `pre-commit` — exits 1. `commit` aborts; `commit --no-verify` does
    ///   not. That pair *is* the measurement of `--no-verify`.
    /// * `prepare-commit-msg` — appends a paragraph to the message file.
    ///   `--no-verify` does **not** skip it (verified on stock 2.55.0: after
    ///   `commit --no-verify`, `hook-prepare-commit-msg.txt` exists and
    ///   `hook-commit-msg.txt` does not), so it is how a case sees that a
    ///   commit which bypassed the gate still went through the rewrite.
    /// * `commit-msg` — appends a second paragraph. Skipped by `--no-verify`
    ///   and reached through `merge --no-ff`, which runs it and not
    ///   `pre-commit`.
    /// * `pre-merge-commit`, `post-merge`, `post-commit`, `post-checkout`,
    ///   `post-rewrite` — each writes a file naming the arguments it was given,
    ///   so which hooks ran, in which order, and with what, survives into the
    ///   worktree where the state probe reads it. The `post-*` hooks' exit
    ///   status is ignored by git, which is itself worth pinning.
    /// * `pre-push` — exits 1, over a real peer, so `push` refuses and
    ///   `push --no-verify` does not.
    /// * `pre-rebase` and `pre-auto-gc` — exit 1, the two remaining refusals
    ///   that no other shape can produce.
    /// * `.remote.git/hooks/update` — refuses `refs/heads/veto` and accepts
    ///   everything else. A hook running in the **receiving** repository is a
    ///   kind no shape has had, and it is the one refusal `--no-verify` cannot
    ///   bypass, because it does not run on this side at all.
    ///
    /// No hook invokes git, for the reason [`Shape::Hooked`] gives: each side
    /// of a case runs its own binary, and a hook naming one by path would make
    /// the other side execute it too.
    HooksFail,
    /// A recorded rerere resolution, replayed, over a merge that is still in
    /// progress.
    ///
    /// A case is one argv against a pristine copy, so it cannot conflict,
    /// resolve, and then ask about the resolution — which left every `rerere`
    /// path that needs a *prior* record unreachable. `rerere diff`, `rerere
    /// status` and `rerere remaining` all read `.git/MERGE_RR` and the cache
    /// under `.git/rr-cache`, and both are absent from every other shape;
    /// `rerere forget`, `rerere clear` and `rerere gc` had nothing to act on;
    /// and the replay itself — git recognising a conflict it has seen and
    /// writing the old resolution back into the worktree — had never run.
    ///
    /// The shape is left mid-merge with all three outcomes present at once,
    /// which is what makes one `status` separate them:
    ///
    /// * `rr.txt` and `other.txt` conflicted, were resolved, and conflict
    ///   identically again — so git resolved them from the cache
    ///   (`Resolved 'rr.txt' using previous resolution.`) and the worktree
    ///   holds the *recorded* text, not conflict markers, while the index still
    ///   has stages 1/2/3.
    /// * `fresh.txt` conflicts for the first time, so only a preimage was
    ///   recorded and the markers are still there. It is what `rerere
    ///   remaining` and `rerere status` name, and what `rerere diff` diffs.
    ///
    /// `rerere.enabled` is in the repository config rather than passed per
    /// case: the record was made at build time and the replay has to happen
    /// without a case having to ask for it.
    Rerere,
    /// Three linked worktrees: one locked with a reason, one open, and one
    /// whose directory is gone.
    ///
    /// [`Shape::Worktree`] has a single, ordinary linked worktree, so the whole
    /// lock protocol was unreachable — and a case is one argv against a
    /// pristine copy, so it cannot lock one first. `worktree unlock` could only
    /// ever be measured on "not locked", `worktree lock` on "not locked yet",
    /// `worktree remove` on the path where nothing objects, and `worktree
    /// list --porcelain`'s `locked` and `prunable` lines had never been
    /// printed.
    ///
    /// * `wt` — locked, with the reason `held by the fixture`, so
    ///   `.git/worktrees/wt/locked` has content rather than being empty (git
    ///   writes an empty file for `lock` without `--reason`, and the two are
    ///   different answers to `worktree list --porcelain`). `worktree remove
    ///   wt` must refuse.
    /// * `wt-open` — not locked, so `unlock` must refuse it and `remove` must
    ///   accept it.
    /// * `wt-gone` — registered, with its directory deleted, which is the
    ///   `prunable gitdir file points to non-existent location` state and the
    ///   only thing `worktree prune` has ever had to prune.
    ///
    /// Registered with `--relative-paths` and re-`read-tree`d for the two
    /// reasons [`Shape::Worktree`] documents: absolute registrations would make
    /// both copies of the fixture point at the template, and a checkout's stat
    /// data would make the shape hash differently at two build locations.
    WorktreeLocked,
    /// A tag pointing at a tag pointing at a tag, plus tags on a blob and on a
    /// tree.
    ///
    /// Every tag in the corpus points straight at a commit, so peeling was a
    /// one-step operation everywhere it was measured and an implementation that
    /// peels once scored the same as one that peels to the end. `rev-parse
    /// <tag>^{}`, `show-ref -d`'s `^{}` line, `for-each-ref`'s `%(*objecttype)`,
    /// `describe`'s peel and `tag -d` over a chain all had a one-deep case and
    /// nothing else — and `cat-file -t` on a tag object whose target is *not* a
    /// commit had no object at all.
    ///
    /// `inner` -> commit, `outer` -> `inner`, `outermost` -> `outer`, so the
    /// peel is three deep; `light-to-tag` is a lightweight ref at the same tag
    /// object, so the same peel is reached through a ref that is not itself a
    /// tag object. `blobtag` and `treetag` annotate a blob and a tree, which is
    /// where an implementation that assumes a tag's target is a commit stops
    /// agreeing.
    TagChain,
    /// A shallow clone: `.git/shallow` grafted two commits below the tip, with
    /// the rest of the history reachable only from a peer inside the fixture.
    ///
    /// `fetch --unshallow`, `fetch --depth`, `fetch --deepen`, `clone
    /// --shallow-since`, `rev-parse --is-shallow-repository`, `log`'s grafted
    /// boundary and `fsck`'s tolerance of missing parents had no repository to
    /// be true of: no shape carried a `shallow` file, and a case cannot create
    /// one because a case is one argv.
    ///
    /// **Built without the network, and that is a constraint rather than a
    /// convenience.** The peer is a bare repository at `.remote.git` inside the
    /// fixture — the same place [`Shape::BehindRemote`] keeps its own, so the
    /// per-case copy carries it and `probe_peer` already reads it — and the
    /// clone runs with `--no-local` (git refuses `--depth` over a plain local
    /// path: `--depth is ignored in local clones; use file:// instead`) under an
    /// explicit `protocol.file.allow=always`. Nothing resolves a hostname at
    /// build time or at case time; `remote.origin.url` is rewritten to
    /// `./.remote.git` afterwards so a case that deepens the clone reaches its
    /// own copy's peer and never the template's.
    ///
    /// `--no-single-branch` keeps the full fetch refspec, so `sh-side` is a
    /// shallow remote-tracking branch as well and `--unshallow` has more than
    /// one line of `.git/shallow` to retire.
    Shallow,
    /// A partial clone: a promisor remote, promisor packs, and blobs that are
    /// genuinely absent from the object store.
    ///
    /// `--filter=`, `rev-list --missing=`, `--exclude-promisor-objects`, `gc
    /// --exclude-promisor-objects`, `repack --filter-to` and `backfill` all
    /// describe a repository that is missing objects on purpose, and every
    /// shape in the corpus has every object it references. A missing object was
    /// therefore only ever reachable as *damage* ([`Shape::Damaged`]), which is
    /// the opposite condition: damage is an error, and a promisor absence is
    /// not.
    ///
    /// `hist.txt` is rewritten across four commits, so three of its four blobs
    /// exist only in history and stay missing after the checkout fetches the
    /// fourth. `rev-list --missing=print --objects --all` prints exactly those
    /// three with a `?` prefix on stock 2.55.0, and `status` is clean — the
    /// worktree is a normal one, which is what separates this from a
    /// `--no-checkout` clone.
    ///
    /// Built from the same local peer as [`Shape::Shallow`], for the same
    /// reason and with the same `--no-local` +
    /// `protocol.file.allow=always` handling, plus `uploadpack.allowFilter` on
    /// the peer — without it the server silently ignores the filter
    /// (`warning: filtering not recognized by server, ignoring`) and the clone
    /// comes back complete, which would leave the shape looking built and
    /// measuring nothing.
    Promisor,
    /// One name held by two ref namespaces at once, four times over, plus a
    /// name that is both a branch and a tracked path.
    ///
    /// A corpus agent compared `for-each-ref refs/heads` against `refs/tags`
    /// across every built template and found the intersection empty in all of
    /// them, which left `ref_rev_parse_rules` — the table in `refs.c` that
    /// decides *which* `refs/…/<name>` a bare `<name>` means — unmeasurable.
    /// With no name in two namespaces, every rule in the table resolves the
    /// same ref, so an implementation that consults one rule scores exactly
    /// like one that walks all six, and git's
    /// `warning: refname '<name>' is ambiguous.` had never been printed by any
    /// case in the corpus.
    ///
    /// Four names, one per adjacent pair of rules, so a table walked in the
    /// wrong order is caught where it went wrong rather than in aggregate.
    /// Measured on stock 2.55.0 over this shape:
    ///
    /// * `ambi` — a branch at `HEAD~1` and a *lightweight* tag at `HEAD`.
    ///   `rev-parse ambi` answers the tag's commit, so `refs/tags/` outranks
    ///   `refs/heads/`.
    /// * `ambi-ann` — the same pair with an *annotated* tag, so the winning
    ///   answer is a tag object rather than a commit and `cat-file -t ambi-ann`
    ///   says `tag`. Precedence and peeling are separable only with both
    ///   spellings present.
    /// * `top` — `refs/top`, `refs/heads/top` and `refs/tags/top`, three refs
    ///   for one name. `refs/<name>` is the first rule in the table and beats
    ///   the other two.
    /// * `rem/ambi` — `refs/heads/rem/ambi` and `refs/remotes/rem/ambi`, where
    ///   the branch wins. The only pair here whose answer is a *branch*, and
    ///   the one a DWIM checkout is built on.
    ///
    /// `dual` is the other kind of ambiguity and needs no second ref: a branch
    /// and a tracked file of the same name, which is
    /// `fatal: ambiguous argument 'dual': both revision and filename` — a
    /// refusal every verb taking `<rev> [--] <path>` shares, and one no shape
    /// could produce, because no shape had a path whose name was also a ref.
    ///
    /// Every diagnostic here goes to **stderr** and the resolved id to stdout,
    /// so the cases that measure the warning are strict ones.
    AmbiguousRef,
    /// Two objects whose ids share a four-character prefix — twice, once
    /// between a commit and a blob and once between two blobs.
    ///
    /// Four is git's floor for an abbreviation (`minimum_abbrev`), and the
    /// corpus is nowhere near large enough to reach it by luck: measured across
    /// the shapes, `Packed` holds 34 objects, `CrissCross` 33, `Octopus` 24 and
    /// `Branched` 13, with no two ids sharing four characters anywhere. So
    /// `core.disambiguate`, an ambiguous `rev-parse`, the
    /// `error: short object ID … is ambiguous` / `hint: The candidates are:`
    /// report, `rev-parse --disambiguate=`, and the *widening* an abbreviation
    /// does to stay unique were all unreachable — a port that abbreviates by
    /// truncating a string scored the same as one that asks the object store
    /// how many characters are needed.
    ///
    /// A collision this small is constructed, not found. Candidate blob bodies
    /// `collide <n>\n` were hashed the way git hashes a blob —
    /// `sha1("blob " + len + "\0" + body)` — for `n` from 0 upwards, and the
    /// first `n` whose id carried the four characters wanted was kept and is
    /// baked in below as a literal. Nothing probabilistic is left at build
    /// time: the same three bodies always produce the same three ids.
    ///
    /// * `edfa` — the `initial` commit every shape in this file descends from
    ///   is `edfab1b71619a22120a8da1a3d85d68e0200290a`, so that half needed no
    ///   search at all; `collide 62671` hashes to
    ///   `edfaaf1e9919bbb3ea91c4aee0ba9bde868cdbba` and is tracked as
    ///   `commit-mate.txt`. A commit and a blob at one prefix is the pair
    ///   `core.disambiguate` exists for: `=commit` and `=committish` answer the
    ///   commit, `=blob` answers the blob, and with neither set `rev-parse`
    ///   prints both candidates and exits 128.
    /// * `a366` — `collide 105` and `collide 215` hash to
    ///   `a36664d0c037c06c0ee81cfcfb3af000a19a60ed` and
    ///   `a3660f2dc25d8d30ea9d1ae52b12eed1d2cd3bd7`, tracked as `pair-a.txt`
    ///   and `pair-b.txt`. Two objects of the *same* type at one prefix is the
    ///   ambiguity no `core.disambiguate` value can resolve, which is what
    ///   separates a port that implements the setting from one that reads it as
    ///   "take the first candidate".
    ///
    /// The widening is what makes the commit half worth more than the
    /// disambiguation: on this shape `log --oneline --abbrev=4` prints the
    /// initial commit as **five** characters (`edfab`) and every other commit
    /// as four, because four characters of that one id are no longer unique.
    /// `core.abbrev` is left at its default, so ordinary output is unaffected
    /// and only a case that asks for four sees it.
    ///
    /// The colliding blobs are *tracked* rather than written loose, so `gc`,
    /// `prune` and `repack` cannot quietly take the shape's premise away.
    ///
    /// A commit-to-commit collision is deliberately absent. It would have taken
    /// a search over commit *messages* whose answer is valid for one exact tree
    /// and parent — a literal that silently stops colliding the first time
    /// anything earlier in the shape moves. The build asserts all three ids
    /// instead, so this shape fails loudly rather than quietly measuring
    /// nothing.
    PrefixCollision,
    /// The three `am` hooks, and two hooks that are present and **not
    /// executable**.
    ///
    /// [`install_hooks`] chmods every hook it writes to 0755 and both
    /// hook-bearing shapes go through it, so "a hook that is there and does not
    /// run" had no fixture — and it is a branch git takes deliberately, not an
    /// edge case: it stats the file, skips it, and says
    /// `hint: The '.git/hooks/pre-commit' hook was ignored because it's not set
    /// as executable.` through `advice.ignoredHook`. `git hook run pre-commit`
    /// answers `error: cannot find a hook named pre-commit` and exits 1.
    ///
    /// `applypatch-msg`, `pre-applypatch` and `post-applypatch` are installed
    /// by no shape at all, which left all four `am` hook spellings —
    /// `--no-verify` among them — pinned on nothing but "the flag parses and is
    /// inert".
    ///
    /// What each one is for:
    ///
    /// * `applypatch-msg` (0755) — appends `applypatch-trailer` to the message
    ///   file it is handed, and exits 1 when that file contains `REJECT`. The
    ///   trailer is how a case sees that the hook ran; the refusal is how it
    ///   sees `am` stop before applying anything.
    /// * `pre-applypatch` (0755) — exits 1 when `veto-preapply.txt` is in the
    ///   worktree. It runs *after* the patch reaches the index and before the
    ///   commit, so a mailbox that creates that path leaves a state nothing
    ///   else in the corpus produces: the change staged, no commit made, `am`
    ///   still in progress.
    /// * `post-applypatch` (0755) — records that it ran and exits **1**, which
    ///   git ignores. That a `post-*` hook's status does not reach the caller
    ///   is worth a case of its own.
    /// * `pre-commit` (0644) — would append to `hook-pre-commit.txt` and exit
    ///   1. A `commit` on this shape must succeed and must leave that file
    ///   absent.
    /// * `commit-msg` (0644) — would append `not-executable-trailer` to the
    ///   message. A `commit` must produce a message without it.
    ///
    /// Three mailboxes, because `am` is the verb under test and a case is one
    /// argv against a pristine copy: `mail/ok.mbox` (two patches that apply),
    /// `mail/reject.mbox` (the message `applypatch-msg` refuses) and
    /// `mail/preveto.mbox` (the patch that trips `pre-applypatch`). They are
    /// produced by `format-patch --no-signature` for the reason
    /// [`Shape::Patches`] gives: the signature carries the *builder's* git
    /// version into tracked content.
    ///
    /// No hook invokes git, for the reason [`Shape::Hooked`] gives.
    AmHooks,
    /// A submodule that carries a submodule, registered and not initialised, so
    /// `submodule update --init --recursive` has two levels to descend and
    /// `--init` alone has one.
    ///
    /// [`Shape::Submodule`] is one level deep, which makes `--recursive` a
    /// synonym for its own absence: with one level to visit, a port that never
    /// recurses scores exactly like one that does. Here the two spellings
    /// produce different repositories — `--init` leaves `mid/leaf` empty and
    /// `--init --recursive` fills it — so the flag is measured by what it
    /// builds rather than by whether it parses.
    ///
    /// **Reproducible, and not exempt from [`tests::shapes_build_reproducibly`].**
    /// That is the difficulty of the shape and the reason a previous wave
    /// skipped it: `Submodule` bakes its upstream's absolute path into
    /// `.gitmodules` and `.git/config` and is exempt from that test by
    /// construction, and a nested one would record four such paths instead of
    /// two. None is recorded here:
    ///
    /// * both upstreams are **bare repositories inside the fixture** —
    ///   `.leaf.git` and `.mid.git`, hidden from status through
    ///   `.git/info/exclude`, the arrangement [`Shape::BehindRemote`] uses for
    ///   its peer — so the per-case copy carries its own and never reaches the
    ///   template's.
    /// * neither registration is made by `submodule add`. That command clones,
    ///   and a clone resolves the URL and writes the absolute answer into
    ///   `.git/config`, into the module's `remote.origin.url`, and into the
    ///   `clone: from` line of every reflog it creates. Both `.gitmodules` are
    ///   written directly and both gitlinks staged with
    ///   `update-index --cacheinfo`, so the only URLs anywhere are the relative
    ///   ones in the two tracked `.gitmodules` files: `./.mid.git` in the
    ///   parent, read from the parent's root, and `../.leaf.git` in `mid`, read
    ///   from `mid/`.
    ///
    /// **Why it is registered rather than checked out**, which is the honest
    /// limit of this shape. A populated submodule cannot be hashed by that test
    /// at all, and not because of anything a builder does: `digest` runs
    /// `status` over the shape, `status` recurses into every populated
    /// submodule to decide whether it is dirty, and that refresh rewrites the
    /// submodule's own index with fresh `stat` data. Measured directly on a
    /// populated build of this shape — `for-each-ref` and `ls-files` leave
    /// `.git/modules/mid/index` byte-identical, and `status --porcelain=v1
    /// --untracked-files=all` changes it. Inode and ctime cannot agree between
    /// two build locations, so the digest of any shape with a checked-out
    /// submodule differs from itself; `read-tree` does not help, because the
    /// probe runs after it. That is a second, independent reason
    /// [`Shape::Submodule`] must stay exempt, beside the absolute paths its doc
    /// names.
    ///
    /// An empty `mid/` directory is left in place because that is what a
    /// non-recursive clone leaves: with the directory missing, `status` calls
    /// the gitlink deleted, which is a different repository state from an
    /// unpopulated one. Git cannot track an empty directory, so it survives
    /// only because the per-case copy is a directory walk — the same reason
    /// [`Shape::NoIndexTrees`]'s empty sides survive.
    ///
    /// `leaf` is recorded at `leaf: two` while its history holds `leaf: one`
    /// before it, so a recursive update that stopped at the first level, or
    /// checked out the wrong commit, is visible in one line of
    /// `submodule status --recursive`.
    NestedSubmodule,
    /// A split index: the entries parked in `.git/sharedindex.<sha>`, with
    /// `.git/index` holding little more than the link to it.
    ///
    /// No shape had one, so `core.splitIndex`, `update-index --split-index`,
    /// `--no-split-index`, `splitIndex.maxPercentChange` and the `link`
    /// extension were argument-parsing questions — and
    /// [`crate::runner`]'s index probe already knows how to report a shared
    /// index that no fixture ever produced.
    ///
    /// The previous wave skipped this for a stated reason: `sharedindex.<sha>`
    /// is named for a hash over the index it holds, that index stores per-entry
    /// `stat` data, and two builds therefore disagree on the file's *name* as
    /// well as on its bytes — which `shapes_build_reproducibly` fails on, since
    /// it exempts `.git/index` alone. Confirmed directly: two builds of the
    /// same repository three seconds apart produced `sharedindex.a2f349d0…`
    /// and `sharedindex.1834a108…`.
    ///
    /// The reason is also the fix, and it needs no exemption. `read-tree`
    /// rewrites the entries from the tree with the `stat` fields **zeroed** —
    /// the normalisation [`Shape::Worktree`] applies to a linked worktree's
    /// index, and a state git writes itself — so the shared index that splits
    /// out of it is a function of the tree alone. Under that ordering the same
    /// two builds produce `sharedindex.11c3c770b7d0f1d25e1f9d17209f24c247ffc268`
    /// byte for byte.
    ///
    /// A second commit follows the split, so `.git/index` holds entries of its
    /// own beside the link and the shape is not the degenerate "everything is
    /// shared" case.
    ///
    /// The untracked cache is **not** here, and this is the shape it would have
    /// belonged to. It cannot be built on the machine that builds these
    /// fixtures at all: `update-index --untracked-cache` answers
    /// `warning: untracked cache is disabled on this system or location` and
    /// records nothing, because git probes the filesystem's mtime behaviour
    /// first and refuses where it does not trust it. A shape carrying the flag
    /// and not the cache would look built and measure nothing.
    SplitIndex,
}

impl Shape {
    pub const ALL: &'static [Shape] = &[
        Shape::Linear,
        Shape::Branched,
        Shape::Merged,
        Shape::Dirty,
        Shape::Conflicted,
        Shape::Detached,
        Shape::AwkwardPaths,
        Shape::Submodule,
        Shape::Attributes,
        Shape::Renamed,
        Shape::Whitespace,
        Shape::Packed,
        Shape::Patches,
        Shape::Sparse,
        Shape::MergeableDirty,
        Shape::MergeableStaged,
        Shape::Stashed,
        Shape::BehindRemote,
        Shape::Worktree,
        Shape::Octopus,
        Shape::NoIndexTrees,
        Shape::DecomposedPaths,
        Shape::Hooked,
        Shape::Unrelated,
        Shape::CrissCross,
        Shape::Cherry,
        Shape::Symlinks,
        Shape::CommitGraph,
        Shape::Damaged,
        Shape::IntentToAdd,
        Shape::PendingRename,
        Shape::NotesReplace,
        Shape::HooksFail,
        Shape::Rerere,
        Shape::WorktreeLocked,
        Shape::TagChain,
        Shape::Shallow,
        Shape::Promisor,
        Shape::AmbiguousRef,
        Shape::PrefixCollision,
        Shape::AmHooks,
        Shape::NestedSubmodule,
        Shape::SplitIndex,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Linear => "linear",
            Shape::Branched => "branched",
            Shape::Merged => "merged",
            Shape::Dirty => "dirty",
            Shape::Conflicted => "conflicted",
            Shape::Detached => "detached",
            Shape::AwkwardPaths => "awkward-paths",
            Shape::Submodule => "submodule",
            Shape::Attributes => "attributes",
            Shape::Renamed => "renamed",
            Shape::Whitespace => "whitespace",
            Shape::Packed => "packed",
            Shape::Patches => "patches",
            Shape::Sparse => "sparse",
            Shape::MergeableDirty => "mergeable-dirty",
            Shape::MergeableStaged => "mergeable-staged",
            Shape::Stashed => "stashed",
            Shape::BehindRemote => "behind-remote",
            Shape::Worktree => "worktree",
            Shape::Octopus => "octopus",
            Shape::NoIndexTrees => "no-index-trees",
            Shape::DecomposedPaths => "decomposed-paths",
            Shape::Hooked => "hooked",
            Shape::Unrelated => "unrelated",
            Shape::CrissCross => "criss-cross",
            Shape::Cherry => "cherry",
            Shape::Symlinks => "symlinks",
            Shape::CommitGraph => "commit-graph",
            Shape::Damaged => "damaged",
            Shape::IntentToAdd => "intent-to-add",
            Shape::PendingRename => "pending-rename",
            Shape::NotesReplace => "notes-replace",
            Shape::HooksFail => "hooks-fail",
            Shape::Rerere => "rerere",
            Shape::WorktreeLocked => "worktree-locked",
            Shape::TagChain => "tag-chain",
            Shape::Shallow => "shallow",
            Shape::Promisor => "promisor",
            Shape::AmbiguousRef => "ambiguous-ref",
            Shape::PrefixCollision => "prefix-collision",
            Shape::AmHooks => "am-hooks",
            Shape::NestedSubmodule => "nested-submodule",
            Shape::SplitIndex => "split-index",
        }
    }
}

/// Run stock git in `dir`, failing loudly on non-zero exit.
///
/// Fixture construction has no tolerance for partial success: a half-built
/// premise would silently weaken every case that uses it.
fn git(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = crate::stock::command()?;
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(args);
    let out = cmd
        .output()
        .with_context(|| format!("spawn stock git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "fixture: stock git {args:?} in {} failed ({})\n{}",
            dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write(dir: &Path, rel: &str, body: &str) -> Result<()> {
    write_bytes(dir, rel, body.as_bytes())
}

fn write_bytes(dir: &Path, rel: &str, body: &[u8]) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

/// `n` numbered lines. Generated rather than literal so similarity indices are
/// arithmetic rather than eyeballed: editing `k` of `n` lines is a `k/n` change.
fn numbered(prefix: &str, n: usize) -> String {
    numbered_with_edits(prefix, n, &[])
}

/// `numbered`, with the listed 1-based lines rewritten so they no longer match.
fn numbered_with_edits(prefix: &str, n: usize, edited: &[usize]) -> String {
    (1..=n)
        .map(|i| {
            if edited.contains(&i) {
                format!("{prefix} line {i} edited\n")
            } else {
                format!("{prefix} line {i}\n")
            }
        })
        .collect()
}

/// The single `*.pack` under `.git/objects/pack`, which `repack -ad` has just
/// left as the only one. Fails loudly rather than guessing: every consumer of
/// this path needs the pack to exist.
fn sole_pack(dir: &Path) -> Result<PathBuf> {
    let pack_dir = dir.join(".git").join("objects").join("pack");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&pack_dir)
        .with_context(|| format!("read {}", pack_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        n => bail!("fixture: expected exactly one pack in {}, found {n}", pack_dir.display()),
    }
}

/// Build `shape` at `dir`. `home` is the hermetic HOME for the build commands.
pub fn build(shape: Shape, dir: &Path, home: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    // `-b main` pins the initial branch so the fixture does not inherit the
    // host's `init.defaultBranch` — another way the machine leaks in.
    git(dir, home, &["init", "-q", "-b", "main"])?;

    // A case runs against a *copy* of the template, and a copy gets fresh inodes
    // and creation times no matter how carefully it is made. Git's default stat
    // comparison includes both, so every copied fixture looked stat-dirty to the
    // commands that check the index without refreshing it first — `git apply
    // --index` and, through it, `git quiltimport` — which failed with
    // `does not match index` on *both* sides and so scored as agreement while
    // measuring nothing. `minimal` drops inode and ctime from the comparison;
    // `copy_tree` carries the mtime across so the remaining fields still match.
    // This compensates for an artifact of the harness, not for anything the
    // implementation under test does.
    git(dir, home, &["config", "core.checkStat", "minimal"])?;

    write(dir, "README.md", "# fixture\n")?;
    write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\n")?;
    git(dir, home, &["add", "."])?;
    git(dir, home, &["commit", "-q", "-m", "initial"])?;

    match shape {
        Shape::Linear => {}

        Shape::Branched => {
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "add two"])?;
            git(dir, home, &["tag", "v0.1.0"])?;
            git(dir, home, &["tag", "-a", "v0.2.0", "-m", "annotated"])?;
            git(dir, home, &["branch", "feature"])?;
            git(dir, home, &["checkout", "-q", "feature"])?;
            write(dir, "feature.txt", "feature work\n")?;
            git(dir, home, &["add", "feature.txt"])?;
            git(dir, home, &["commit", "-qm", "feature commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
        }

        Shape::Merged => {
            git(dir, home, &["checkout", "-q", "-b", "side"])?;
            write(dir, "side.txt", "side\n")?;
            git(dir, home, &["add", "side.txt"])?;
            git(dir, home, &["commit", "-qm", "side commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "main.txt", "main\n")?;
            git(dir, home, &["add", "main.txt"])?;
            git(dir, home, &["commit", "-qm", "main commit"])?;
            git(dir, home, &["merge", "--no-ff", "-m", "merge side", "side"])?;
        }

        Shape::Dirty => {
            // One of each: staged, unstaged, untracked, and a deletion.
            write(dir, "staged.txt", "staged\n")?;
            git(dir, home, &["add", "staged.txt"])?;
            write(dir, "README.md", "# fixture\nmodified, not staged\n")?;
            write(dir, "untracked.txt", "untracked\n")?;
            std::fs::remove_file(dir.join("src/lib.rs"))?;
        }

        Shape::Conflicted => {
            git(dir, home, &["checkout", "-q", "-b", "theirs"])?;
            write(dir, "conflict.txt", "theirs\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "theirs"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "conflict.txt", "ours\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "ours"])?;
            // Expected to exit non-zero — that *is* the state being built.
            let mut cmd = crate::stock::command()?;
            env::harden(&mut cmd, home);
            cmd.current_dir(dir).args(["merge", "theirs"]);
            let out = cmd.output()?;
            if out.status.success() {
                bail!("fixture: conflicted merge unexpectedly succeeded");
            }
        }

        Shape::Detached => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "second"])?;
            git(dir, home, &["checkout", "-q", "--detach", "HEAD~1"])?;
        }

        Shape::AwkwardPaths => {
            write(dir, "with space.txt", "space\n")?;
            write(dir, "üñïçødé.txt", "unicode\n")?;
            write(dir, "quote\"name.txt", "quote\n")?;
            write(dir, "nested/deep/path.txt", "deep\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "awkward paths"])?;
        }

        Shape::Submodule => {
            // A real submodule needs a real upstream; build one beside the parent.
            let upstream = dir.join("..").join(format!("{}-upstream", dir.file_name().unwrap().to_string_lossy()));
            std::fs::create_dir_all(&upstream)?;
            let upstream = upstream.canonicalize()?;
            git(&upstream, home, &["init", "-q", "-b", "main"])?;
            write(&upstream, "mod.txt", "submodule content\n")?;
            git(&upstream, home, &["add", "."])?;
            git(&upstream, home, &["commit", "-qm", "submodule initial"])?;

            git(
                dir,
                home,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    upstream.to_str().context("upstream path not utf-8")?,
                    "sub",
                ],
            )?;
            git(dir, home, &["commit", "-qm", "add submodule"])?;
        }

        Shape::Attributes => {
            // Tracked paths first: every rule below matches something real, so
            // `check-attr`/`check-ignore` are asked about the matching path set
            // and not only about the "nothing configured" answer.
            write(dir, "src/tabs.rs", "fn indented() {\n\tlet x = 1;\n}\n")?;
            write(dir, "docs/manual.md", "# manual\n\nprose\n")?;
            write(dir, "vendor/generated.js", "// generated, do not edit\n")?;
            write(dir, "sub/nested.txt", "nested\n")?;
            write(dir, "logs/keep.log", "tracked even though *.log is ignored\n")?;
            write_bytes(dir, "assets/logo.bin", &binary_blob(0))?;

            write(
                dir,
                ".gitattributes",
                // Later rules win, so the `binary` macro has to follow `text=auto`.
                "* text=auto\n\
                 *.rs text eol=lf whitespace=tab-in-indent,trailing-space\n\
                 *.md diff=markdown export-ignore\n\
                 *.log -diff\n\
                 vendor/** linguist-generated -diff -merge\n\
                 assets/*.bin binary\n\
                 sub/nested.txt merge=union\n\
                 missing-attr.txt !text\n",
            )?;
            // A nested file overrides the root for its own subtree — precedence
            // is the part of attribute lookup a single file cannot exercise.
            write(dir, "sub/.gitattributes", "nested.txt -diff eol=crlf\n*.txt text\n")?;
            // `.git/info/attributes` outranks both, and lives outside the worktree.
            write(dir, ".git/info/attributes", "*.info ident\ninfo-only.txt text\n")?;

            write(
                dir,
                ".gitignore",
                "*.log\n!important.log\nbuild/\n/notes.tmp\n**/deep-ignored/\n*.o\n",
            )?;
            write(dir, "sub/.gitignore", "!*.log\nlocal-*\n")?;
            write(dir, ".git/info/exclude", "excluded-by-info.txt\n")?;

            write(
                dir,
                ".mailmap",
                "Proper Name <proper@example.invalid> <old@example.invalid>\n\
                 Proper Name <proper@example.invalid> Alias Name <alias@example.invalid>\n\
                 <canonical@example.invalid> Typo Name <typo@example.invalid>\n\
                 Solo Name <solo@example.invalid>\n",
            )?;

            git(dir, home, &["add", "-A"])?;
            // Ignored by its own rule, so it needs `-f` — and being tracked is
            // exactly what makes it interesting to `check-ignore`.
            git(dir, home, &["add", "-f", "logs/keep.log"])?;
            git(dir, home, &["commit", "-qm", "attributes: rules and subjects"])?;

            // Commits by the identities `.mailmap` rewrites. `--author` beats the
            // hardened environment for the author field; the committer stays
            // pinned, which is what keeps the ids reproducible.
            write(dir, "sub/nested.txt", "nested\nsecond line\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by old address", "--author=Old Name <old@example.invalid>"],
            )?;
            write(dir, "docs/manual.md", "# manual\n\nprose\nmore prose\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by alias", "--author=Alias Name <alias@example.invalid>"],
            )?;
            write(dir, "src/tabs.rs", "fn indented() {\n\tlet x = 2;\n}\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by typo", "--author=Typo Name <typo@example.invalid>"],
            )?;

            // Untracked subjects for the ignore rules, written after the commit
            // so `add -A` above could not swallow them.
            write(dir, "build/output.o", "ignored by two rules\n")?;
            write(dir, "logs/debug.log", "ignored\n")?;
            write(dir, "important.log", "un-ignored by the negation\n")?;
            write(dir, "notes.tmp", "ignored, anchored to the root\n")?;
            write(dir, "sub/deep-ignored/thing.txt", "ignored by the ** rule\n")?;
            write(dir, "sub/local-scratch.txt", "ignored by the nested rule\n")?;
            write(dir, "excluded-by-info.txt", "ignored by .git/info/exclude\n")?;
            write(dir, "tracked-looking.txt", "not ignored at all\n")?;
        }

        Shape::Renamed => {
            write(dir, "orig/alpha.txt", &numbered("alpha", 40))?;
            write(dir, "orig/beta.txt", &numbered("beta", 40))?;
            write(dir, "orig/gamma.txt", &numbered("gamma", 40))?;
            write(dir, "orig/delta.txt", &numbered("delta", 40))?;
            git(dir, home, &["add", "orig"])?;
            git(dir, home, &["commit", "-qm", "renames: seed"])?;

            // 100% similarity: content identical, path changed.
            std::fs::create_dir_all(dir.join("moved"))?;
            git(dir, home, &["mv", "orig/alpha.txt", "moved/alpha.txt"])?;
            git(dir, home, &["commit", "-qm", "renames: pure rename"])?;

            // 8 of 40 lines rewritten, which stock scores as `R072` — under
            // `-M90%`, over `-M50%`. The similarity index is what separates a
            // correct threshold implementation from one that ignores the
            // threshold, so it has to be a known value rather than whatever an
            // ad-hoc edit happens to produce.
            git(dir, home, &["mv", "orig/beta.txt", "moved/beta.txt"])?;
            write(
                dir,
                "moved/beta.txt",
                &numbered_with_edits("beta", 40, &[3, 7, 11, 15, 19, 23, 27, 31]),
            )?;
            git(dir, home, &["commit", "-qam", "renames: rename with edit"])?;

            // A copy whose source is modified in the same commit, which is what
            // plain `-C` (without `--find-copies-harder`) is able to see.
            write(dir, "copies/gamma.txt", &numbered("gamma", 40))?;
            write(dir, "orig/gamma.txt", &numbered_with_edits("gamma", 40, &[5, 10]))?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "renames: copy with modified source"])?;

            // Same path, entirely different content: the rewrite `-B` splits.
            write(dir, "orig/delta.txt", &numbered("rewritten", 40))?;
            git(dir, home, &["commit", "-qam", "renames: rewrite in place"])?;
        }

        Shape::Whitespace => {
            write(dir, "ws/indent.c", WS_TABS)?;
            write(dir, "ws/eol.txt", "alpha\r\nbeta\r\ngamma\r\n")?;
            git(dir, home, &["add", "ws"])?;
            git(dir, home, &["commit", "-qm", "whitespace: seed"])?;

            write(dir, "ws/indent.c", WS_SPACES)?;
            git(dir, home, &["commit", "-qam", "whitespace: tabs to spaces"])?;

            write(dir, "ws/indent.c", WS_TRAILING)?;
            git(dir, home, &["commit", "-qam", "whitespace: trailing blanks"])?;

            write(dir, "ws/eol.txt", "alpha\nbeta\ngamma\n")?;
            git(dir, home, &["commit", "-qam", "whitespace: crlf to lf"])?;

            // One real edit surrounded by whitespace churn. `-w` has to drop the
            // churn and keep the edit — and take its context from the *post*
            // image, which is the class of bug no other shape can reach.
            write(dir, "ws/indent.c", WS_MIXED)?;
            git(dir, home, &["commit", "-qam", "whitespace: one edit amid churn"])?;

            // Unstaged and whitespace-only, so bare `git diff` — the most-run
            // diff of all — has something for `-w`/`-b` to ignore.
            write(dir, "ws/indent.c", WS_REINDENTED)?;
        }

        Shape::Packed => {
            // Seven revisions of one 400-line file: successive blobs share most
            // of their content, which is what gives pack-objects deltas to find.
            for rev in 0..7usize {
                let edits: Vec<usize> = (1..=rev).map(|k| k * 37).collect();
                write(dir, "big.txt", &numbered_with_edits("payload", 400, &edits))?;
                if rev == 0 {
                    git(dir, home, &["add", "big.txt"])?;
                }
                let msg = format!("packed: revision {rev}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // `pack.threads=1` pins delta selection. The threaded search splits
            // the object window by thread count, so without this the pack bytes
            // depend on the builder's core count and the shape stops being
            // reproducible on another machine.
            git(dir, home, &["-c", "pack.threads=1", "repack", "-adq"])?;

            // A pack file name embeds the pack's own checksum, so no case can
            // name one in argv without being rewritten whenever the fixture
            // changes by a byte. Stable copies in the worktree fix that, and put
            // `index-pack`'s output where the state probe can see it.
            let pack = sole_pack(dir)?;
            std::fs::create_dir_all(dir.join("packs"))?;
            std::fs::copy(&pack, dir.join("packs/sample.pack"))?;
            std::fs::copy(pack.with_extension("idx"), dir.join("packs/sample.idx"))?;
            std::fs::copy(&pack, dir.join("packs/unindexed.pack"))?;
            git(dir, home, &["add", "packs"])?;
            git(dir, home, &["commit", "-qm", "packed: pack files at stable paths"])?;

            // A second pack, *without* `-d`, so loose copies of packed objects
            // survive and `prune-packed` has real work rather than a no-op.
            git(dir, home, &["-c", "pack.threads=1", "repack", "-q"])?;

            // An object no ref reaches, for `prune`, `fsck --unreachable` and
            // `count-objects -v`. The reflog is expired because it would
            // otherwise keep the commit reachable and hide the state.
            write(dir, "orphan.txt", "unreachable once the reset lands\n")?;
            git(dir, home, &["add", "orphan.txt"])?;
            git(dir, home, &["commit", "-qm", "packed: soon unreachable"])?;
            git(dir, home, &["reset", "-q", "--hard", "HEAD~1"])?;
            git(dir, home, &["reflog", "expire", "--expire=all", "--all"])?;
        }

        Shape::Patches => {
            write(dir, "app/main.c", MAIN_C_BASE)?;
            write_bytes(dir, "app/data.bin", &binary_blob(1))?;
            git(dir, home, &["add", "app"])?;
            git(dir, home, &["commit", "-qm", "patches: seed"])?;

            // The changes the patches carry live on a side branch, so main's
            // tree stays the pre-image every patch applies to and no object is
            // left dangling.
            git(dir, home, &["checkout", "-q", "-b", "pending"])?;
            write(dir, "app/main.c", MAIN_C_ONE)?;
            git(dir, home, &["commit", "-qam", "patches: add subtract"])?;
            write(dir, "app/main.c", MAIN_C_TWO)?;
            write_bytes(dir, "app/data.bin", &binary_blob(2))?;
            git(dir, home, &["commit", "-qam", "patches: bump version and data"])?;
            git(dir, home, &["checkout", "-q", "main"])?;

            // `--no-signature` drops git's own version number from the trailer,
            // which would otherwise put the builder's git build into tracked
            // content and make the fixture non-reproducible across machines.
            let mbox = git(
                dir,
                home,
                &["format-patch", "--no-signature", "--binary", "--stdout", "main..pending"],
            )?;
            write(dir, "mail/series.mbox", &mbox)?;
            let single = git(
                dir,
                home,
                &["format-patch", "--no-signature", "--stdout", "-1", "pending~1"],
            )?;
            write(dir, "mail/one.eml", &single)?;

            let valid = git(dir, home, &["diff", "main", "pending~1", "--", "app/main.c"])?;
            write(dir, "patches/valid.patch", &valid)?;
            let second = git(dir, home, &["diff", "pending~1", "pending", "--", "app/main.c"])?;
            let binary = git(dir, home, &["diff", "--binary", "main", "pending", "--", "app/data.bin"])?;
            write(dir, "patches/binary.patch", &binary)?;
            write(dir, "patches/corrupt.patch", CORRUPT_PATCH)?;
            write(dir, "patches/context-only.patch", CONTEXT_ONLY_PATCH)?;
            write(dir, "patches/whitespace.patch", WHITESPACE_PATCH)?;
            write(dir, "patches/offset.patch", OFFSET_PATCH)?;

            // quilt keeps its patch order in a `series` file beside the patches.
            // The `From:`/`Subject:` headers are what stop `quiltimport` from
            // stopping to ask for an author, which it does by reading stdin —
            // and a case that blocks on stdin measures nothing.
            write(dir, "quilt/series", "0001-first.patch\n0002-second.patch\n")?;
            write(
                dir,
                "quilt/0001-first.patch",
                &format!(
                    "From: {name} <{email}>\nSubject: first quilt patch\n\nAdds subtract().\n\n{valid}",
                    name = env::AUTHOR_NAME,
                    email = env::AUTHOR_EMAIL,
                ),
            )?;
            write(
                dir,
                "quilt/0002-second.patch",
                &format!(
                    "From: {name} <{email}>\nSubject: second quilt patch\n\nBumps VERSION.\n\n{second}",
                    name = env::AUTHOR_NAME,
                    email = env::AUTHOR_EMAIL,
                ),
            )?;

            git(dir, home, &["add", "mail", "patches", "quilt"])?;
            git(dir, home, &["commit", "-qm", "patches: fixtures"])?;
        }

        Shape::Sparse => {
            write(dir, "inside/keep.txt", "kept by the cone\n")?;
            write(dir, "inside/nested/also.txt", "kept, nested\n")?;
            write(dir, "outside/drop.txt", "excluded from the worktree\n")?;
            write(dir, "outside/nested/deep.txt", "excluded, nested\n")?;
            write(dir, "root.txt", "root files stay in a cone checkout\n")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "sparse: seed"])?;

            git(dir, home, &["sparse-checkout", "init", "--cone"])?;
            git(dir, home, &["sparse-checkout", "set", "inside"])?;

            // An untracked file inside the excluded cone. `status`, `clean` and
            // `add` each have to decide what a sparse-excluded path means, and
            // `rm` on the *tracked* excluded path is a fixed bug this pins.
            write(dir, "outside/stray.txt", "untracked, inside the excluded cone\n")?;
        }

        Shape::Stashed => {
            // Three commits give the entries something to sit on top of.
            write(dir, "counter.txt", "1\n")?;
            write(dir, "notes.txt", "notes\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-q", "-m", "add counter and notes"])?;

            // Entry @{2}: unstaged only.
            write(dir, "counter.txt", "1\nstashed-unstaged\n")?;
            git(dir, home, &["stash", "push", "-m", "unstaged only"])?;
            // Entry @{1}: carries an untracked file, which only `-u` picks up.
            write(dir, "extra.txt", "untracked, stashed with -u\n")?;
            git(dir, home, &["stash", "push", "-u", "-m", "with untracked"])?;
            // Entry @{0}: staged *and* unstaged work, so `--index` has something
            // to restore and `--keep-index` something to keep.
            write(dir, "notes.txt", "notes\nstaged\n")?;
            git(dir, home, &["add", "notes.txt"])?;
            write(dir, "notes.txt", "notes\nstaged\nunstaged\n")?;
            git(dir, home, &["stash", "push", "-m", "staged and unstaged"])?;

            // And a current worktree with one of each, so a fresh `stash push`
            // has all four kinds of content to decide about.
            write(dir, ".gitignore", "ignored.txt\n")?;
            git(dir, home, &["add", ".gitignore"])?;
            git(dir, home, &["commit", "-q", "-m", "ignore ignored.txt"])?;
            write(dir, "counter.txt", "1\nworktree-unstaged\n")?;
            write(dir, "notes.txt", "notes\nworktree-staged\n")?;
            git(dir, home, &["add", "notes.txt"])?;
            write(dir, "notes.txt", "notes\nworktree-staged\nthen-unstaged\n")?;
            write(dir, "fresh.txt", "untracked in the worktree\n")?;
            write(dir, "ignored.txt", "ignored in the worktree\n")?;
        }

        Shape::BehindRemote => {
            write(dir, "shared.txt", "shared, the remote rewrites this\n")?;
            write(dir, "mine.txt", "mine, the remote never touches this\n")?;
            write(dir, "clash.txt", "clash, div rewrites this\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-q", "-m", "add shared, mine and clash"])?;

            // The remote lives inside the fixture so the per-case copy carries it,
            // and is reached by a relative URL so the copy's own one is used.
            git(dir, home, &["init", "-q", "--bare", ".remote.git"])?;
            git(dir, home, &["remote", "add", "origin", "./.remote.git"])?;
            git(dir, home, &["push", "-q", "origin", "main"])?;
            git(dir, home, &["branch", "--set-upstream-to=origin/main", "main"])?;

            // Advance the remote's `main` by three commits, none of them touching
            // `mine.txt`, so a fast-forward over a dirty `mine.txt` must succeed.
            git(dir, home, &["checkout", "-q", "-b", "upstream-work"])?;
            for n in ["2", "3", "4"] {
                write(dir, "shared.txt", &format!("shared, the remote rewrites this\nupstream {n}\n"))?;
                git(dir, home, &["commit", "-qam", &format!("upstream {n}")])?;
            }
            git(dir, home, &["push", "-q", "origin", "upstream-work:main"])?;

            // `div` diverges: the remote rewrites `clash.txt`, the local side adds
            // its own commit, so a pull has to merge and then refuse on the path
            // the worktree is holding dirty.
            git(dir, home, &["checkout", "-q", "-b", "div", "main"])?;
            write(dir, "clash.txt", "clash, div rewrites this\nremote side\n")?;
            git(dir, home, &["commit", "-qam", "div on the remote"])?;
            git(dir, home, &["push", "-q", "origin", "div"])?;
            git(dir, home, &["reset", "-q", "--hard", "HEAD~1"])?;
            write(dir, "notes-div.txt", "local side of div\n")?;
            git(dir, home, &["add", "notes-div.txt"])?;
            git(dir, home, &["commit", "-q", "-m", "div locally"])?;
            git(dir, home, &["branch", "--set-upstream-to=origin/div", "div"])?;

            // Back on `main`, three commits behind, with the remote's own refs
            // fetched so `pull` has a tracking ref to compare against.
            git(dir, home, &["checkout", "-q", "main"])?;
            git(dir, home, &["branch", "-D", "upstream-work"])?;
            git(dir, home, &["fetch", "-q", "origin"])?;

            // The bare remote is not part of the tree under test.
            write(dir, ".git/info/exclude", ".remote.git/\n")?;

            // Dirty in two ways that matter: a file the remote never touches, and
            // one that `div` rewrites.
            write(dir, "mine.txt", "mine, the remote never touches this\nlocal edit\n")?;
            write(dir, "clash.txt", "clash, div rewrites this\nlocal edit\n")?;
        }

        Shape::MergeableDirty => {
            mergeable_history(dir, home)?;
            // `hot.txt` is rewritten by `ff-hot`/`div-hot`, so a merge of those
            // has to refuse; `keep.txt` is rewritten by nothing, so a merge of
            // anything has to carry it through; `squat.txt` sits untracked
            // exactly where `ff-squat`/`div-squat` want to write.
            write(dir, "hot.txt", "hot, edited in the worktree\n")?;
            write(dir, "keep.txt", "keep, edited in the worktree\n")?;
            write(dir, "squat.txt", "untracked squatter\n")?;
        }

        Shape::MergeableStaged => {
            mergeable_history(dir, home)?;
            // Staged, and on a path no branch below touches — so no checkout
            // could overwrite it. A fast-forward carries it through; a strategy
            // refuses the whole merge over it anyway.
            write(dir, "keep.txt", "keep, staged\n")?;
            git(dir, home, &["add", "keep.txt"])?;
        }

        Shape::Worktree => {
            // The linked worktree lives inside the fixture root so the per-case
            // copy carries it, and is hidden from the main worktree's status the
            // same way `BehindRemote` hides its bare remote.
            write(dir, ".git/info/exclude", "wt/\n")?;

            // `--relative-paths` is what makes the shape copyable at all. By
            // default `worktree add` records the *absolute* path of the worktree
            // in `.git/worktrees/wt/gitdir` and of the git dir in `wt/.git`, so
            // both copies of the fixture would point back at the template: the
            // two sides would share one repository and the comparison would
            // measure nothing. With relative paths each copy points at itself.
            git(dir, home, &["worktree", "add", "--relative-paths", "-q", "-b", "linked", "wt"])?;

            // `worktree add` checks the files out and records their `stat` data
            // in `.git/worktrees/wt/index`. That data is inode and mtime, so two
            // builds of the shape would differ there and the shape would stop
            // being reproducible — `shapes_build_reproducibly` exempts the main
            // `.git/index` for exactly this reason, and a second index is not
            // covered by that exemption. `read-tree` rewrites the same entries
            // from the tree with the stat fields zeroed, which is a state git
            // writes itself (any `read-tree` without `-u` does) and refreshes on
            // first use, so the worktree stays a normal checked-out one.
            git(&dir.join("wt"), home, &["read-tree", "HEAD"])?;
        }

        Shape::Octopus => {
            // Three branches off the base, each touching one path of its own so
            // the octopus merges cleanly and needs no strategy of substance.
            for branch in ["oct-a", "oct-b", "oct-c"] {
                git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
                write(dir, &format!("{branch}.txt"), &format!("{branch}\n"))?;
                git(dir, home, &["add", &format!("{branch}.txt")])?;
                git(dir, home, &["commit", "-qm", &format!("{branch} commit")])?;
            }

            // Forked before the merge and never merged, so it survives as a lane
            // beside the octopus under `--all`.
            git(dir, home, &["checkout", "-q", "-b", "oct-side", "main"])?;
            write(dir, "oct-side.txt", "oct-side\n")?;
            git(dir, home, &["add", "oct-side.txt"])?;
            git(dir, home, &["commit", "-qm", "oct-side commit"])?;

            // `main` moves on first, so the merge's first parent is not the base
            // and the graph has a lane to collapse under the octopus as well.
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "trunk.txt", "trunk\n")?;
            git(dir, home, &["add", "trunk.txt"])?;
            git(dir, home, &["commit", "-qm", "main moves on"])?;
            git(
                dir,
                home,
                &["merge", "-q", "--no-ff", "-m", "octopus merge", "oct-a", "oct-b", "oct-c"],
            )?;
        }

        Shape::NoIndexTrees => {
            // The queue with both a source and a destination: one path changed
            // on both sides, one only on the left, one only on the right.
            write(dir, "ni/da/common.txt", "common one\ncommon two\ncommon three\n")?;
            write(dir, "ni/db/common.txt", "common one\ncommon two changed\ncommon three\n")?;
            write(dir, "ni/da/left.txt", "only on the left\n")?;
            write(dir, "ni/db/right.txt", "only on the right\n")?;
            // The two degenerate queues. Their other half is an empty directory,
            // which is the only way to make a comparison that is purely an add
            // or purely a delete.
            write(dir, "ni/addonly_b/added.txt", "added on the right\n")?;
            write(dir, "ni/delonly_a/gone.txt", "gone from the right\n")?;
            std::fs::create_dir_all(dir.join("ni").join("addonly_a"))?;
            std::fs::create_dir_all(dir.join("ni").join("delonly_b"))?;
            // Two plain files, for the cases that want a single modified pair
            // and nothing else in the queue.
            write(dir, "ni/a.txt", "alpha\nbeta\ngamma\n")?;
            write(dir, "ni/b.txt", "alpha\nBETA\ngamma\n")?;

            // A pair whose only difference is whitespace: leading indentation,
            // an interior run, a trailing blank, and a blank line added. `-w`,
            // `-b`, `--ignore-blank-lines`, `--ignore-space-at-eol` and
            // `--check` all decide something here and nothing in the pairs
            // above, where every difference survives every ignore rule.
            write(dir, "ni/ws_a.txt", "one\ntwo three\nfour\n")?;
            write(dir, "ni/ws_b.txt", "  one\ntwo   three\n\nfour   \n")?;

            // A pair git calls binary. Without a NUL in reach, `--binary`,
            // `--text` and the "Binary files differ" line were unreachable on
            // the no-index path, where there is no `.gitattributes` to say so
            // instead.
            write_bytes(dir, "ni/bin_a.bin", b"\x00\x01binary one\x00\xff")?;
            write_bytes(dir, "ni/bin_b.bin", b"\x00\x01binary two\x00\xfe")?;

            // A final line with no newline on one side only, which is the whole
            // of the `\ No newline at end of file` marker — and it has to be
            // emitted for the right side.
            write(dir, "ni/eol_a.txt", "last line\n")?;
            write_bytes(dir, "ni/eol_b.txt", b"last line")?;

            // Identical content under two names, one per directory: the only
            // input where rename detection on a no-index queue has a rename to
            // find rather than a modification to leave alone.
            write(dir, "ni/ra/moved.txt", "carried across unchanged\nsecond line\n")?;
            write(dir, "ni/rb/moved-elsewhere.txt", "carried across unchanged\nsecond line\n")?;

            // Function bodies, so `--function-context` has a hunk header to
            // extend to and `-U<n>` has more than three lines to widen past.
            write(
                dir,
                "ni/fn_a.c",
                "int first(void)\n{\n\treturn 1;\n}\n\nint second(void)\n{\n\tint x = 2;\n\treturn x;\n}\n",
            )?;
            write(
                dir,
                "ni/fn_b.c",
                "int first(void)\n{\n\treturn 1;\n}\n\nint second(void)\n{\n\tint x = 3;\n\treturn x;\n}\n",
            )?;

            // Tracked, so the state probe's `status` stays quiet and the shape
            // reports only what a case did. The empty directories cannot be
            // tracked and are invisible to `status` for the same reason.
            git(dir, home, &["add", "ni"])?;
            git(dir, home, &["commit", "-qm", "no-index: subject trees"])?;

            // Read only by the cases that run *inside* this repository; the ones
            // that run outside it never see this file. Ten is deliberately not
            // 7 and not what `auto` would pick for a repository this small.
            git(dir, home, &["config", "core.abbrev", "10"])?;
        }

        Shape::DecomposedPaths => {
            write(dir, NFD_TRACKED, "decomposed\n")?;
            git(dir, home, &["add", "--", NFD_TRACKED])?;
            git(dir, home, &["commit", "-qm", "nfd: a decomposed path"])?;
            // Dirty through the index, and dirty through the directory walk:
            // `status` has to name the same decomposed path from both.
            write(dir, NFD_TRACKED, "decomposed\nworktree edit\n")?;
            write(dir, NFD_UNTRACKED, "untracked, decomposed\n")?;
        }

        Shape::Hooked => {
            // A subdirectory with tracked content, so a case carrying `cwd` runs
            // from inside the repository rather than at its root. That
            // combination — a hook present, and a working directory below the
            // top level — is the one that failed outright.
            write(dir, "sub/nested.txt", "nested\n")?;
            write(dir, "top.txt", "top\n")?;
            git(dir, home, &["add", "sub/nested.txt", "top.txt"])?;
            git(dir, home, &["commit", "-qm", "hooked: a subdirectory to run from"])?;

            // Deliberately hooks that do NOT invoke git. Each side of a case runs
            // its own binary, and a hook naming one by path would make the other
            // side execute it too — the fixture would then measure one binary
            // through the other and call the agreement parity. `exit 0` and a
            // plain write need no binary at all, and the defect this shape exists
            // for reproduces with exactly that.
            //
            // `$GIT_INDEX_FILE` is echoed rather than used: git points a
            // `pre-commit` hook at the real index, at `index.lock`, or at
            // `next-index-<pid>.lock` depending on the commit mode
            // (builtin/commit.c:468, :493, :554), and a hook that records which
            // one it was given turns that into something a case can compare.
            let hooks = dir.join(".git/hooks");
            std::fs::create_dir_all(&hooks)?;
            for (name, body) in [
                (
                    "pre-commit",
                    "#!/bin/sh\nprintf 'pre-commit %s\\n' \"${GIT_INDEX_FILE##*/}\" > hook-ran.txt\nexit 0\n",
                ),
                // Appends a trailer to every commit message, so a case can see
                // whether the hook's edit survived — and, for the verbs that pass
                // `--no-verify`, whether it was correctly skipped.
                (
                    "commit-msg",
                    "#!/bin/sh\nprintf 'hooked-trailer\\n' >> \"$1\"\nexit 0\n",
                ),
            ] {
                let path = hooks.join(name);
                std::fs::write(&path, body)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
                }
            }
        }

        Shape::Unrelated => {
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "unrelated: main moves"])?;

            // Shares no path with `main`, so the allowed merge is clean and the
            // resulting tree is the union of two roots.
            orphan(dir, home, "alien", &["README.md", "src/lib.rs"])?;
            write(dir, "alien.txt", "alien root\n")?;
            git(dir, home, &["add", "alien.txt"])?;
            git(dir, home, &["commit", "-qm", "alien root"])?;
            write(dir, "alien.txt", "alien root\nalien second\n")?;
            git(dir, home, &["commit", "-qam", "alien second"])?;
            // A tag on the far root, so `describe`, `format-patch` and the
            // `--contains` queries can name it without an id.
            git(dir, home, &["tag", "alien-tip"])?;

            // Collides with `main` on `README.md`. An add/add conflict between
            // unrelated roots has no common ancestor to diff against, which is a
            // different path through the strategy than the add/add conflicts
            // every other shape can produce.
            orphan(dir, home, "alien-clash", &["alien.txt"])?;
            write(dir, "README.md", "# alien fixture\n\nsame path, no common ancestor\n")?;
            git(dir, home, &["add", "README.md"])?;
            git(dir, home, &["commit", "-qm", "alien clash root"])?;

            git(dir, home, &["checkout", "-q", "main"])?;
        }

        Shape::CrissCross => {
            write(dir, "cc.txt", &numbered("cc", 12))?;
            write(dir, "clash.txt", "base\n")?;
            write(dir, "calm.txt", "calm\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "criss-cross: base"])?;

            // The two future merge bases. They disagree on `clash.txt`, which is
            // what makes the virtual base a conflicted merge rather than a
            // second copy of `base`.
            git(dir, home, &["checkout", "-q", "-b", "cc-a", "main"])?;
            write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[2]))?;
            write(dir, "clash.txt", "a\n")?;
            git(dir, home, &["commit", "-qam", "criss-cross: a"])?;

            git(dir, home, &["checkout", "-q", "-b", "cc-b", "main"])?;
            write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[11]))?;
            write(dir, "clash.txt", "b\n")?;
            git(dir, home, &["commit", "-qam", "criss-cross: b"])?;

            // Each side merges the other and resolves `clash.txt` its own way,
            // so the two tips disagree there again and the outer merge has to
            // reach for a base.
            for (tip, from, other, keep, edit) in [
                ("cc-left", "cc-a", "cc-b", "a\n", 5usize),
                ("cc-right", "cc-b", "cc-a", "b\n", 8usize),
            ] {
                git(dir, home, &["checkout", "-q", "-b", tip, from])?;
                git_conflicting(dir, home, &["merge", "--no-commit", other])?;
                write(dir, "clash.txt", keep)?;
                git(dir, home, &["add", "clash.txt"])?;
                let msg = format!("criss-cross: {tip} merge");
                git(dir, home, &["commit", "-qm", &msg])?;
                // One more commit per side on a path that merges cleanly, so the
                // criss-cross merge is not *only* a conflict.
                write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[2, edit, 11]))?;
                let msg = format!("criss-cross: {tip} tip");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // HEAD stays on `cc-left`: a case is one argv and cannot check out
            // first, so the branch the criss-cross is merged *from* has to be the
            // one already checked out.
            git(dir, home, &["checkout", "-q", "cc-left"])?;
        }

        Shape::Cherry => {
            write(dir, "app.txt", &numbered("app", 10))?;
            git(dir, home, &["add", "app.txt"])?;
            git(dir, home, &["commit", "-qm", "cherry: seed"])?;
            git(dir, home, &["branch", "topic"])?;

            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3]))?;
            git(dir, home, &["commit", "-qam", "cherry: shared patch"])?;
            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3, 8]))?;
            git(dir, home, &["commit", "-qam", "cherry: upstream only"])?;

            git(dir, home, &["checkout", "-q", "topic"])?;
            // Committed *before* the pick on purpose. Every input to a commit id
            // is pinned here except the parent, so picking onto the fork point
            // would reproduce the original commit byte for byte and the two
            // branches would share it instead of holding two commits with one
            // patch id.
            write(dir, "topic-base.txt", "topic\n")?;
            git(dir, home, &["add", "topic-base.txt"])?;
            git(dir, home, &["commit", "-qm", "cherry: topic base"])?;
            git(dir, home, &["cherry-pick", "main~1"])?;
            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3, 10]))?;
            git(dir, home, &["commit", "-qam", "cherry: topic only"])?;
        }

        Shape::Symlinks => {
            write(dir, "empty.txt", "")?;
            write(dir, "dir/empty-nested.txt", "")?;
            write(dir, "dir/target.txt", "target content\n")?;
            // One per answer `cat-file --follow-symlinks` has: a blob, a blob
            // reached through a symlinked directory, `dangling`, `symlink` for a
            // target outside the tree, and a link to a link.
            symlink(dir, "link-to-file", "README.md")?;
            symlink(dir, "link-to-dir", "dir")?;
            symlink(dir, "link-broken", "no/such/target")?;
            symlink(dir, "link-escape", "../outside.txt")?;
            symlink(dir, "link-to-link", "link-to-file")?;
            symlink(dir, "dir/link-up", "../dir/target.txt")?;
            // Retargeted in the worktree below, so one symlink is dirty and the
            // rest stay clean.
            symlink(dir, "link-wt", "README.md")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "symlinks: seed"])?;

            // The patch's subject changes live on a side branch so `main`'s tree
            // stays the pre-image the patch applies to, as in [`Shape::Patches`].
            git(dir, home, &["checkout", "-q", "-b", "sym-pending"])?;
            symlink(dir, "later-link", "empty.txt")?;
            write(dir, "later-empty.txt", "")?;
            std::fs::remove_file(dir.join("dir/target.txt"))?;
            symlink(dir, "dir/target.txt", "../empty.txt")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "symlinks: pending changes"])?;
            git(dir, home, &["checkout", "-q", "main"])?;

            let patch = git(dir, home, &["diff", "main", "sym-pending"])?;
            write(dir, "patches/symlink.patch", &patch)?;
            git(dir, home, &["add", "patches"])?;
            git(dir, home, &["commit", "-qm", "symlinks: a patch that adds one"])?;

            // Dirty through the index and dirty through the directory walk: a
            // retargeted symlink and an untracked one, plus an untracked empty
            // file for the `add`/`status`/`clean` side of the question.
            std::fs::remove_file(dir.join("link-wt"))?;
            symlink(dir, "link-wt", "src/lib.rs")?;
            symlink(dir, "stray-link", "README.md")?;
            write(dir, "stray-empty.txt", "")?;
        }

        Shape::CommitGraph => {
            for n in 1..=5usize {
                write(dir, "cg.txt", &numbered("cg", n))?;
                if n == 1 {
                    git(dir, home, &["add", "cg.txt"])?;
                }
                let msg = format!("commit-graph: {n}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // A fork that is never merged and a branch that is, so the graph
            // holds both a merge commit and a tip off the main chain.
            git(dir, home, &["checkout", "-q", "-b", "cg-loose", "main~3"])?;
            write(dir, "cg-loose.txt", "loose\n")?;
            git(dir, home, &["add", "cg-loose.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: loose fork"])?;

            git(dir, home, &["checkout", "-q", "-b", "cg-side", "main"])?;
            write(dir, "cg-side.txt", "side\n")?;
            git(dir, home, &["add", "cg-side.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: side"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            git(dir, home, &["merge", "-q", "--no-ff", "-m", "commit-graph: merge side", "cg-side"])?;

            // `--changed-paths` writes the Bloom filters `log -- <path>` reads;
            // without them the flag is a write option with no reader.
            git(dir, home, &["commit-graph", "write", "--reachable", "--changed-paths"])?;

            // After the write, so the graph is valid and incomplete.
            write(dir, "cg-late.txt", "committed after the graph was written\n")?;
            git(dir, home, &["add", "cg-late.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: after the write"])?;
        }

        Shape::Damaged => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "damaged: a second commit"])?;

            // Written as files rather than through `update-ref`, which refuses
            // both: git will not create a ref to an object it cannot find, and
            // will not point a symref at a branch that does not exist. The
            // damage this shape exists for is precisely the state git's own
            // plumbing declines to produce.
            write(dir, ".git/refs/heads/dangling", &format!("{MISSING_OBJECT}\n"))?;
            write(dir, ".git/refs/heads/broken-symref", "ref: refs/heads/does-not-exist\n")?;

            // A loose object whose name is a valid id and whose contents are not
            // a zlib stream. `cat-file --batch-all-objects` still lists it — as
            // `missing` — so the object *set* stays enumerable and the damage is
            // in the read.
            write(
                dir,
                &format!(".git/objects/{}/{}", &CORRUPT_OBJECT[..2], &CORRUPT_OBJECT[2..]),
                "this is not a zlib stream\n",
            )?;

            // An empty entry. See the shape's doc comment for why it is empty
            // rather than a path that does not exist.
            write(dir, ".git/objects/info/alternates", "\n")?;
        }

        Shape::IntentToAdd => {
            // A committed neighbour, so the shape has a path that is plainly
            // tracked next to the ones that are only half-tracked.
            write(dir, "tracked.txt", "tracked\n")?;
            git(dir, home, &["add", "tracked.txt"])?;
            git(dir, home, &["commit", "-qm", "intent-to-add: a tracked file"])?;

            // The intent-to-add entries. `add -N` records the path with the
            // *empty* blob, so the index and the worktree disagree for every one
            // of these by construction — which is what makes ` A` a state and not
            // a rendering detail.
            write(dir, "ita-new.txt", "ita with content\nsecond line\n")?;
            write(dir, "sub/ita-nested.txt", "nested ita\n")?;
            write(dir, "ita-gone.txt", "ita then deleted\n")?;
            git(dir, home, &["add", "-N", "ita-new.txt", "sub/ita-nested.txt", "ita-gone.txt"])?;
            // Deleted after the entry was made: an ITA path with nothing under
            // it, which stock renders ` D` against a blob that is empty rather
            // than ` A`.
            std::fs::remove_file(dir.join("ita-gone.txt"))?;

            // The contrast case. A real staged add, then edited — `AM`, which is
            // the rendering an intent-to-add is most often confused with.
            write(dir, "both.txt", "staged then modified\n")?;
            git(dir, home, &["add", "both.txt"])?;
            write(dir, "both.txt", "staged then modified\nmore\n")?;

            write(dir, "staged.txt", "plain staged\n")?;
            git(dir, home, &["add", "staged.txt"])?;
            write(dir, "untracked.txt", "untracked plain\n")?;
        }

        Shape::PendingRename => {
            for name in ["pure", "near", "far", "wild", "wt", "copy"] {
                write(dir, &format!("{name}.txt"), &numbered(name, 40))?;
            }
            write(dir, "pkg/deep.txt", &numbered("deep", 40))?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "pending-rename: seed"])?;

            // Content untouched: `R100`, at the top level and below it.
            git(dir, home, &["mv", "pure.txt", "pure-renamed.txt"])?;
            git(dir, home, &["mv", "pkg/deep.txt", "pkg/deep-renamed.txt"])?;

            // Staged at `R100`, then edited again in the worktree. The index
            // column says rename and the worktree column says modified at the
            // same time, which is the `2 RM` record.
            git(dir, home, &["mv", "near.txt", "near-renamed.txt"])?;
            write(dir, "near-renamed.txt", &numbered_with_edits("near", 40, &[7]))?;

            // The two thresholds. `numbered_with_edits` keeps the line count
            // fixed and rewrites `k` of them, so the similarity index is a
            // function of `k` alone: 12 of 40 measures `R060` on stock 2.55.0
            // and 20 of 40 measures `R039`.
            let far_edits: Vec<usize> = (1..=12).collect();
            let wild_edits: Vec<usize> = (1..=20).collect();
            git(dir, home, &["mv", "far.txt", "far-renamed.txt"])?;
            write(dir, "far-renamed.txt", &numbered_with_edits("far", 40, &far_edits))?;
            git(dir, home, &["add", "far-renamed.txt"])?;
            git(dir, home, &["mv", "wild.txt", "wild-renamed.txt"])?;
            write(dir, "wild-renamed.txt", &numbered_with_edits("wild", 40, &wild_edits))?;
            git(dir, home, &["add", "wild-renamed.txt"])?;

            // The worktree column's own rename. Git pairs a worktree deletion
            // with a worktree addition only when the addition is in the index,
            // so an intent-to-add entry is the only way to express a rename that
            // has not been staged.
            std::fs::rename(dir.join("wt.txt"), dir.join("wt-renamed.txt"))?;
            git(dir, home, &["add", "-N", "wt-renamed.txt"])?;

            // Source left in place: a copy, which `-C` may report and `-M` alone
            // must not.
            std::fs::copy(dir.join("copy.txt"), dir.join("copy-two.txt"))?;
            git(dir, home, &["add", "copy-two.txt"])?;
        }

        Shape::NotesReplace => {
            for n in 1..=3 {
                write(dir, &format!("note{n}.txt"), &format!("note subject {n}\n"))?;
                git(dir, home, &["add", &format!("note{n}.txt")])?;
                git(dir, home, &["commit", "-qm", &format!("notes: commit {n}")])?;
            }

            git(dir, home, &["notes", "add", "-m", "default note on HEAD", "HEAD"])?;
            git(dir, home, &["notes", "add", "-m", "default note on HEAD~1", "HEAD~1"])?;
            git(dir, home, &["notes", "--ref=review", "add", "-m", "review note on HEAD", "HEAD"])?;
            git(dir, home, &["notes", "--ref=review", "add", "-m", "review note on HEAD~2", "HEAD~2"])?;
            // Annotates the same commit as `refs/notes/commits` with different
            // text, so `notes merge other` conflicts instead of fast-forwarding.
            git(
                dir,
                home,
                &["notes", "--ref=other", "add", "-m", "other note on HEAD, conflicting", "HEAD"],
            )?;

            // A commit replaced by one with the same tree and the same parent
            // and a different message: every id in the repository is unchanged,
            // so the only thing that can differ is whether the walk consults
            // `refs/replace/*`.
            let target = rev(dir, home, "HEAD~2")?;
            let tree = rev(dir, home, "HEAD~2^{tree}")?;
            let parent = rev(dir, home, "HEAD~3")?;
            let replacement = git(
                dir,
                home,
                &["commit-tree", "-p", &parent, "-m", "notes: replacement for commit 1", &tree],
            )?
            .trim()
            .to_string();
            git(dir, home, &["replace", &target, &replacement])?;

            // And a blob, which reaches the substitution through a different
            // door: `cat-file -p HEAD:README.md` answers with the replacement
            // and `--no-replace-objects` with the original.
            //
            // Hashed from a file rather than from stdin because [`git`] does not
            // feed one; the file is removed again so the shape's worktree is not
            // changed by the way its objects were made.
            const SCRATCH: &str = ".replacement-readme";
            write(dir, SCRATCH, "# replaced readme\n")?;
            let new_blob = git(dir, home, &["hash-object", "-w", SCRATCH])?.trim().to_string();
            std::fs::remove_file(dir.join(SCRATCH))?;
            let old_blob = rev(dir, home, "HEAD:README.md")?;
            git(dir, home, &["replace", &old_blob, &new_blob])?;
        }

        Shape::HooksFail => {
            write(dir, "side-base.txt", "base\n")?;
            git(dir, home, &["add", "side-base.txt"])?;
            git(dir, home, &["commit", "-qm", "hooks-fail: base"])?;

            git(dir, home, &["checkout", "-q", "-b", "hf-side"])?;
            write(dir, "hf-side.txt", "side\n")?;
            git(dir, home, &["add", "hf-side.txt"])?;
            git(dir, home, &["commit", "-qm", "hooks-fail: side commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            // So `merge hf-side` is a three-way rather than a fast-forward, and
            // therefore runs `pre-merge-commit` and `commit-msg`.
            write(dir, "hf-main.txt", "main\n")?;
            git(dir, home, &["add", "hf-main.txt"])?;
            git(dir, home, &["commit", "-qm", "hooks-fail: main commit"])?;

            // The peer `pre-push` has to refuse a transport to. Same place, same
            // relative URL and same `info/exclude` as [`Shape::BehindRemote`].
            git(dir, home, &["init", "-q", "--bare", "-b", "main", PEER])?;
            git(dir, home, &["remote", "add", "origin", PEER_URL])?;
            git(dir, home, &["push", "-q", "origin", "main", "hf-side"])?;
            git(dir, home, &["branch", "--set-upstream-to=origin/main", "main"])?;
            // One commit past the remote, so `push` has something to send and
            // the refusal is not a no-op.
            write(dir, "hf-ahead.txt", "ahead of the remote\n")?;
            git(dir, home, &["add", "hf-ahead.txt"])?;
            git(dir, home, &["commit", "-qm", "hooks-fail: ahead of origin"])?;
            // The branch the *receiving* repository's `update` hook rejects by
            // name. It exists only here, so a case can push a ref the local
            // hooks are happy with and the remote's is not.
            git(dir, home, &["branch", "veto"])?;
            write(dir, ".git/info/exclude", ".remote.git/\n")?;

            // Dirty, so `commit -a` has something to be refused over.
            write(dir, "side-base.txt", "base\nedited in the worktree\n")?;

            install_hooks(&dir.join(".git/hooks"), FAILING_HOOKS)?;
            install_hooks(&dir.join(PEER).join("hooks"), PEER_HOOKS)?;
        }

        Shape::Rerere => {
            // In the repository config rather than per case: the record below is
            // made at build time, and the replay has to happen without a case
            // having to ask for it.
            git(dir, home, &["config", "rerere.enabled", "true"])?;

            write(dir, "rr.txt", "base one\nbase two\nbase three\n")?;
            write(dir, "other.txt", "other one\nother two\nother three\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "rerere: base"])?;

            git(dir, home, &["checkout", "-q", "-b", "rr-side"])?;
            write(dir, "rr.txt", "side one\nbase two\nside three\n")?;
            write(dir, "other.txt", "side other\nother two\nother three\n")?;
            git(dir, home, &["commit", "-qam", "rerere: side"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "rr.txt", "main one\nbase two\nmain three\n")?;
            write(dir, "other.txt", "main other\nother two\nother three\n")?;
            git(dir, home, &["commit", "-qam", "rerere: main"])?;

            // The recording pass: conflict, resolve by hand, commit. Stock
            // reports `Recorded preimage` on the conflict and `Recorded
            // resolution` on the commit, and leaves preimage/postimage pairs
            // under `.git/rr-cache`.
            git_conflicting(dir, home, &["merge", "rr-side"])?;
            write(dir, "rr.txt", "resolved one\nbase two\nresolved three\n")?;
            write(dir, "other.txt", "resolved other\nother two\nother three\n")?;
            git(dir, home, &["add", "rr.txt", "other.txt"])?;
            git(dir, home, &["commit", "-qm", "rerere: resolved merge"])?;

            // Undo the merge and give both sides one more commit, so the same
            // two conflicts recur *byte for byte* — which is the condition
            // rerere keys on — beside a third that has never been seen.
            git(dir, home, &["reset", "-q", "--hard", "HEAD~1"])?;
            git(dir, home, &["checkout", "-q", "rr-side"])?;
            write(dir, "fresh.txt", "side fresh\ncommon\n")?;
            git(dir, home, &["add", "fresh.txt"])?;
            git(dir, home, &["commit", "-qm", "rerere: side fresh"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "fresh.txt", "main fresh\ncommon\n")?;
            git(dir, home, &["add", "fresh.txt"])?;
            git(dir, home, &["commit", "-qm", "rerere: main fresh"])?;

            // Left mid-merge on purpose: `rerere diff`, `rerere status` and
            // `rerere remaining` all read `.git/MERGE_RR`, which exists only
            // while a merge is unresolved.
            git_conflicting(dir, home, &["merge", "rr-side"])?;
        }

        Shape::WorktreeLocked => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "worktree-locked: a second commit"])?;

            write(dir, ".git/info/exclude", "wt/\nwt-open/\nwt-gone/\n")?;
            for (name, branch) in [("wt", "wt-held"), ("wt-open", "wt-open"), ("wt-gone", "wt-gone")] {
                git(dir, home, &["worktree", "add", "--relative-paths", "-q", "-b", branch, name])?;
                // Same reason as [`Shape::Worktree`]: `worktree add` records the
                // checkout's inode and mtime in the linked worktree's own index,
                // and that index is not exempt from the determinism check.
                git(&dir.join(name), home, &["read-tree", "HEAD"])?;
            }

            git(dir, home, &["worktree", "lock", "--reason", "held by the fixture", "wt"])?;
            // Registered and gone: the `prunable` state, and the only thing
            // `worktree prune` has ever had to prune.
            std::fs::remove_dir_all(dir.join("wt-gone"))?;
        }

        Shape::TagChain => {
            write(dir, "a.txt", "one\n")?;
            git(dir, home, &["add", "a.txt"])?;
            git(dir, home, &["commit", "-qm", "tags: one"])?;

            // `advice.nestedTag` is silenced on the command line rather than in
            // the repository config: the hint is the point of the shape, and a
            // persisted setting would show up in the `config --list --local`
            // probe as a fact about the fixture rather than about the case.
            git(dir, home, &["tag", "-a", "inner", "-m", "inner annotated tag"])?;
            for (name, target) in [("outer", "inner"), ("outermost", "outer")] {
                let msg = format!("{name} tag, points at {target}");
                let quiet = "advice.nestedTag=false";
                git(dir, home, &["-c", quiet, "tag", "-a", name, "-m", &msg, target])?;
            }
            // A lightweight ref at the same tag object: the same three-deep peel
            // reached through a ref that is not itself a tag object.
            git(dir, home, &["tag", "light-to-tag", "inner"])?;

            write(dir, "b.txt", "two\n")?;
            git(dir, home, &["add", "b.txt"])?;
            git(dir, home, &["commit", "-qm", "tags: two"])?;

            // Tags whose target is not a commit at all.
            let blob = rev(dir, home, "HEAD:a.txt")?;
            let tree = rev(dir, home, "HEAD^{tree}")?;
            git(dir, home, &["tag", "-a", "blobtag", "-m", "tag on a blob", &blob])?;
            git(dir, home, &["tag", "-a", "treetag", "-m", "tag on a tree", &tree])?;

            // Two commits past `inner`, so `describe` has a distance to render
            // and has to peel to find it.
            write(dir, "c.txt", "three\n")?;
            git(dir, home, &["add", "c.txt"])?;
            git(dir, home, &["commit", "-qm", "tags: three"])?;
        }

        Shape::Shallow => {
            for n in 1..=5usize {
                write(dir, "deep.txt", &format!("deep {n}\n"))?;
                if n == 1 {
                    git(dir, home, &["add", "deep.txt"])?;
                }
                let msg = format!("shallow: deep {n}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }
            // Forks below the graft, so `--unshallow` has a second line of
            // `.git/shallow` to retire and not just a deeper first parent.
            git(dir, home, &["branch", "sh-side", "main~3"])?;
            git(dir, home, &["init", "-q", "--bare", "-b", "main", PEER])?;
            git(dir, home, &["remote", "add", "origin", PEER_URL])?;
            git(dir, home, &["push", "-q", "origin", "main", "sh-side"])?;

            restage_as_clone(dir, home, &["--no-single-branch", "--depth=2"])?;
        }

        Shape::Promisor => {
            write(dir, "hist.txt", "hist v0\n")?;
            git(dir, home, &["add", "hist.txt"])?;
            git(dir, home, &["commit", "-qm", "partial: hist v0"])?;
            for n in 1..=3usize {
                write(dir, "hist.txt", &format!("hist v{n}\n"))?;
                let msg = format!("partial: hist v{n}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }
            git(dir, home, &["branch", "pc-side", "main~2"])?;

            git(dir, home, &["init", "-q", "--bare", "-b", "main", PEER])?;
            // Without this the server ignores the filter and answers with a
            // complete pack (`warning: filtering not recognized by server,
            // ignoring`), which would leave the shape looking built and
            // measuring nothing.
            git(&dir.join(PEER), home, &["config", "uploadpack.allowFilter", "true"])?;
            git(dir, home, &["remote", "add", "origin", PEER_URL])?;
            git(dir, home, &["push", "-q", "origin", "main", "pc-side"])?;

            restage_as_clone(dir, home, &["--no-single-branch", "--filter=blob:none"])?;
        }

        Shape::AmbiguousRef => {
            write(dir, "a1.txt", "a1\n")?;
            git(dir, home, &["add", "a1.txt"])?;
            git(dir, home, &["commit", "-qm", "ambiguous: second"])?;
            // A tracked path whose name is also a branch. Committed last so the
            // path exists at the tip, which is where a case looks for it.
            write(dir, "dual", "a path whose name is also a branch\n")?;
            git(dir, home, &["add", "dual"])?;
            git(dir, home, &["commit", "-qm", "ambiguous: third"])?;

            // Three distinct commits, so which rule won is visible in the answer
            // rather than having to be inferred. `root` is the `initial` commit
            // every shape descends from.
            let root = rev(dir, home, "HEAD~2")?;
            let mid = rev(dir, home, "HEAD~1")?;
            let tip = rev(dir, home, "HEAD")?;

            git(dir, home, &["branch", "ambi", &mid])?;
            git(dir, home, &["tag", "ambi", &tip])?;
            git(dir, home, &["branch", "ambi-ann", &mid])?;
            git(dir, home, &["tag", "-a", "ambi-ann", "-m", "annotated, and also a branch", &tip])?;
            // `refs/<name>` is the first of the six rules, and the only one no
            // porcelain writes — hence `update-ref` rather than `branch`/`tag`.
            git(dir, home, &["update-ref", "refs/top", &root])?;
            git(dir, home, &["branch", "top", &mid])?;
            git(dir, home, &["tag", "top", &tip])?;
            git(dir, home, &["branch", "dual", &mid])?;
            git(dir, home, &["branch", "rem/ambi", &mid])?;
            git(dir, home, &["update-ref", "refs/remotes/rem/ambi", &tip])?;

            // The premise, asserted rather than assumed: a shape whose names
            // stopped being ambiguous would still build, and every case on it
            // would then measure an ordinary lookup while claiming otherwise.
            for (name, want) in [("ambi", &tip), ("top", &root), ("rem/ambi", &mid)] {
                let got = rev(dir, home, name)?;
                if &got != want {
                    bail!("fixture: ambiguous-ref: {name} resolved to {got}, wanted {want}");
                }
            }
        }

        Shape::PrefixCollision => {
            write(dir, "commit-mate.txt", COLLIDE_COMMIT_MATE)?;
            write(dir, "pair-a.txt", COLLIDE_PAIR_A)?;
            write(dir, "pair-b.txt", COLLIDE_PAIR_B)?;
            git(dir, home, &["add", "commit-mate.txt", "pair-a.txt", "pair-b.txt"])?;
            git(dir, home, &["commit", "-qm", "collision: three blobs at two prefixes"])?;
            // A second commit, so `log --oneline --abbrev=4` has a row whose id
            // is *not* the colliding one to print beside it: the widening shows
            // as a difference between two rows of one listing.
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "collision: a second commit to abbreviate"])?;

            // Both halves of the premise, asserted. `--disambiguate` lists every
            // object with the prefix, so this is the property itself rather than
            // a proxy for it: two candidates each, and a commit among the first
            // pair.
            for (prefix, want_kinds) in
                [("edfa", ["commit", "blob"].as_slice()), ("a366", ["blob", "blob"].as_slice())]
            {
                let arg = format!("--disambiguate={prefix}");
                let listed = git(dir, home, &["rev-parse", &arg])?;
                let ids: Vec<&str> = listed.lines().collect();
                if ids.len() != want_kinds.len() {
                    bail!(
                        "fixture: prefix-collision: {prefix} names {} objects, wanted {}",
                        ids.len(),
                        want_kinds.len()
                    );
                }
                let mut kinds: Vec<String> = Vec::new();
                for id in &ids {
                    kinds.push(git(dir, home, &["cat-file", "-t", id])?.trim().to_string());
                }
                kinds.sort();
                let mut wanted: Vec<String> = want_kinds.iter().map(|k| (*k).to_string()).collect();
                wanted.sort();
                if kinds != wanted {
                    bail!("fixture: prefix-collision: {prefix} holds {kinds:?}, wanted {wanted:?}");
                }
            }
        }

        Shape::AmHooks => {
            write(dir, "app/main.c", MAIN_C_BASE)?;
            git(dir, home, &["add", "app"])?;
            git(dir, home, &["commit", "-qm", "am-hooks: seed"])?;

            // The patches live on side branches so `main`'s tree stays the
            // pre-image every mailbox applies to, the way `Shape::Patches` does
            // it.
            git(dir, home, &["checkout", "-q", "-b", "am-pending"])?;
            write(dir, "app/main.c", MAIN_C_ONE)?;
            git(dir, home, &["commit", "-qam", "am-hooks: add subtract"])?;
            write(dir, "app/main.c", MAIN_C_TWO)?;
            git(dir, home, &["commit", "-qam", "am-hooks: bump version"])?;

            // The word `applypatch-msg` refuses on, in the message rather than
            // in the diff: the hook is handed the message file alone.
            git(dir, home, &["checkout", "-q", "-b", "am-reject", "main"])?;
            write(dir, "rejected.txt", "the message asks the hook to refuse\n")?;
            git(dir, home, &["add", "rejected.txt"])?;
            git(dir, home, &["commit", "-qm", "am-hooks: REJECT this one"])?;

            // The path `pre-applypatch` refuses on, in the diff rather than in
            // the message: that hook is given no arguments and can only look at
            // the tree the patch has already been applied to.
            git(dir, home, &["checkout", "-q", "-b", "am-preveto", "main"])?;
            write(dir, "veto-preapply.txt", "stop before the commit\n")?;
            git(dir, home, &["add", "veto-preapply.txt"])?;
            git(dir, home, &["commit", "-qm", "am-hooks: trips pre-applypatch"])?;

            git(dir, home, &["checkout", "-q", "main"])?;
            for (file, args) in [
                ("mail/ok.mbox", ["main..am-pending"].as_slice()),
                ("mail/reject.mbox", ["-1", "am-reject"].as_slice()),
                ("mail/preveto.mbox", ["-1", "am-preveto"].as_slice()),
            ] {
                let mut argv = vec!["format-patch", "--no-signature", "--stdout"];
                argv.extend_from_slice(args);
                let mbox = git(dir, home, &argv)?;
                write(dir, file, &mbox)?;
            }
            git(dir, home, &["add", "mail"])?;
            git(dir, home, &["commit", "-qm", "am-hooks: mailboxes"])?;

            install_hooks(&dir.join(".git/hooks"), AM_HOOKS)?;
            // The two that must NOT run. Written here rather than through
            // `install_hooks` precisely because that function's contract is to
            // make a hook executable, and the absence of that bit is the whole
            // measurement.
            install_inert_hooks(&dir.join(".git/hooks"), INERT_HOOKS)?;
        }

        Shape::NestedSubmodule => {
            // Both upstreams are staged inside the fixture and the staging
            // directory is removed before the parent commits, so no path outside
            // `dir` is touched and nothing untracked is left behind. A staging
            // checkout beside the template — how `Shape::Submodule` builds its
            // upstream — would put the build location into the fixture, which is
            // the one thing this shape must not do.
            let stage = dir.join(NESTED_STAGE);
            let leaf_work = stage.join("leaf");
            std::fs::create_dir_all(&leaf_work)?;
            git(&leaf_work, home, &["init", "-q", "-b", "main", "."])?;
            write(&leaf_work, "leaf.txt", "leaf one\n")?;
            git(&leaf_work, home, &["add", "leaf.txt"])?;
            git(&leaf_work, home, &["commit", "-qm", "leaf: one"])?;
            write(&leaf_work, "leaf.txt", "leaf two\n")?;
            git(&leaf_work, home, &["commit", "-qam", "leaf: two"])?;
            let leaf_head = rev(&leaf_work, home, "HEAD")?;
            git(dir, home, &["init", "-q", "--bare", "-b", "main", LEAF_PEER])?;
            // Relative, resolved against the staging checkout's own directory,
            // so even the transient repository never names the build location.
            git(&leaf_work, home, &["remote", "add", "origin", "../../.leaf.git"])?;
            git(&leaf_work, home, &["push", "-q", "origin", "main"])?;

            let mid_work = stage.join("mid");
            std::fs::create_dir_all(&mid_work)?;
            git(&mid_work, home, &["init", "-q", "-b", "main", "."])?;
            write(&mid_work, "mid.txt", "mid\n")?;
            // Written, not produced by `submodule add`: that would clone the
            // leaf into *this* directory and record the path it resolved.
            write(
                &mid_work,
                ".gitmodules",
                "[submodule \"leaf\"]\n\tpath = leaf\n\turl = ../.leaf.git\n",
            )?;
            git(&mid_work, home, &["add", "mid.txt", ".gitmodules"])?;
            let cacheinfo = format!("160000,{leaf_head},leaf");
            git(&mid_work, home, &["update-index", "--add", "--cacheinfo", &cacheinfo])?;
            git(&mid_work, home, &["commit", "-qm", "mid: carry a leaf submodule"])?;
            let mid_head = rev(&mid_work, home, "HEAD")?;
            git(dir, home, &["init", "-q", "--bare", "-b", "main", MID_PEER])?;
            git(&mid_work, home, &["remote", "add", "origin", "../../.mid.git"])?;
            git(&mid_work, home, &["push", "-q", "origin", "main"])?;
            std::fs::remove_dir_all(&stage)?;

            // The parent registers `mid` the same way `mid` registers `leaf`,
            // and for the same reason: `submodule add` clones, and a clone
            // writes the absolute path it resolved into `.git/config` and into
            // every reflog it creates.
            write(dir, ".gitmodules", "[submodule \"mid\"]\n\tpath = mid\n\turl = ./.mid.git\n")?;
            git(dir, home, &["add", ".gitmodules"])?;
            let cacheinfo = format!("160000,{mid_head},mid");
            git(dir, home, &["update-index", "--add", "--cacheinfo", &cacheinfo])?;
            git(dir, home, &["commit", "-qm", "nested: a submodule that carries one"])?;

            // The empty directory a clone leaves at an uninitialised
            // submodule's path. Without it `status` calls the gitlink deleted,
            // which is a different repository state from an unpopulated one;
            // the per-case copy is a directory walk, so it survives.
            std::fs::create_dir_all(dir.join("mid"))?;
            write(dir, ".git/info/exclude", ".leaf.git/\n.mid.git/\n")?;
        }

        Shape::SplitIndex => {
            write(dir, "si-a.txt", "a\n")?;
            write(dir, "si-b.txt", "b\n")?;
            write(dir, "sub/si-c.txt", "c\n")?;
            git(dir, home, &["add", "si-a.txt", "si-b.txt", "sub/si-c.txt"])?;
            git(dir, home, &["commit", "-qm", "split-index: seed"])?;

            // Order is the whole trick: `read-tree` zeroes the `stat` fields, so
            // the shared index that splits out of the result is a function of
            // the tree alone and both builds name it the same.
            git(dir, home, &["read-tree", "HEAD"])?;
            git(dir, home, &["update-index", "--split-index"])?;

            // A commit after the split, so `.git/index` carries entries of its
            // own beside the `link` extension rather than being a bare pointer.
            // Verified not to re-share: the shared half keeps one name and one
            // set of bytes across two builds.
            write(dir, "si-d.txt", "d\n")?;
            git(dir, home, &["add", "si-d.txt"])?;
            git(dir, home, &["commit", "-qm", "split-index: after the split"])?;

            let shared = std::fs::read_dir(dir.join(".git"))?
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("sharedindex."))
                .count();
            if shared != 1 {
                bail!("fixture: split-index: {shared} shared index files, wanted exactly 1");
            }
        }
    }
    Ok(())
}

/// The bare peer every shape that needs one keeps *inside* the fixture, so the
/// per-case copy carries its own. Spelled the same as `runner::PEER_DIR`, which
/// is where `probe_peer` looks.
const PEER: &str = ".remote.git";
/// The peer as a URL: relative, so the copy resolves to its own peer rather than
/// to the template's. Absolute would make every case share one remote.
const PEER_URL: &str = "./.remote.git";
/// Where [`restage_as_clone`] puts the clone before moving it up.
const STAGE_DIR: &str = ".stage";

/// One revision resolved to its id, trimmed.
fn rev(dir: &Path, home: &Path, spec: &str) -> Result<String> {
    Ok(git(dir, home, &["rev-parse", spec])?.trim().to_string())
}

/// Write executable hook scripts into `hooks_dir`.
fn install_hooks(hooks_dir: &Path, hooks: &[(&str, &str)]) -> Result<()> {
    std::fs::create_dir_all(hooks_dir)?;
    for (name, body) in hooks {
        let path = hooks_dir.join(name);
        std::fs::write(&path, body).with_context(|| format!("write hook {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Write hook scripts into `hooks_dir` **without** the executable bit.
///
/// The mirror image of [`install_hooks`], and a separate function rather than a
/// mode argument on that one: every existing caller means "install a hook that
/// runs", and a shape whose hooks silently stopped running would go on passing
/// while measuring nothing. Here the missing bit *is* the subject, so it is
/// spelled in the name.
fn install_inert_hooks(hooks_dir: &Path, hooks: &[(&str, &str)]) -> Result<()> {
    std::fs::create_dir_all(hooks_dir)?;
    for (name, body) in hooks {
        let path = hooks_dir.join(name);
        std::fs::write(&path, body).with_context(|| format!("write hook {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}


/// The bare upstreams [`Shape::NestedSubmodule`] keeps inside the fixture, and
/// the directory its two staging checkouts are built in and removed from.
const LEAF_PEER: &str = ".leaf.git";
const MID_PEER: &str = ".mid.git";
/// The mid upstream as the parent reads it: relative to the parent's own root,
/// which is where `submodule update` runs.
const MID_PEER_URL: &str = "./.mid.git";
const NESTED_STAGE: &str = ".stage-mod";

/// Blob bodies whose ids collide with something else in
/// [`Shape::PrefixCollision`] at four characters. See that variant's doc for how
/// they were found; the ids they must produce are asserted at build time.
const COLLIDE_COMMIT_MATE: &str = "collide 62671\n";
const COLLIDE_PAIR_A: &str = "collide 105\n";
const COLLIDE_PAIR_B: &str = "collide 215\n";

/// The three `am` hooks [`Shape::AmHooks`] installs, executable.
///
/// None of them runs git, for the reason [`Shape::Hooked`] gives: each side of a
/// case runs its own binary, and a hook naming one by path would make the other
/// side execute it too.
const AM_HOOKS: &[(&str, &str)] = &[
    (
        "applypatch-msg",
        "#!/bin/sh\n\
         printf 'applypatch-msg\\n' >> hook-applypatch-msg.txt\n\
         grep -q REJECT \"$1\" && exit 1\n\
         printf '\\napplypatch-trailer\\n' >> \"$1\"\n\
         exit 0\n",
    ),
    (
        "pre-applypatch",
        "#!/bin/sh\n\
         printf 'pre-applypatch\\n' >> hook-pre-applypatch.txt\n\
         test -f veto-preapply.txt && exit 1\n\
         exit 0\n",
    ),
    // Exits non-zero on purpose: git ignores a `post-*` hook's status, and a
    // case that sees `am` succeed anyway is what pins that.
    (
        "post-applypatch",
        "#!/bin/sh\n\
         printf 'post-applypatch\\n' >> hook-post-applypatch.txt\n\
         exit 1\n",
    ),
];

/// The two hooks [`Shape::AmHooks`] installs **without** the executable bit.
///
/// Both would be loud if they ran: one refuses the commit outright, the other
/// rewrites its message. A commit on that shape must show neither.
const INERT_HOOKS: &[(&str, &str)] = &[
    (
        "pre-commit",
        "#!/bin/sh\n\
         printf 'pre-commit ran\\n' >> hook-pre-commit.txt\n\
         exit 1\n",
    ),
    (
        "commit-msg",
        "#!/bin/sh\n\
         printf '\\nnot-executable-trailer\\n' >> \"$1\"\n\
         exit 0\n",
    ),
];

/// Replace the repository at `dir` with a clone of the peer already built
/// inside it, keeping the peer.
///
/// Shallow and partial clones cannot be *made* out of a repository that already
/// has every object: `--depth` and `--filter` are properties of what the server
/// sent, so the only faithful way to build one is to clone. [`build`] has
/// already initialised `dir` and committed into it by the time a shape's arm
/// runs, and `git clone` refuses a non-empty destination — hence the wipe, the
/// clone into [`STAGE_DIR`], and the move back up.
///
/// Three details are load-bearing:
///
/// * `--no-local`. Git ignores both `--depth` and `--filter` when it recognises
///   a local path and takes its directory-copy shortcut (`warning: --depth is
///   ignored in local clones; use file:// instead`), so the shape would build
///   without failing and carry neither property.
/// * a **relative** URL, not `file://`. Either would work as a transport;
///   neither survives the clone unchanged. Git resolves the URL before it
///   records it, so `remote.origin.url` and the `clone: from <url>` line in
///   every reflog it writes name the build directory, and a fixture built at
///   two locations differs there — which is what `shapes_build_reproducibly`
///   fails on. Both are put back: the config below, the reflogs in
///   [`scrub_clone_url`]. The relative form is preferred anyway because it is
///   what the fixture ends up carrying, so the two spellings never disagree.
/// * `protocol.file.allow=always`, explicitly. The default is `user`, which
///   happens to permit this clone today; naming it means the shape does not
///   depend on that default staying put.
///
/// Nothing here resolves a hostname, at build time or at case time: the peer is
/// a directory inside the fixture and the URL that survives into the config is
/// `./.remote.git`.
fn restage_as_clone(dir: &Path, home: &Path, clone_args: &[&str]) -> Result<()> {
    let doomed: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != std::ffi::OsStr::new(PEER))
        .map(|e| e.path())
        .collect();
    for path in doomed {
        if std::fs::symlink_metadata(&path)?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }

    let mut args: Vec<&str> =
        vec!["-c", "protocol.file.allow=always", "clone", "-q", "--no-local"];
    args.extend_from_slice(clone_args);
    args.push(PEER_URL);
    args.push(STAGE_DIR);
    git(dir, home, &args)?;

    let stage = dir.join(STAGE_DIR);
    let moved: Vec<PathBuf> =
        std::fs::read_dir(&stage)?.filter_map(Result::ok).map(|e| e.path()).collect();
    for path in moved {
        let name = path.file_name().context("staged entry has no name")?;
        std::fs::rename(&path, dir.join(name))?;
    }
    std::fs::remove_dir(&stage)?;

    // The clone recorded the peer's absolute path; put the relative form back so
    // the per-case copy reaches its own peer.
    git(dir, home, &["config", "remote.origin.url", PEER_URL])?;
    // The prologue's setting went with the old `.git`. See [`build`] for why
    // every fixture needs it.
    git(dir, home, &["config", "core.checkStat", "minimal"])?;
    write(dir, ".git/info/exclude", ".remote.git/\n")?;
    scrub_clone_url(dir)?;
    Ok(())
}

/// Rewrite the absolute peer path git recorded in the clone's reflogs back to
/// the relative one, and fail loudly if any of it survives.
///
/// `git clone` writes `clone: from <url>` into `.git/logs/HEAD` and into the
/// reflog of every ref it created, and it absolutises the URL first — so a
/// fixture built at two locations differs in exactly those files and
/// `shapes_build_reproducibly` fails on it. The reflog is real repository state
/// that `probe_reflogs` compares, so it is normalised rather than deleted:
/// both sides of a case then read the same three lines, and they name the same
/// relative peer the config does.
///
/// The check at the end is the part that matters. A future git that records the
/// path somewhere else would otherwise reintroduce the leak silently, and the
/// determinism test would report it as a mystery rather than as this.
fn scrub_clone_url(dir: &Path) -> Result<()> {
    // The *whole* message is rewritten rather than the path inside it. Replacing
    // the path meant computing it, and the computed form and the recorded form
    // differ by whichever symlinks `getcwd(2)` resolved — on macOS the fixture
    // is under `/tmp` and git records `/private/tmp/...`, so a substring
    // replacement of the un-resolved path matched the tail and left `/private`
    // behind. A reflog message has exactly one shape here, so replacing it
    // outright has nothing to get wrong.
    const MARKER: &str = "\tclone: from ";
    let roots: Vec<String> = [Some(dir.to_path_buf()), dir.canonicalize().ok()]
        .into_iter()
        .flatten()
        .map(|p| p.display().to_string())
        .collect();

    for (_, path) in walk(&dir.join(".git").join("logs")) {
        let body = std::fs::read_to_string(&path)?;
        let fixed: String = body
            .lines()
            .map(|line| match line.split_once(MARKER) {
                Some((head, _)) => format!("{head}{MARKER}{PEER_URL}\n"),
                None => format!("{line}\n"),
            })
            .collect();
        for root in &roots {
            if fixed.contains(root.as_str()) {
                bail!(
                    "fixture: {} still names the build directory after scrubbing",
                    path.display()
                );
            }
        }
        if fixed != body {
            std::fs::write(&path, fixed)?;
        }
    }
    Ok(())
}

/// Every regular file under `dir`, as `(path relative to dir, absolute path)`,
/// sorted. Empty when `dir` does not exist.
fn walk(dir: &Path) -> Vec<(String, PathBuf)> {
    fn rec(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => rec(&path, &rel, out),
                Ok(m) if m.is_file() => out.push((rel, path)),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    rec(dir, "", &mut out);
    out.sort();
    out
}

/// The hooks [`Shape::HooksFail`] installs in the repository under test.
///
/// Every one of them writes a file into the worktree naming what it was handed,
/// so *which* hooks ran and in what order survives the command and is read by
/// the state probe — a hook that only exits cannot be distinguished from one
/// that was never called. None of them invokes git: each side of a case runs its
/// own binary, and a hook naming one by path would make the other side execute
/// it too.
///
/// The refusals are `pre-commit`, `pre-push`, `pre-rebase` and `pre-auto-gc`.
/// `post-commit` exits 1 deliberately and git ignores it, which is the other
/// half of the same question.
const FAILING_HOOKS: &[(&str, &str)] = &[
    (
        "pre-commit",
        r##"#!/bin/sh
printf 'pre-commit refuses\n' >&2
printf 'pre-commit %s\n' "${GIT_INDEX_FILE##*/}" > hook-pre-commit.txt
exit 1
"##,
    ),
    // Not skipped by `--no-verify`, unlike the two hooks around it, so this is
    // what shows that a commit which bypassed the gate still went through the
    // rewrite.
    (
        "prepare-commit-msg",
        r##"#!/bin/sh
printf '\nprepared-by-hook\n' >> "$1"
printf 'prepare-commit-msg %s %s\n' "$2" "$3" > hook-prepare-commit-msg.txt
exit 0
"##,
    ),
    (
        "commit-msg",
        r##"#!/bin/sh
printf '\ncommit-msg-trailer\n' >> "$1"
printf 'commit-msg %s\n' "${1##*/}" > hook-commit-msg.txt
exit 0
"##,
    ),
    (
        "pre-merge-commit",
        r##"#!/bin/sh
printf 'pre-merge-commit\n' > hook-pre-merge-commit.txt
exit 0
"##,
    ),
    (
        "post-merge",
        r##"#!/bin/sh
printf 'post-merge %s\n' "$1" > hook-post-merge.txt
exit 0
"##,
    ),
    // Exits 1 on purpose: git ignores a `post-commit` failure, and an
    // implementation that propagates it turns a successful commit into a
    // failing one.
    (
        "post-commit",
        r##"#!/bin/sh
printf 'post-commit\n' > hook-post-commit.txt
exit 1
"##,
    ),
    (
        "post-checkout",
        r##"#!/bin/sh
printf 'post-checkout %s %s %s\n' "$1" "$2" "$3" > hook-post-checkout.txt
exit 0
"##,
    ),
    // Reads its stdin: the ref list git feeds it is a function of the rewrite,
    // so recording it turns `post-rewrite` into something a case can compare.
    (
        "post-rewrite",
        r##"#!/bin/sh
printf 'post-rewrite %s\n' "$1" > hook-post-rewrite.txt
cat >> hook-post-rewrite.txt
exit 0
"##,
    ),
    (
        "pre-push",
        r##"#!/bin/sh
printf 'pre-push %s\n' "$1" > hook-pre-push.txt
cat >> hook-pre-push.txt
printf 'pre-push refuses\n' >&2
exit 1
"##,
    ),
    (
        "pre-rebase",
        r##"#!/bin/sh
printf 'pre-rebase %s %s\n' "$1" "$2" > hook-pre-rebase.txt
printf 'pre-rebase refuses\n' >&2
exit 1
"##,
    ),
    (
        "pre-auto-gc",
        r##"#!/bin/sh
printf 'pre-auto-gc refuses\n' > hook-pre-auto-gc.txt
exit 1
"##,
    ),
];

/// The hook [`Shape::HooksFail`] installs in its **peer**.
///
/// It runs inside the receiving repository, which is the one refusal
/// `--no-verify` cannot bypass: `--no-verify` skips the hooks on the pushing
/// side and has no say over the other end. It refuses one ref by name so a
/// single push can be accepted and rejected at once.
const PEER_HOOKS: &[(&str, &str)] = &[(
    "update",
    r##"#!/bin/sh
if [ "$1" = "refs/heads/veto" ]; then
	printf 'update refuses %s\n' "$1" >&2
	exit 1
fi
exit 0
"##,
)];

/// The id `refs/heads/dangling` points at in [`Shape::Damaged`]: well-formed,
/// and belonging to no object. A literal rather than a hash of anything, so it
/// cannot accidentally become an object the fixture also stores.
pub const MISSING_OBJECT: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
/// The id of [`Shape::Damaged`]'s corrupt loose object, chosen the same way and
/// for the same reason.
pub const CORRUPT_OBJECT: &str = "ab1234567890123456789012345678901234abcd";

/// Start an unborn branch and clear the previous history out of the index and
/// the worktree, so the next commit is a *root* rather than a child.
///
/// `git checkout --orphan` moves HEAD alone: the index and worktree are left
/// holding the branch that was checked out, and committing them would make a
/// root whose tree is the old one. `carried` names the files that survive the
/// index reset and have to be removed by hand.
fn orphan(dir: &Path, home: &Path, name: &str, carried: &[&str]) -> Result<()> {
    git(dir, home, &["checkout", "-q", "--orphan", name])?;
    git(dir, home, &["rm", "-r", "-q", "--cached", "."])?;
    for path in carried {
        std::fs::remove_file(dir.join(path))?;
    }
    Ok(())
}

/// Run stock git for a step whose *failure* is the state being built, and fail
/// loudly if it succeeds.
///
/// A conflicted merge exits non-zero, so [`git`] cannot run one — and a step
/// that silently stopped conflicting would leave a shape that still builds and
/// no longer carries what its cases need.
fn git_conflicting(dir: &Path, home: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = crate::stock::command()?;
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(args);
    let out = cmd.output().with_context(|| format!("spawn stock git {args:?}"))?;
    if out.status.success() {
        bail!("fixture: stock git {args:?} in {} was expected to conflict", dir.display());
    }
    Ok(())
}

/// Create a symlink at `rel` pointing at `target`.
///
/// Targets are always relative: an absolute one would put the build directory
/// into a tracked blob, and the shape would hash differently at a second build
/// location — which is exactly what `shapes_build_reproducibly` fails on.
#[cfg(unix)]
fn symlink(dir: &Path, rel: &str, target: &str) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, &path)
        .with_context(|| format!("symlink {} -> {target}", path.display()))
}

/// Symlink fixtures need a filesystem that has symlinks. Reported rather than
/// silently degraded: a shape built without its symlinks would still build, and
/// every case using it would then measure nothing.
#[cfg(not(unix))]
fn symlink(_dir: &Path, rel: &str, target: &str) -> Result<()> {
    bail!("fixture: symlink {rel} -> {target} needs a unix filesystem")
}

/// `é.txt`, decomposed: `e` + U+0301, the form macOS stores and hands back.
///
/// Written as an escape rather than as a literal so the bytes are unambiguous
/// in the source — an editor that normalises on save would otherwise turn this
/// shape into a duplicate of [`Shape::AwkwardPaths`] without anything noticing.
pub const NFD_TRACKED: &str = "e\u{301}.txt";
/// The same combining mark on an untracked path.
pub const NFD_UNTRACKED: &str = "e\u{301}-new.txt";

/// The history the two mergeable shapes share.
///
/// A branch cannot be both fast-forwardable from `main` and diverged from it,
/// and the two take different paths through git's gates — `unpack_trees()`
/// alone versus the strategy's index check first — so both kinds are built and
/// each case picks by branch name:
///
/// * `div-*` fork before `main`'s last commit, so merging one is a three-way.
/// * `ff-*` fork after it, so merging one is a fast-forward.
///
/// Each branch touches exactly one path, named for what the shape's dirt does
/// to it: `cold` is left alone by the worktree, `hot` is edited there, `squat`
/// is added by the branch and squatted on by an untracked file, `other` exists
/// so two heads can be merged at once as an octopus.
fn mergeable_history(dir: &Path, home: &Path) -> Result<()> {
    for (path, body) in [
        ("keep.txt", "keep\n"),
        ("hot.txt", "hot\n"),
        ("cold.txt", "cold\n"),
        ("trunk.txt", "trunk\n"),
    ] {
        write(dir, path, body)?;
    }
    git(dir, home, &["add", "."])?;
    git(dir, home, &["commit", "-qm", "mergeable base"])?;

    for (branch, path, body) in [
        ("div-cold", "cold.txt", "cold, from div\n"),
        ("div-hot", "hot.txt", "hot, from div\n"),
        ("div-squat", "squat.txt", "squat, from div\n"),
        ("div-other", "other.txt", "other, from div\n"),
    ] {
        git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
        write(dir, path, body)?;
        git(dir, home, &["add", path])?;
        git(dir, home, &["commit", "-qm", branch])?;
    }

    // `main` moves last among the shared commits, which is what leaves it
    // diverged from every `div-*` above and an ancestor of every `ff-*` below.
    git(dir, home, &["checkout", "-q", "main"])?;
    write(dir, "trunk.txt", "trunk, moved\n")?;
    git(dir, home, &["commit", "-qam", "main moves"])?;

    for (branch, path, body) in [
        ("ff-cold", "cold.txt", "cold, from ff\n"),
        ("ff-hot", "hot.txt", "hot, from ff\n"),
        ("ff-squat", "squat.txt", "squat, from ff\n"),
    ] {
        git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
        write(dir, path, body)?;
        git(dir, home, &["add", path])?;
        git(dir, home, &["commit", "-qm", branch])?;
    }
    git(dir, home, &["checkout", "-q", "main"])?;
    Ok(())
}

/// A deterministic non-text blob. `variant` changes every byte, so two revisions
/// of the same file are unmistakably binary *and* unmistakably different.
fn binary_blob(variant: u8) -> Vec<u8> {
    (0..1024u32)
        .map(|i| ((i * 7 + variant as u32 * 13) % 251) as u8)
        .collect()
}

/// Tab-indented seed for [`Shape::Whitespace`].
const WS_TABS: &str = "int main(void)\n{\n\tint total = 0;\n\tfor (int i = 0; i < 10; i++) {\n\t\ttotal += i;\n\t}\n\treturn total;\n}\n";
/// The same lines, tabs expanded to four spaces. Nothing else changes.
const WS_SPACES: &str = "int main(void)\n{\n    int total = 0;\n    for (int i = 0; i < 10; i++) {\n        total += i;\n    }\n    return total;\n}\n";
/// The space-indented form with trailing blanks appended to three lines.
const WS_TRAILING: &str = "int main(void)\n{\n    int total = 0;   \n    for (int i = 0; i < 10; i++) {\t\n        total += i;\n    }\n    return total; \n}\n";
/// One real edit (`+= i` becomes `+= i * 2`) with whitespace churn on every
/// other line. `-w` must report exactly the one edit.
const WS_MIXED: &str = "int main(void)\n{\n  int total = 0;\n  for (int i = 0; i < 10; i++) {\n      total += i * 2;\n  }\n  return total;\n}\n";
/// The unstaged, whitespace-only worktree edit: same tokens, eight-space indent.
const WS_REINDENTED: &str = "int main(void)\n{\n        int total = 0;\n        for (int i = 0; i < 10; i++) {\n                total += i * 2;\n        }\n        return total;\n}\n";

/// Pre-image every patch in [`Shape::Patches`] applies to.
const MAIN_C_BASE: &str = "static const int VERSION = 1;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint main(void)\n{\n\treturn add(1, 2);\n}\n";
/// First patch's post-image: a function added.
const MAIN_C_ONE: &str = "static const int VERSION = 1;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint subtract(int a, int b)\n{\n\treturn a - b;\n}\n\nint main(void)\n{\n\treturn add(1, 2) + subtract(4, 3);\n}\n";
/// Second patch's post-image: a constant bumped.
const MAIN_C_TWO: &str = "static const int VERSION = 2;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint subtract(int a, int b)\n{\n\treturn a - b;\n}\n\nint main(void)\n{\n\treturn add(1, 2) + subtract(4, 3);\n}\n";

/// Patch-shaped but corrupt: the hunk header promises seven pre-image lines and
/// supplies one. `apply --check`, with or without `--cached`, has to reject it —
/// accepting it at exit 0 is a regression that shipped once and could not be
/// pinned because no fixture carried a patch.
const CORRUPT_PATCH: &str = "diff --git a/app/main.c b/app/main.c\nindex 1111111..2222222 100644\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,7 +1,9 @@\n static const int VERSION = 1;\n+int corrupt(void);\n";

/// A hunk of pure context, changing nothing. Stock rejects it as corrupt
/// (`error: corrupt patch at ...:8`, exit 128); the case pins that agreement,
/// which a parser that silently accepts a no-op hunk would break.
const CONTEXT_ONLY_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,3 +1,3 @@\n static const int VERSION = 1;\n \n int add(int a, int b)\n";

/// The same change `valid.patch` carries, with a hunk header that points three
/// lines too early, so applying it takes the offset search rather than a
/// literal line-number match (stock: `Hunk #1 succeeded at 5 (offset 3 lines)`).
///
/// The header says line 2, not line 1, deliberately: `git apply` treats a hunk
/// starting at line 1 as anchored to the start of the file and refuses to
/// search at all, so a `-1,7` header would test rejection instead of search.
const OFFSET_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -2,7 +2,12 @@\n \treturn a + b;\n }\n \n+int subtract(int a, int b)\n+{\n+\treturn a - b;\n+}\n+\n int main(void)\n {\n-\treturn add(1, 2);\n+\treturn add(1, 2) + subtract(4, 3);\n }\n";

/// Adds a line with trailing blanks and one indented with a space before a tab,
/// so `--whitespace=warn|error|fix` each have something to act on.
const WHITESPACE_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,4 +1,6 @@\n static const int VERSION = 1;\n+int trailing(void);  \n+ \tint indented(void);\n \n int add(int a, int b)\n {\n";

/// Recursive copy used to clone a prebuilt template per case. Copying beats
/// rebuilding: fixture construction is the slowest part of a run, and every
/// case needs a pristine repo.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            #[cfg(not(unix))]
            std::fs::copy(&from, &to)?;
        } else {
            copy_file(&from, &to, &meta)?;
        }
    }
    Ok(())
}

/// Copy one file, carrying its modification time across.
///
/// The index stores each entry's `stat` data and git compares it before
/// trusting the entry. Inode and creation time cannot survive a copy, which is
/// why the fixtures set `core.checkStat=minimal`; mtime and size can survive,
/// and this is what makes them.
fn copy_file(from: &Path, to: &Path, meta: &std::fs::Metadata) -> Result<()> {
    std::fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    let Ok(mtime) = meta.modified() else { return Ok(()) };

    // Loose objects and packs are copied read-only, and a read-only handle
    // cannot carry a timestamp update; widen the mode for the call, then put it
    // back so the copy is a faithful one.
    let perms = meta.permissions();
    if perms.readonly() {
        let mut writable = perms.clone();
        writable.set_readonly(false);
        std::fs::set_permissions(to, writable)?;
    }
    if let Ok(f) = std::fs::File::options().write(true).open(to) {
        let _ = f.set_modified(mtime);
    }
    if perms.readonly() {
        std::fs::set_permissions(to, perms)?;
    }
    Ok(())
}

/// Prebuilt template directories, one per shape.
pub struct Templates {
    root: PathBuf,
    pub home: PathBuf,
}

impl Templates {
    /// Build every shape once under `root`.
    pub fn build_all(root: &Path) -> Result<Self> {
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        let templates = root.join("templates");
        std::fs::create_dir_all(&templates)?;
        for &shape in Shape::ALL {
            let dir = templates.join(shape.name());
            if dir.exists() {
                continue;
            }
            build(shape, &dir, &home)
                .with_context(|| format!("building fixture shape {}", shape.name()))?;
        }
        Ok(Self { root: templates, home })
    }

    /// Materialize a pristine copy of `shape` at `dst`.
    pub fn instantiate(&self, shape: Shape, dst: &Path) -> Result<()> {
        copy_tree(&self.root.join(shape.name()), dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over file bytes. The hash only has to distinguish, not resist
    /// attack, and this keeps the test free of a dependency.
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Hash every file in the shape, path-relative and sorted.
    ///
    /// `.git/index` is the one exclusion: it stores `stat` data (inode, device,
    /// mtime) for each entry, which is filesystem state rather than repository
    /// state and cannot match between two directories. The index's *logical*
    /// contents are covered by the `ls-files -v` probe in [`digest`] instead.
    ///
    /// Nothing is normalized. A shape that records its own absolute path
    /// anywhere will hash differently at the two build locations, and failing
    /// on that is the point.
    fn hash_tree(root: &Path, rel: &Path, out: &mut Vec<String>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(root.join(rel))?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        entries.sort();
        for name in entries {
            let child = rel.join(&name);
            if child == Path::new(".git/index") {
                continue;
            }
            let abs = root.join(&child);
            let meta = std::fs::symlink_metadata(&abs)?;
            if meta.is_dir() {
                out.push(format!("d {}", child.display()));
                hash_tree(root, &child, out)?;
            } else if meta.is_symlink() {
                out.push(format!("l {} -> {}", child.display(), std::fs::read_link(&abs)?.display()));
            } else {
                let bytes = std::fs::read(&abs)?;
                out.push(format!("f {} {:016x} {}", child.display(), fnv1a(&bytes), bytes.len()));
            }
        }
        Ok(())
    }

    /// One probe's answer: its stdout, and the exit code it left.
    ///
    /// The code is part of the answer because a probe is allowed to *fail* here.
    /// [`Shape::Damaged`] carries a ref pointing at a missing object, and
    /// `for-each-ref` exits 128 rather than printing it, so a probe runner that
    /// insisted on success could not ask any question at all about a repository
    /// with something wrong in it. Recording the code keeps the check strict in
    /// the direction that matters: a probe that succeeds in one build and fails
    /// in the other is still a difference, and the failing probe's stdout is
    /// still compared. stderr is deliberately left out — it names paths, and the
    /// two builds are at two different ones.
    fn probe(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
        let mut cmd = crate::stock::command()?;
        env::harden(&mut cmd, home);
        cmd.current_dir(dir).args(args);
        let out = cmd.output().with_context(|| format!("spawn stock git {args:?}"))?;
        Ok(format!(
            "exit {}\n{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout)
        ))
    }

    /// Repository state as stock git reports it, plus the on-disk bytes.
    fn digest(dir: &Path, home: &Path) -> Result<String> {
        let mut s = String::new();
        for args in [
            &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"][..],
            &["ls-files", "-v", "--full-name"][..],
            &["cat-file", "--batch-check", "--batch-all-objects"][..],
            &["status", "--porcelain=v1", "--untracked-files=all"][..],
        ] {
            let mut lines: Vec<String> =
                probe(dir, home, args)?.lines().map(str::to_string).collect();
            // `--batch-all-objects` walks packs and loose storage in an order the
            // filesystem influences; the object *set* is what must be stable.
            lines.sort();
            s.push_str(&format!("# {}\n{}\n", args.join(" "), lines.join("\n")));
        }
        let mut files = Vec::new();
        hash_tree(dir, Path::new(""), &mut files)?;
        s.push_str("# tree\n");
        s.push_str(&files.join("\n"));
        Ok(s)
    }

    /// Every shape must build to the same bytes twice.
    ///
    /// A fixture that varies between two builds makes every case that uses it
    /// unmeasurable, and the harness would report that as an implementation
    /// difference rather than as its own defect.
    ///
    /// `Submodule` is excluded by construction, not by convenience: it records
    /// the absolute path of its upstream in `.gitmodules` and `.git/config`, so
    /// two copies at different paths are *supposed* to differ.
    ///
    /// The builder needs a stock git to run at all, and [`crate::stock::git`]
    /// refuses one older than the version the port targets — a machine without a
    /// current git cannot answer this question, so the test says so rather than
    /// reporting a determinism failure it did not measure. Every path that
    /// *reports a number* still refuses outright.
    #[test]
    fn shapes_build_reproducibly() {
        if let Err(why) = crate::stock::git() {
            eprintln!("skipping: no git to build fixtures with — {why}");
            return;
        }
        let root = std::env::temp_dir().join(format!("zvcs-fixture-determinism-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        for &shape in Shape::ALL {
            if shape == Shape::Submodule {
                continue;
            }
            let a = root.join(format!("a/{}", shape.name()));
            let b = root.join(format!("b/{}", shape.name()));
            build(shape, &a, &home).unwrap_or_else(|e| panic!("build {} (a): {e:#}", shape.name()));
            build(shape, &b, &home).unwrap_or_else(|e| panic!("build {} (b): {e:#}", shape.name()));
            let da = digest(&a, &home).unwrap();
            let db = digest(&b, &home).unwrap();
            if da != db {
                let first = da
                    .lines()
                    .zip(db.lines())
                    .find(|(x, y)| x != y)
                    .map(|(x, y)| format!("\n  a: {x}\n  b: {y}"))
                    .unwrap_or_else(|| "\n  (line counts differ)".to_string());
                panic!("shape {} is not reproducible:{first}", shape.name());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
