//! Cases for the nine shapes added in the second wave, each closing a gap a
//! corpus module recorded as unreachable.
//!
//! Grouped by shape, for the reason [`super::fixture_gaps`] is: the shape *is*
//! what is under test. Every block below asks something no case could ask
//! before, because no fixture carried an intent-to-add entry, a rename that had
//! not been committed, a note, a replacement, a hook that refuses, a recorded
//! rerere resolution, a locked worktree, a tag pointing at a tag, a `shallow`
//! file, or an object that is absent on purpose.
//!
//! What each shape supplies, so a reader does not have to rebuild it from
//! `fixture.rs`:
//!
//! * `IntentToAdd` — `ita-new.txt` and `sub/ita-nested.txt` recorded by
//!   `add -N` with content on disk, `ita-gone.txt` recorded and then deleted,
//!   `both.txt` staged for real and then edited (`AM`), plus `staged.txt`,
//!   `tracked.txt` and `untracked.txt`.
//! * `PendingRename` — staged renames `pure.txt` to `pure-renamed.txt` and
//!   `pkg/deep.txt` to `pkg/deep-renamed.txt` (`R100`), `near.txt` to
//!   `near-renamed.txt` staged then re-edited (`2 RM`), `far.txt` to
//!   `far-renamed.txt` (`R060`), `wild.txt` to `wild-renamed.txt` (`R039`), the
//!   unstaged `wt.txt` to `wt-renamed.txt` through an intent-to-add (`2 .R`),
//!   and the copy `copy.txt` to `copy-two.txt`.
//! * `NotesReplace` — `refs/notes/commits`, `refs/notes/review` and
//!   `refs/notes/other` (which collides with the first on `HEAD`), plus
//!   `refs/replace/*` for `notes: commit 1` (a commit with a different message)
//!   and for `README.md`'s blob.
//! * `HooksFail` — `pre-commit`, `pre-push`, `pre-rebase` and `pre-auto-gc` that
//!   exit 1; `prepare-commit-msg` and `commit-msg` that rewrite the message;
//!   `pre-merge-commit`, `post-merge`, `post-commit` (exits 1, ignored),
//!   `post-checkout` and `post-rewrite` that record their arguments; a peer at
//!   `.remote.git` whose `update` hook refuses `refs/heads/veto`; `main` one
//!   commit ahead of `origin/main`; the mergeable branch `hf-side`; and a dirty
//!   `side-base.txt`.
//! * `Rerere` — mid-merge with `rr.txt` and `other.txt` resolved from
//!   `.git/rr-cache` and `fresh.txt` still conflicted, `.git/MERGE_RR` naming
//!   the one unresolved path, `rerere.enabled` in the repository config.
//! * `WorktreeLocked` — `wt` locked with a reason, `wt-open` unlocked,
//!   `wt-gone` registered with its directory deleted.
//! * `TagChain` — `outermost` to `outer` to `inner` to a commit, `light-to-tag`
//!   at the same tag object, `blobtag` on a blob, `treetag` on a tree, two
//!   commits past `inner`.
//! * `Shallow` — `.git/shallow` two commits below the tip, `origin` at
//!   `./.remote.git` holding the rest, `sh-side` shallow beside `main`.
//! * `Promisor` — a partial clone with promisor packs and three of `hist.txt`'s
//!   four blobs missing, `origin` at `./.remote.git` able to supply them.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    intent_to_add(out);
    pending_rename(out);
    notes_and_replace(out);
    hooks_that_refuse(out);
    rerere_state(out);
    worktree_lock(out);
    tag_chain(out);
    shallow(out);
    promisor(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// Intent to add
// ---------------------------------------------------------------------------

/// The third state between tracked and untracked.
///
/// `add -N` records a path with the *empty* blob and a flag, so the entry
/// exists and its content does not. Several commands branch on that flag and
/// none of them could be asked about it: `status` renders ` A` where a staged
/// add is `A ` and a staged-then-edited add is `AM`, `diff` treats the entry as
/// a worktree addition while `diff --cached` hides it, and the two `--ita-*`
/// flags exist only to move that line. Measured on stock 2.55.0 over this
/// shape, `diff --cached --name-status` prints two paths and
/// `diff --cached --ita-visible-in-index --name-status` prints five — the
/// difference is the whole feature, and no fixture could produce it.
fn intent_to_add(out: &mut Vec<Case>) {
    each(
        Shape::IntentToAdd,
        "status",
        &[
            &["status", "--porcelain=v1"],
            &["status", "--porcelain=v1", "--untracked-files=all"],
            &["status", "--porcelain=v1", "--untracked-files=no"],
            &["status", "--porcelain=v2"],
            &["status", "--porcelain=v2", "--branch"],
            &["status", "--porcelain=v2", "--untracked-files=all"],
            &["status", "--short"],
            &["status", "--short", "--branch"],
            &["status", "--long"],
            &["status", "--porcelain=v2", "--ignored"],
            &["status", "--porcelain=v1", "--", "ita-new.txt"],
            &["status", "--porcelain=v2", "--", "sub"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "diff",
        &[
            &["diff", "--name-status"],
            &["diff", "--raw"],
            &["diff", "--stat"],
            &["diff", "--numstat"],
            &["diff", "--summary"],
            &["diff", "--name-only"],
            // The two flags that exist only for this entry class.
            &["diff", "--ita-invisible-in-index", "--name-status"],
            &["diff", "--ita-visible-in-index", "--name-status"],
            &["diff", "--ita-invisible-in-index", "--raw"],
            &["diff", "--cached", "--name-status"],
            &["diff", "--cached", "--ita-visible-in-index", "--name-status"],
            &["diff", "--cached", "--ita-invisible-in-index", "--name-status"],
            &["diff", "--cached", "--ita-visible-in-index", "--stat"],
            &["diff", "--cached", "--stat"],
            &["diff", "HEAD", "--name-status"],
            &["diff", "HEAD", "--stat"],
            &["diff", "HEAD", "--ita-visible-in-index", "--name-status"],
            &["diff", "--exit-code", "--quiet", "--cached"],
            &["diff", "--exit-code", "--quiet"],
            &["diff", "--name-status", "--", "ita-new.txt"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "diff-files",
        &[
            &["diff-files"],
            &["diff-files", "-p"],
            &["diff-files", "--ita-invisible-in-index"],
            &["diff-files", "--abbrev"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "diff-index",
        &[
            &["diff-index", "HEAD"],
            &["diff-index", "--cached", "HEAD"],
            &["diff-index", "--cached", "--ita-visible-in-index", "HEAD"],
            &["diff-index", "--cached", "--ita-invisible-in-index", "HEAD"],
            &["diff-index", "-p", "--cached", "HEAD"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "ls-files",
        &[
            &["ls-files", "--stage"],
            &["ls-files", "-v"],
            &["ls-files", "-t"],
            &["ls-files", "--cached"],
            &["ls-files", "--modified"],
            &["ls-files", "--deleted"],
            &["ls-files", "--others"],
            &["ls-files", "--format=%(objectmode) %(objectname) %(path)"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "commit",
        &[
            &["commit", "-m", "ita: everything staged"],
            &["commit", "-a", "-m", "ita: commit -a"],
            // A path that has an entry and no content.
            &["commit", "-m", "ita: only the ita path", "--", "ita-new.txt"],
            &["commit", "-m", "ita: only the tracked path", "--", "tracked.txt"],
            &["commit", "-m", "ita: the deleted ita path", "--", "ita-gone.txt"],
            &["commit", "--dry-run"],
            &["commit", "--dry-run", "--short"],
            &["commit", "--dry-run", "--porcelain"],
            &["commit", "--dry-run", "--long"],
            &["commit", "--allow-empty", "-m", "ita: allow empty"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "stash",
        &[
            &["stash", "push"],
            &["stash", "push", "-u"],
            &["stash", "push", "-k"],
            &["stash", "push", "-m", "ita stash", "--", "ita-new.txt"],
            &["stash", "push", "--staged"],
            &["stash", "list"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "add",
        &[
            &["add", "."],
            &["add", "-A"],
            &["add", "-N", "."],
            &["add", "-N", "untracked.txt"],
            &["add", "-u"],
            &["add", "--refresh", "."],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "rm",
        &[
            &["rm", "--cached", "ita-new.txt"],
            &["rm", "-f", "ita-new.txt"],
            &["rm", "ita-new.txt"],
            &["rm", "--cached", "ita-gone.txt"],
            &["rm", "-r", "--cached", "sub"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "restore",
        &[
            &["restore", "--staged", "ita-new.txt"],
            &["restore", "ita-new.txt"],
            &["restore", "--staged", "--worktree", "ita-new.txt"],
            &["restore", "--source=HEAD", "--staged", "ita-gone.txt"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "reset",
        &[
            &["reset"],
            &["reset", "--", "ita-new.txt"],
            &["reset", "--hard"],
            &["reset", "--mixed", "HEAD"],
            &["reset", "--keep", "HEAD"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "checkout",
        &[
            &["checkout", "--", "ita-new.txt"],
            &["checkout", "HEAD", "--", "ita-new.txt"],
            &["checkout", "-f", "HEAD"],
        ],
        out,
    );

    each(
        Shape::IntentToAdd,
        "write-tree",
        &[&["write-tree"], &["write-tree", "--missing-ok"]],
        out,
    );

    each(
        Shape::IntentToAdd,
        "grep",
        &[&["grep", "ita"], &["grep", "--cached", "ita"], &["grep", "--untracked", "ita"]],
        out,
    );

    each(
        Shape::IntentToAdd,
        "update-index",
        &[&["update-index", "--refresh"], &["update-index", "-q", "--refresh"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// A rename that has not been committed
// ---------------------------------------------------------------------------

/// The `2` record, and the thresholds that decide whether it is printed.
///
/// [`Shape::Renamed`] put its renames in history, so `status` has never had a
/// candidate pair and the `2` record type — half of `--porcelain=v2`'s grammar
/// — has never been produced by this corpus. The threshold sweep is the point
/// of the pairs: on stock 2.55.0, `far.txt` measures `R060` and `wild.txt`
/// `R039`, so `-M30` reports both as renames, the default `-M50` reports one,
/// and `-M70` reports neither. A port that hard-codes a similarity, or that
/// ignores the flag, agrees with exactly one of those three.
fn pending_rename(out: &mut Vec<Case>) {
    each(
        Shape::PendingRename,
        "status",
        &[
            &["status", "--porcelain=v2"],
            &["status", "--porcelain=v2", "--branch"],
            &["status", "--porcelain=v1"],
            &["status", "--short"],
            &["status", "--long"],
            &["status", "--porcelain=v2", "--no-renames"],
            &["status", "--porcelain=v2", "--renames"],
            &["status", "--porcelain=v2", "--find-renames=30"],
            &["status", "--porcelain=v2", "--find-renames=50"],
            &["status", "--porcelain=v2", "--find-renames=60"],
            &["status", "--porcelain=v2", "--find-renames=70"],
            &["status", "--porcelain=v2", "--find-renames=90"],
            &["status", "--porcelain=v1", "--find-renames=30"],
            &["status", "--short", "--no-renames"],
            &["status", "--porcelain=v2", "--", "pkg"],
        ],
        out,
    );
    // The same question asked through configuration rather than argv, which is
    // the half a flag-only corpus cannot separate.
    for (key, value) in
        [("status.renames", "false"), ("status.renames", "copies"), ("diff.renameLimit", "1")]
    {
        out.push(
            Case::new("status", &["status", "--porcelain=v2"], Shape::PendingRename)
                .with_config(&[(key, value)]),
        );
    }

    each(
        Shape::PendingRename,
        "diff",
        &[
            &["diff", "--cached", "--name-status"],
            &["diff", "--cached", "-M", "--name-status"],
            &["diff", "--cached", "--no-renames", "--name-status"],
            &["diff", "--cached", "-M30", "--name-status"],
            &["diff", "--cached", "-M50", "--name-status"],
            &["diff", "--cached", "-M60", "--name-status"],
            &["diff", "--cached", "-M70", "--name-status"],
            &["diff", "--cached", "--find-renames=39", "--name-status"],
            &["diff", "--cached", "--find-renames=40", "--name-status"],
            &["diff", "--cached", "-C", "--name-status"],
            &["diff", "--cached", "-C", "-C", "--name-status"],
            &["diff", "--cached", "-B", "-M", "--name-status"],
            &["diff", "--cached", "-M", "--summary"],
            &["diff", "--cached", "-M", "--stat"],
            &["diff", "--cached", "-M", "--raw"],
            &["diff", "--cached", "-M", "--numstat"],
            &["diff", "--cached", "-M", "-p"],
            &["diff", "--name-status"],
            &["diff", "-M", "--name-status"],
            &["diff", "HEAD", "-M", "--name-status"],
            &["diff", "HEAD", "-M", "--stat"],
            &["diff", "HEAD", "--find-renames=30", "--name-status"],
        ],
        out,
    );

    each(
        Shape::PendingRename,
        "diff-index",
        &[
            &["diff-index", "--cached", "-M", "HEAD"],
            &["diff-index", "--cached", "-M30", "HEAD"],
            &["diff-index", "--cached", "HEAD"],
            &["diff-index", "-M", "HEAD"],
        ],
        out,
    );

    each(
        Shape::PendingRename,
        "ls-files",
        &[
            &["ls-files", "--stage"],
            &["ls-files", "-v"],
            &["ls-files", "--deleted"],
            &["ls-files", "--others"],
        ],
        out,
    );

    each(
        Shape::PendingRename,
        "commit",
        &[
            &["commit", "-m", "pending-rename: commit the staged renames"],
            &["commit", "-a", "-m", "pending-rename: commit -a"],
            &["commit", "--dry-run", "--porcelain"],
            &["commit", "--dry-run", "--short"],
        ],
        out,
    );

    each(
        Shape::PendingRename,
        "mv",
        &[
            &["mv", "pure-renamed.txt", "pure-again.txt"],
            &["mv", "copy.txt", "copy-moved.txt"],
            &["mv", "wt-renamed.txt", "wt-again.txt"],
        ],
        out,
    );

    each(
        Shape::PendingRename,
        "reset",
        &[&["reset"], &["reset", "--hard"], &["reset", "--", "pure-renamed.txt"]],
        out,
    );

    each(
        Shape::PendingRename,
        "stash",
        &[&["stash", "push"], &["stash", "push", "-u"], &["stash", "push", "-k"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// Notes and replacements that already exist
// ---------------------------------------------------------------------------

/// Verbs that only change how an existing note or replacement is *read*.
///
/// A pristine fixture had neither, so `--notes=`, `--no-notes`,
/// `notes.displayRef`, `--no-replace-objects` and `GIT_NO_REPLACE_OBJECTS` each
/// selected between two empty answers and agreed by default. The replacement
/// half is the sharper one: a corpus agent established that the port writes
/// `refs/replace/*` correctly and never consults it when walking, and that is
/// invisible until an ordinary read verb runs over a repository that already
/// has one. Nothing here writes a replacement — the shape does — so what is
/// measured is the read.
fn notes_and_replace(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "notes",
        &[
            &["notes", "list"],
            &["notes", "list", "HEAD"],
            &["notes", "show", "HEAD"],
            &["notes", "show", "HEAD~1"],
            &["notes", "show", "HEAD~2"],
            &["notes", "get-ref"],
            &["notes", "--ref=review", "list"],
            &["notes", "--ref=review", "show", "HEAD"],
            &["notes", "--ref=other", "list"],
            &["notes", "add", "-f", "-m", "replaced note", "HEAD"],
            &["notes", "append", "-m", "appended note", "HEAD"],
            &["notes", "remove", "HEAD"],
            &["notes", "remove", "HEAD~2"],
            &["notes", "copy", "HEAD", "HEAD~2"],
            &["notes", "copy", "-f", "HEAD", "HEAD~1"],
            &["notes", "prune"],
            &["notes", "prune", "-n"],
            // The conflicting ref is what makes these more than a fast-forward.
            &["notes", "merge", "other"],
            &["notes", "merge", "-s", "ours", "other"],
            &["notes", "merge", "-s", "theirs", "other"],
            &["notes", "merge", "-s", "union", "other"],
            &["notes", "merge", "-s", "cat_sort_uniq", "other"],
            &["notes", "merge", "review"],
            &["notes", "merge", "--abort"],
            &["notes", "merge", "--commit"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "log",
        &[
            &["log", "--oneline"],
            &["log", "--notes", "--format=%s%n%N"],
            &["log", "--no-notes", "--format=%s%n%N"],
            &["log", "--notes=review", "--format=%s%n%N"],
            &["log", "--notes=other", "--format=%s%n%N"],
            &["log", "--notes=*", "--format=%s%n%N"],
            &["log", "--show-notes=review", "--oneline"],
            &["log", "-1", "--format=%N"],
            &["log", "-1", "--format=%(trailers)"],
            &["log", "--oneline", "--all"],
        ],
        out,
    );
    for value in ["refs/notes/review", "refs/notes/*", "refs/notes/nope"] {
        out.push(
            Case::new("log", &["log", "--notes", "--format=%s%n%N"], Shape::NotesReplace)
                .with_config(&[("notes.displayRef", value)]),
        );
    }
    out.push(Case::new("log", &["log", "--format=%s%n%N"], Shape::NotesReplace).with_config(&[
        ("notes.rewriteRef", "refs/notes/*"),
        ("core.notesRef", "refs/notes/review"),
    ]));

    each(
        Shape::NotesReplace,
        "show",
        &[
            &["show", "-s", "HEAD"],
            &["show", "-s", "--no-notes", "HEAD"],
            &["show", "-s", "--notes=review", "HEAD"],
            &["show", "HEAD:README.md"],
            &["show", "HEAD~2"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "cat-file",
        &[
            &["cat-file", "-p", "HEAD:README.md"],
            &["cat-file", "blob", "HEAD:README.md"],
            &["cat-file", "-s", "HEAD:README.md"],
            &["cat-file", "-t", "HEAD~2"],
            &["cat-file", "commit", "HEAD~2"],
            &["cat-file", "-p", "HEAD~2"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "replace",
        &[
            &["replace", "-l"],
            &["replace", "--list"],
            &["replace", "-l", "--format=medium"],
            &["replace", "-l", "--format=long"],
            &["replace", "-l", "--format=short"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "for-each-ref",
        &[
            &["for-each-ref", "--format=%(refname) %(objecttype)", "refs/replace"],
            &["for-each-ref", "--format=%(refname) %(objecttype)", "refs/notes"],
        ],
        out,
    );

    // The same reads with the substitution turned off, through the two doors
    // git offers: a global option and an environment variable. Both must move
    // the same answers, and a port that honours neither agrees with stock on
    // the default and on nothing else. The unflagged twins live in the blocks
    // above and below, so nothing here is pushed twice.
    for args in [
        &["log", "--oneline"][..],
        &["cat-file", "-p", "HEAD:README.md"][..],
        &["rev-list", "--format=%s", "-1", "HEAD~2"][..],
        &["show", "-s", "HEAD~2"][..],
        &["fsck"][..],
    ] {
        let cmd: &'static str = match args[0] {
            "log" => "log",
            "cat-file" => "cat-file",
            "rev-list" => "rev-list",
            "show" => "show",
            _ => "fsck",
        };
        out.push(
            Case::new(cmd, args, Shape::NotesReplace).with_globals(&[&["--no-replace-objects"]]),
        );
        out.push(
            Case::new(cmd, args, Shape::NotesReplace)
                .with_env(&[("GIT_NO_REPLACE_OBJECTS", "1")]),
        );
    }
    each(
        Shape::NotesReplace,
        "rev-list",
        &[&["rev-list", "--format=%s", "-1", "HEAD~2"], &["rev-list", "--count", "HEAD"]],
        out,
    );
    each(Shape::NotesReplace, "show", &[&["show", "-s", "HEAD~2"]], out);

    each(
        Shape::NotesReplace,
        "fsck",
        &[
            &["fsck"],
            &["fsck", "--no-progress"],
            &["fsck", "--unreachable"],
            &["fsck", "--connectivity-only"],
        ],
        out,
    );

    each(
        Shape::NotesReplace,
        "gc",
        &[&["gc", "--prune=now"], &["gc", "--aggressive", "--prune=now"]],
        out,
    );

    each(Shape::NotesReplace, "prune", &[&["prune"], &["prune", "-v", "-n"]], out);
}

// ---------------------------------------------------------------------------
// Hooks that refuse
// ---------------------------------------------------------------------------

/// The other half of [`Shape::Hooked`]: what happens when a hook says no.
///
/// A corpus agent recorded that `--no-verify` could not be measured, and the
/// reason is structural rather than an oversight — with every hook exiting 0,
/// running them and skipping them produce the same commit. The pairs below are
/// the measurement: `commit -am` against `commit --no-verify -am`, `push`
/// against `push --no-verify`, each over a repository where the hook refuses.
///
/// `prepare-commit-msg` is the control. Stock does **not** skip it for
/// `--no-verify` (verified on 2.55.0: after `commit --no-verify` the worktree
/// has `hook-prepare-commit-msg.txt` and not `hook-commit-msg.txt`), so a port
/// that treats `--no-verify` as "run no hooks" writes a different commit
/// message and is caught by the object id even though its stdout matches.
///
/// `push origin veto` is the refusal `--no-verify` cannot reach: the hook that
/// rejects it runs in the receiving repository, which this side does not
/// control.
fn hooks_that_refuse(out: &mut Vec<Case>) {
    each(
        Shape::HooksFail,
        "commit",
        &[
            &["commit", "-am", "hooks: plain"],
            &["commit", "--no-verify", "-am", "hooks: no verify"],
            &["commit", "-n", "-am", "hooks: short no verify"],
            &["commit", "--amend", "--no-edit"],
            &["commit", "--amend", "--no-edit", "--no-verify"],
            &["commit", "--allow-empty", "-m", "hooks: empty"],
            &["commit", "--allow-empty", "--no-verify", "-m", "hooks: empty no verify"],
            &["commit", "--dry-run"],
            &["commit", "-am", "hooks: cleanup verbatim", "--cleanup=verbatim"],
            &["commit", "--no-verify", "-am", "hooks: cleanup verbatim", "--cleanup=verbatim"],
        ],
        out,
    );

    each(
        Shape::HooksFail,
        "merge",
        &[
            &["merge", "--no-ff", "-m", "hooks: merge", "hf-side"],
            &["merge", "--no-ff", "--no-verify", "-m", "hooks: merge no verify", "hf-side"],
            &["merge", "--no-commit", "--no-ff", "hf-side"],
            &["merge", "--squash", "hf-side"],
            &["merge", "--abort"],
        ],
        out,
    );

    each(
        Shape::HooksFail,
        "push",
        &[
            &["push", "origin", "main"],
            &["push", "--no-verify", "origin", "main"],
            &["push", "-n", "origin", "main"],
            &["push", "--dry-run", "origin", "main"],
            // Refused by the *peer*, which `--no-verify` has no say over.
            &["push", "origin", "veto"],
            &["push", "--no-verify", "origin", "veto"],
            &["push", "--no-verify", "--atomic", "origin", "main", "veto"],
            &["push", "--no-verify", "origin", "main", "veto"],
            &["push", "--no-verify", "--porcelain", "origin", "main"],
        ],
        out,
    );

    each(
        Shape::HooksFail,
        "rebase",
        &[
            &["rebase", "hf-side"],
            &["rebase", "--onto", "hf-side", "main~1"],
            &["rebase", "--no-verify", "hf-side"],
            &["rebase", "--abort"],
        ],
        out,
    );

    each(
        Shape::HooksFail,
        "gc",
        &[&["gc", "--auto"], &["gc", "--auto", "--quiet"], &["gc", "--prune=now"]],
        out,
    );

    each(
        Shape::HooksFail,
        "checkout",
        &[
            &["checkout", "hf-side"],
            &["checkout", "-b", "hf-new"],
            &["checkout", "--", "side-base.txt"],
            &["checkout", "-f", "hf-side"],
        ],
        out,
    );

    each(Shape::HooksFail, "switch", &[&["switch", "hf-side"], &["switch", "-c", "hf-new"]], out);

    each(
        Shape::HooksFail,
        "cherry-pick",
        &[&["cherry-pick", "hf-side"], &["cherry-pick", "--no-commit", "hf-side"]],
        out,
    );

    each(
        Shape::HooksFail,
        "revert",
        &[&["revert", "--no-edit", "HEAD"], &["revert", "--no-commit", "HEAD"]],
        out,
    );

    each(
        Shape::HooksFail,
        "stash",
        &[&["stash", "push"], &["stash", "push", "-u"], &["stash", "list"]],
        out,
    );

    each(Shape::HooksFail, "status", &[&["status", "--porcelain=v2"], &["status", "--short"]], out);

    each(
        Shape::HooksFail,
        "pull",
        &[&["pull", "--no-rebase", "origin", "main"], &["pull", "--ff-only", "origin", "main"]],
        out,
    );

    // `core.hooksPath` pointed somewhere with nothing in it: the one argv that
    // turns every hook above off at once, and the only way to see that the
    // refusals are the hooks and not the verbs.
    for args in [&["commit", "-am", "hooks: elsewhere"][..], &["push", "origin", "main"][..]] {
        let cmd: &'static str = if args[0] == "commit" { "commit" } else { "push" };
        out.push(
            Case::new(cmd, args, Shape::HooksFail)
                .with_config(&[("core.hooksPath", "no-such-hooks")]),
        );
    }
}

// ---------------------------------------------------------------------------
// A recorded rerere resolution
// ---------------------------------------------------------------------------

/// `rerere` over a cache that already has something in it.
///
/// Every path here needs a *prior* resolution, and a case is one argv against a
/// pristine copy, so none of them could be reached: `rerere diff`, `rerere
/// status` and `rerere remaining` read `.git/MERGE_RR` and `.git/rr-cache`,
/// `rerere forget` needs a record to drop, and the replay — git writing an old
/// resolution back into the worktree — had never run anywhere in the corpus.
///
/// The shape is mid-merge with all three outcomes at once, so a single `status`
/// separates them: `rr.txt` and `other.txt` were replayed from the cache (the
/// worktree holds the recorded text while the index still has stages 1/2/3),
/// and `fresh.txt` has never been seen (markers, and a preimage only). Stock
/// 2.55.0 answers `fresh.txt` to both `rerere status` and `rerere remaining`
/// and prints a marker-to-marker diff for it under `rerere diff`.
fn rerere_state(out: &mut Vec<Case>) {
    each(
        Shape::Rerere,
        "rerere",
        &[
            &["rerere"],
            &["rerere", "status"],
            &["rerere", "remaining"],
            &["rerere", "diff"],
            &["rerere", "gc"],
            &["rerere", "clear"],
            &["rerere", "forget", "fresh.txt"],
            &["rerere", "forget", "rr.txt"],
            &["rerere", "forget", "other.txt"],
            &["rerere", "forget", "."],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "status",
        &[
            &["status", "--porcelain=v2"],
            &["status", "--porcelain=v1"],
            &["status", "--short"],
            &["status", "--long"],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "ls-files",
        &[
            &["ls-files", "-u"],
            &["ls-files", "--unmerged"],
            &["ls-files", "--stage"],
            &["ls-files", "-v"],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "diff",
        &[
            &["diff"],
            &["diff", "--name-only", "--diff-filter=U"],
            &["diff", "--cc"],
            &["diff", "--ours", "--name-only"],
            &["diff", "--theirs", "--name-only"],
            &["diff", "--base", "--name-only"],
            &["diff", "--stat"],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "cat-file",
        &[
            &["cat-file", "-p", ":1:fresh.txt"],
            &["cat-file", "-p", ":2:fresh.txt"],
            &["cat-file", "-p", ":3:fresh.txt"],
            &["cat-file", "-p", ":2:rr.txt"],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "checkout",
        &[
            &["checkout", "--conflict=diff3", "--", "fresh.txt"],
            &["checkout", "--conflict=merge", "--", "fresh.txt"],
            &["checkout", "-m", "--", "rr.txt"],
            &["checkout", "--ours", "--", "fresh.txt"],
            &["checkout", "--theirs", "--", "fresh.txt"],
        ],
        out,
    );

    each(
        Shape::Rerere,
        "merge",
        &[&["merge", "--abort"], &["merge", "--continue"], &["merge", "--quit"]],
        out,
    );

    each(
        Shape::Rerere,
        "commit",
        &[
            &["commit", "--no-edit"],
            &["commit", "-m", "rerere: forced"],
            &["commit", "-a", "--no-edit"],
        ],
        out,
    );

    each(Shape::Rerere, "add", &[&["add", "fresh.txt"], &["add", "-A"], &["add", "-u"]], out);

    each(
        Shape::Rerere,
        "restore",
        &[
            &["restore", "--staged", "fresh.txt"],
            &["restore", "--worktree", "--source=MERGE_HEAD", "fresh.txt"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// A locked worktree
// ---------------------------------------------------------------------------

/// The lock protocol, and the prunable registration.
///
/// [`Shape::Worktree`] has one ordinary linked worktree, and a case cannot lock
/// one first, so `worktree lock` was only ever measured on "not locked yet",
/// `worktree unlock` on "not locked", `worktree remove` on the path where
/// nothing objects, and `worktree prune` on a registry with nothing stale in
/// it. `worktree list --porcelain`'s `locked` and `prunable` lines had never
/// been printed by either side.
fn worktree_lock(out: &mut Vec<Case>) {
    each(
        Shape::WorktreeLocked,
        "worktree",
        &[
            &["worktree", "list"],
            &["worktree", "list", "--porcelain"],
            &["worktree", "list", "--porcelain", "-z"],
            &["worktree", "list", "-v"],
            &["worktree", "remove", "wt"],
            &["worktree", "remove", "--force", "wt"],
            &["worktree", "remove", "wt-open"],
            &["worktree", "remove", "wt-gone"],
            &["worktree", "unlock", "wt"],
            &["worktree", "unlock", "wt-open"],
            &["worktree", "lock", "wt"],
            &["worktree", "lock", "wt-open"],
            &["worktree", "lock", "--reason", "locked by the case", "wt-open"],
            &["worktree", "prune"],
            &["worktree", "prune", "-v"],
            &["worktree", "prune", "-n"],
            &["worktree", "prune", "-n", "-v"],
            &["worktree", "repair"],
            &["worktree", "move", "wt", "wt-moved"],
            &["worktree", "move", "wt-open", "wt-moved"],
            &["worktree", "add", "--relative-paths", "-b", "wt-extra", "wt2"],
            &["worktree", "add", "--detach", "wt2"],
        ],
        out,
    );

    each(
        Shape::WorktreeLocked,
        "branch",
        &[
            &["branch", "--list"],
            &["branch", "-a"],
            &["branch", "-vv"],
            &["branch", "-D", "wt-held"],
            &["branch", "-D", "wt-gone"],
        ],
        out,
    );

    each(
        Shape::WorktreeLocked,
        "checkout",
        &[
            &["checkout", "wt-held"],
            &["checkout", "wt-gone"],
            &["checkout", "--ignore-other-worktrees", "wt-held"],
        ],
        out,
    );

    each(Shape::WorktreeLocked, "switch", &[&["switch", "wt-held"], &["switch", "wt-open"]], out);

    each(
        Shape::WorktreeLocked,
        "gc",
        &[&["gc", "--prune=now"], &["gc", "--prune=now", "--aggressive"]],
        out,
    );

    each(Shape::WorktreeLocked, "fsck", &[&["fsck"], &["fsck", "--connectivity-only"]], out);

    // From inside the locked worktree: the discovery answers differ from the
    // main one, and this is the only shape where the worktree they come from is
    // locked.
    for args in [
        &["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"][..],
        &["status", "--porcelain=v2", "--branch"][..],
        &["worktree", "list", "--porcelain"][..],
    ] {
        let cmd: &'static str = match args[0] {
            "rev-parse" => "rev-parse",
            "status" => "status",
            _ => "worktree",
        };
        out.push(Case::new(cmd, args, Shape::WorktreeLocked).in_dir("wt"));
    }
}

// ---------------------------------------------------------------------------
// A tag chain
// ---------------------------------------------------------------------------

/// Peeling more than one level, and peeling to something that is not a commit.
///
/// Every tag in the corpus points straight at a commit, so an implementation
/// that peels exactly once scored the same as one that peels to the end, and a
/// tag whose target is a blob or a tree did not exist at all. On stock 2.55.0
/// over this shape `rev-parse outermost` and `rev-parse outermost^{}` differ by
/// three objects, and `show-ref -d` prints a `^{}` line for all six tags.
fn tag_chain(out: &mut Vec<Case>) {
    each(
        Shape::TagChain,
        "rev-parse",
        &[
            &["rev-parse", "outermost"],
            &["rev-parse", "outermost^{}"],
            &["rev-parse", "outermost^{commit}"],
            &["rev-parse", "outermost^{tree}"],
            &["rev-parse", "outermost^{tag}"],
            &["rev-parse", "outer^{}"],
            &["rev-parse", "light-to-tag^{}"],
            &["rev-parse", "blobtag^{}"],
            &["rev-parse", "blobtag^{blob}"],
            &["rev-parse", "treetag^{}"],
            &["rev-parse", "treetag^{tree}"],
            &["rev-parse", "--verify", "outermost^{commit}"],
            &["rev-parse", "--verify", "blobtag^{commit}"],
            &["rev-parse", "outermost^0"],
            &["rev-parse", "--tags"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "cat-file",
        &[
            &["cat-file", "-t", "outermost"],
            &["cat-file", "-t", "outermost^{}"],
            &["cat-file", "-p", "outermost"],
            &["cat-file", "tag", "outermost"],
            &["cat-file", "-t", "blobtag"],
            &["cat-file", "-p", "blobtag"],
            &["cat-file", "-p", "blobtag^{}"],
            &["cat-file", "-t", "treetag^{}"],
            &["cat-file", "-s", "outermost"],
            &["cat-file", "-p", "light-to-tag"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "describe",
        &[
            &["describe"],
            &["describe", "--tags"],
            &["describe", "--long"],
            &["describe", "--all"],
            &["describe", "--abbrev=0"],
            &["describe", "--contains", "HEAD"],
            &["describe", "HEAD~1"],
            &["describe", "--first-parent"],
            &["describe", "--match", "outer*"],
            &["describe", "--exclude", "inner"],
            &["describe", "outermost"],
            &["describe", "--always", "blobtag"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "show-ref",
        &[
            &["show-ref"],
            &["show-ref", "-d"],
            &["show-ref", "--tags", "-d"],
            &["show-ref", "--verify", "refs/tags/outermost"],
            &["show-ref", "--dereference", "outermost"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "for-each-ref",
        &[
            &[
                "for-each-ref",
                "--format=%(refname) %(objecttype) %(*objecttype) %(*objectname)",
                "refs/tags",
            ],
            &["for-each-ref", "--format=%(refname:short) %(contents:subject)", "refs/tags"],
            &["for-each-ref", "--format=%(refname) %(taggername) %(taggerdate:iso)", "refs/tags"],
            &["for-each-ref", "--points-at", "HEAD~2", "refs/tags"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "tag",
        &[
            &["tag", "-l"],
            &["tag", "-l", "-n1"],
            &["tag", "--format=%(refname:short) %(objecttype) %(*objecttype)"],
            &["tag", "-d", "outermost"],
            &["tag", "-d", "outer"],
            &["tag", "-d", "inner"],
            &["tag", "-d", "blobtag"],
            &["tag", "-d", "outermost", "outer", "inner"],
            &["tag", "--points-at", "HEAD~2"],
            &["tag", "--contains", "HEAD~2"],
            &["tag", "-v", "outermost"],
            &["tag", "-a", "-m", "one more", "over-outermost", "outermost"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "rev-list",
        &[
            &["rev-list", "--count", "outermost"],
            &["rev-list", "--objects", "outermost"],
            &["rev-list", "--count", "--tags"],
            &["rev-list", "--objects", "--all"],
            &["rev-list", "--max-parents=0", "--all"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "log",
        &[
            &["log", "--oneline", "outermost"],
            &["log", "--oneline", "--decorate", "--all"],
            &["log", "--oneline", "--decorate=full", "-1"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "show",
        &[
            &["show", "-s", "outermost"],
            &["show", "blobtag"],
            &["show", "treetag"],
            &["show", "light-to-tag"],
        ],
        out,
    );

    each(
        Shape::TagChain,
        "name-rev",
        &[&["name-rev", "--tags", "HEAD"], &["name-rev", "--all"]],
        out,
    );

    each(Shape::TagChain, "ls-remote", &[&["ls-remote", "."], &["ls-remote", "--tags", "."]], out);

    each(
        Shape::TagChain,
        "update-ref",
        &[&["update-ref", "-d", "refs/tags/outer"], &["update-ref", "-d", "refs/tags/outermost"]],
        out,
    );

    each(Shape::TagChain, "fsck", &[&["fsck"], &["fsck", "--unreachable"], &["fsck", "--strict"]], out);

    each(Shape::TagChain, "gc", &[&["gc", "--prune=now"]], out);
    each(Shape::TagChain, "prune", &[&["prune"], &["prune", "-n", "-v"]], out);
    each(Shape::TagChain, "archive", &[&["archive", "--format=tar", "outermost"]], out);
}

// ---------------------------------------------------------------------------
// A shallow clone
// ---------------------------------------------------------------------------

/// The grafted boundary, and the commands that move it.
///
/// No shape carried a `shallow` file and a case cannot create one, so
/// `--unshallow`, `--deepen`, `--depth` on an existing clone,
/// `rev-parse --is-shallow-repository` and every traversal that has to stop at
/// a graft had no repository to be true of. The peer is local
/// (`./.remote.git`, inside the fixture), so nothing here resolves a hostname:
/// the deepening cases talk to a directory.
fn shallow(out: &mut Vec<Case>) {
    each(
        Shape::Shallow,
        "rev-parse",
        &[
            &["rev-parse", "--is-shallow-repository"],
            &["rev-parse", "HEAD"],
            &["rev-parse", "HEAD~1"],
            &["rev-parse", "HEAD~2"],
            &["rev-parse", "--verify", "HEAD~2"],
        ],
        out,
    );

    each(
        Shape::Shallow,
        "log",
        &[
            &["log", "--oneline"],
            &["log", "--oneline", "--all"],
            &["log", "--oneline", "--graph", "--boundary"],
            &["log", "--format=%H %P"],
            &["log", "--oneline", "sh-side"],
        ],
        out,
    );

    each(
        Shape::Shallow,
        "rev-list",
        &[
            &["rev-list", "--count", "HEAD"],
            &["rev-list", "--count", "--all"],
            &["rev-list", "--max-parents=0", "HEAD"],
            &["rev-list", "--objects", "HEAD"],
            &["rev-list", "--children", "HEAD"],
        ],
        out,
    );

    each(
        Shape::Shallow,
        "fetch",
        &[
            &["fetch", "--unshallow", "origin"],
            &["fetch", "--unshallow", "--no-tags", "origin"],
            &["fetch", "--deepen=1", "origin"],
            &["fetch", "--deepen=2", "origin"],
            &["fetch", "--depth=1", "origin"],
            &["fetch", "--depth=3", "origin"],
            &["fetch", "--depth=99", "origin"],
            &["fetch", "origin"],
            &["fetch", "--all"],
            &["fetch", "--prune", "origin"],
        ],
        out,
    );

    each(
        Shape::Shallow,
        "pull",
        &[&["pull", "--no-rebase", "origin", "main"], &["pull", "--ff-only", "origin", "main"]],
        out,
    );

    each(
        Shape::Shallow,
        "fsck",
        &[&["fsck"], &["fsck", "--connectivity-only"], &["fsck", "--no-progress"]],
        out,
    );

    each(Shape::Shallow, "gc", &[&["gc", "--prune=now"], &["gc", "--aggressive", "--prune=now"]], out);

    each(Shape::Shallow, "repack", &[&["repack", "-a", "-d"], &["repack", "-A", "-d"]], out);

    each(Shape::Shallow, "prune", &[&["prune"], &["prune", "-n", "-v"]], out);

    each(
        Shape::Shallow,
        "clone",
        &[&["clone", "--no-local", ".", "sh-clone"], &["clone", ".", "sh-clone"]],
        out,
    );

    each(
        Shape::Shallow,
        "status",
        &[&["status", "--porcelain=v2", "--branch"], &["status", "--short", "--branch"]],
        out,
    );

    each(
        Shape::Shallow,
        "merge-base",
        &[&["merge-base", "HEAD", "origin/main"], &["merge-base", "--is-ancestor", "HEAD~1", "HEAD"]],
        out,
    );

    each(
        Shape::Shallow,
        "describe",
        &[&["describe", "--always"], &["describe", "--always", "--long"]],
        out,
    );

    each(
        Shape::Shallow,
        "bundle",
        &[&["bundle", "create", "sh.bundle", "--all"], &["bundle", "create", "sh.bundle", "HEAD"]],
        out,
    );

    each(Shape::Shallow, "archive", &[&["archive", "--format=tar", "HEAD"]], out);
    each(Shape::Shallow, "remote", &[&["remote", "-v"], &["remote", "show", "origin"]], out);
    each(Shape::Shallow, "ls-remote", &[&["ls-remote", "origin"]], out);
    each(Shape::Shallow, "count-objects", &[&["count-objects", "-v"]], out);
}

// ---------------------------------------------------------------------------
// A partial clone
// ---------------------------------------------------------------------------

/// Objects that are absent on purpose.
///
/// Every other shape has every object it references, and the one shape that
/// does not — [`Shape::Damaged`] — is missing them by *damage*, which is the
/// opposite condition: damage is an error and a promisor absence is not. So
/// `rev-list --missing=`, `--exclude-promisor-objects`,
/// `gc --exclude-promisor-objects`, `repack --filter`, `backfill` and the lazy
/// fetch itself had no repository to describe. On stock 2.55.0 over this shape
/// `rev-list --objects --all --missing=print` prints three `?`-prefixed ids,
/// and reading any of them makes git fetch it from the local peer rather than
/// fail.
fn promisor(out: &mut Vec<Case>) {
    each(
        Shape::Promisor,
        "rev-list",
        &[
            &["rev-list", "--objects", "--all", "--missing=print"],
            &["rev-list", "--objects", "--all", "--missing=allow-any"],
            &["rev-list", "--objects", "--all", "--missing=allow-promisor"],
            &["rev-list", "--objects", "--all", "--missing=error"],
            &["rev-list", "--objects", "--all", "--missing=print-info"],
            &["rev-list", "--objects", "--all", "--exclude-promisor-objects"],
            &["rev-list", "--all", "--count", "--exclude-promisor-objects"],
            &["rev-list", "--objects", "HEAD", "--missing=print"],
            &["rev-list", "--count", "--all"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "cat-file",
        &[
            // Reading a missing blob: git fetches it from the promisor remote.
            &["cat-file", "-p", "HEAD~3:hist.txt"],
            &["cat-file", "-s", "HEAD~3:hist.txt"],
            &["cat-file", "-t", "HEAD~3:hist.txt"],
            &["cat-file", "-p", "HEAD:hist.txt"],
            &["cat-file", "-e", "HEAD~3:hist.txt"],
        ],
        out,
    );
    // The same read with the lazy fetch forbidden, which is what separates
    // "absent and fetchable" from "absent".
    for args in [&["cat-file", "-p", "HEAD~3:hist.txt"][..], &["log", "-p", "--oneline"][..]] {
        let cmd: &'static str = if args[0] == "cat-file" { "cat-file" } else { "log" };
        out.push(Case::new(cmd, args, Shape::Promisor).with_globals(&[&["--no-lazy-fetch"]]));
    }

    each(
        Shape::Promisor,
        "log",
        &[
            &["log", "--oneline"],
            &["log", "--oneline", "--stat"],
            &["log", "-p", "--oneline"],
            &["log", "--oneline", "--", "hist.txt"],
            &["log", "--follow", "--oneline", "--", "hist.txt"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "gc",
        &[
            &["gc", "--prune=now"],
            &["gc", "--prune=now", "--aggressive"],
            &["gc", "--prune=now", "--keep-largest-pack"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "repack",
        &[
            &["repack", "-a", "-d"],
            &["repack", "-A", "-d"],
            &["repack", "-a", "-d", "--filter=blob:none"],
            &["repack", "-a", "-d", "--filter=blob:limit=1"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "fsck",
        &[&["fsck"], &["fsck", "--connectivity-only"], &["fsck", "--no-progress"]],
        out,
    );

    each(
        Shape::Promisor,
        "fetch",
        &[
            &["fetch", "origin"],
            &["fetch", "--refetch", "origin"],
            &["fetch", "--filter=blob:none", "origin"],
            &["fetch", "--no-filter", "origin"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "config",
        &[
            &["config", "--get", "remote.origin.promisor"],
            &["config", "--get", "remote.origin.partialclonefilter"],
            &["config", "--get", "extensions.partialclone"],
            &["config", "--list", "--local"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "checkout",
        &[&["checkout", "pc-side"], &["checkout", "HEAD~2", "--", "hist.txt"]],
        out,
    );

    each(Shape::Promisor, "switch", &[&["switch", "pc-side"]], out);

    each(
        Shape::Promisor,
        "blame",
        &[&["blame", "hist.txt"], &["blame", "--porcelain", "hist.txt"]],
        out,
    );

    each(
        Shape::Promisor,
        "diff",
        &[
            &["diff", "HEAD~3", "HEAD", "--stat"],
            &["diff", "HEAD~3", "HEAD"],
            &["diff", "--stat", "pc-side"],
        ],
        out,
    );

    each(
        Shape::Promisor,
        "clone",
        &[&["clone", "--no-local", ".", "pc-clone"], &["clone", ".", "pc-clone"]],
        out,
    );

    each(Shape::Promisor, "count-objects", &[&["count-objects", "-v"]], out);
    each(Shape::Promisor, "prune", &[&["prune"], &["prune", "-n", "-v"]], out);
    each(Shape::Promisor, "backfill", &[&["backfill"], &["backfill", "--min-batch-size=1"]], out);
    each(Shape::Promisor, "status", &[&["status", "--porcelain=v2", "--branch"]], out);
    each(Shape::Promisor, "archive", &[&["archive", "--format=tar", "HEAD~3"]], out);
}
