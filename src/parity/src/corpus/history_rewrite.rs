//! Differential corpus cases for the history_rewrite subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Commands covered: `cherry`, `cherry-pick`, `revert`, `rebase`, `replay`,
//! `filter-branch`, `subtree`, `replace`, `notes`.
//!
//! These are the verbs that *rewrite* history rather than report on it, so the
//! post-state probe carries most of the weight: a rewrite that prints the right
//! progress lines and produces different commit ids is a failure, and only the
//! `for-each-ref` / `rev-parse HEAD` / `cat-file --batch-all-objects` probes in
//! `runner::probe_state` can see it. The fixture pins `GIT_AUTHOR_DATE` and
//! `GIT_COMMITTER_DATE` (`env::FIXED_DATE`), so rewritten commit ids are
//! reproducible and are a legitimate assertion target.
//!
//! ## Two limits on what can be measured from here
//!
//! **No per-case environment.** `runner::Case` carries only `cmd`, `args` and
//! `shape`; every invocation gets the one hardened environment from
//! `env::harden`, which pins `GIT_SEQUENCE_EDITOR=true`. `true` exits 0 without
//! touching the todo file, and the env var outranks `sequence.editor` in git's
//! own lookup order, so `-c sequence.editor=…` cannot override it either.
//! Interactive rebase is therefore only reachable with an *unedited* todo —
//! every line stays `pick`. The `reword` / `edit` / `squash` / `fixup` / `break`
//! / `drop` instructions, and `--edit-todo` on a live rebase, cannot be driven
//! from the corpus at all. `--exec` is the one todo verb reachable without an
//! editor, because it is inserted from argv, and it is used below. One case
//! probes the env-over-config precedence itself.
//!
//! **A copied fixture has a stale index stat cache.** `Templates::instantiate`
//! copies the repository file by file, which changes every inode, so
//! `git diff-index HEAD` reports modifications even though the content matches.
//! `git-subtree`'s `ensure_clean` runs exactly that check, so `subtree add`,
//! `merge`, `pull` and `--rejoin` refuse with "working tree has modifications"
//! on every shape. Both sides refuse identically, so the cases below pin the
//! refusal rather than the success path; `subtree split` needs no clean tree and
//! is exercised for real.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    cherry(out);
    cherry_pick(out);
    revert(out);
    rebase(out);
    replay(out);
    filter_branch(out);
    subtree(out);
    replace(out);
    notes(out);
}

/// `cherry` — equivalence-class reporting, read-only.
///
/// The interesting axis is patch-id equality: `-` for a commit whose patch is
/// already upstream, `+` for one that is not. `Merged` gets its own case because
/// a merge commit has no patch id and has to be skipped rather than crash.
fn cherry(out: &mut Vec<Case>) {
    read_only("cherry", &["cherry", "HEAD", "HEAD"], out);
    out.push(Case::new("cherry", &["cherry", "main", "feature"], Shape::Branched));
    out.push(Case::new("cherry", &["cherry", "-v", "main", "feature"], Shape::Branched));
    // Upstream only: the head defaults to HEAD.
    out.push(Case::new("cherry", &["cherry", "main"], Shape::Branched));
    // Third argument bounds the range from below.
    out.push(Case::new("cherry", &["cherry", "-v", "main", "feature", "main~1"], Shape::Branched));
    out.push(Case::new("cherry", &["cherry", "--abbrev=8", "-v", "main", "feature"], Shape::Branched));
    out.push(Case::new("cherry", &["cherry", "main", "side"], Shape::Merged));
    out.push(Case::new("cherry", &["cherry", "does-not-exist"], Shape::Linear));
}

/// `cherry-pick` — replay one commit, and the sequencer state it leaves behind.
fn cherry_pick(out: &mut Vec<Case>) {
    out.push(Case::new("cherry-pick", &["cherry-pick", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "-n", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "-x", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "-n", "-x", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "-s", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--ff", "feature"], Shape::Branched));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--allow-empty", "feature"], Shape::Branched));
    // A named strategy goes to `merge-<name>` as a child (sequencer.c's
    // `try_merge_command`), which interpolates `-X` unquoted — so read-tree sees
    // `--theirs` as an option and rejects it before looking at a tree. The port
    // used to refuse with its own two-merge-base message and a different exit
    // code, i.e. the right failure for the wrong reason.
    out.push(Case::new(
        "cherry-pick",
        &["cherry-pick", "--strategy=resolve", "-Xtheirs", "feature"],
        Shape::Branched,
    ));
    // A merge commit needs an explicit mainline; without one it must be refused.
    out.push(Case::new("cherry-pick", &["cherry-pick", "-m", "1", "HEAD"], Shape::Merged));
    out.push(Case::new("cherry-pick", &["cherry-pick", "HEAD"], Shape::Merged));
    // Redundant pick of the tip: empty result, kept only with the explicit flag.
    out.push(Case::new(
        "cherry-pick",
        &["cherry-pick", "--allow-empty", "--keep-redundant-commits", "HEAD"],
        Shape::Linear,
    ));

    // Error paths. Each of these must agree on the exit code *and* leave the
    // repository untouched — a refusal that half-applies is the worse bug.
    out.push(Case::new(
        "cherry-pick",
        &["cherry-pick", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Linear,
    ));
    out.push(Case::new("cherry-pick", &["cherry-pick", "HEAD"], Shape::Dirty));
    // Mid-merge: the sequencer must refuse to start on top of an unmerged index.
    out.push(Case::new("cherry-pick", &["cherry-pick", "theirs"], Shape::Conflicted));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--continue"], Shape::Linear));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--abort"], Shape::Linear));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--quit"], Shape::Linear));
    out.push(Case::new("cherry-pick", &["cherry-pick", "--skip"], Shape::Linear));
}

/// `revert` — the inverse replay, sharing the sequencer with `cherry-pick`.
///
/// `--no-edit` is spelled out on the committing cases so the result does not
/// depend on `GIT_EDITOR` being a no-op.
fn revert(out: &mut Vec<Case>) {
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD"], Shape::Linear));
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD"], Shape::Branched));
    out.push(Case::new("revert", &["revert", "-n", "--no-edit", "HEAD"], Shape::Branched));
    out.push(Case::new("revert", &["revert", "-s", "--no-edit", "HEAD"], Shape::Branched));
    // Two revs in one invocation: the sequencer runs a two-entry todo.
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD", "HEAD~1"], Shape::Branched));
    // Reverting the root commit deletes files that later commits modified, so
    // this is the modify/delete conflict path rather than a clean revert.
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD~1"], Shape::Branched));
    out.push(Case::new("revert", &["revert", "-n", "--no-edit", "HEAD~1"], Shape::Branched));
    // Merge commits: no mainline is an error, either mainline is a real revert.
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD"], Shape::Merged));
    out.push(Case::new("revert", &["revert", "-m", "1", "--no-edit", "HEAD"], Shape::Merged));
    out.push(Case::new("revert", &["revert", "-m", "2", "--no-edit", "HEAD"], Shape::Merged));

    // Error paths.
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD"], Shape::Dirty));
    out.push(Case::new("revert", &["revert", "--no-edit", "HEAD"], Shape::Conflicted));
    out.push(Case::new(
        "revert",
        &["revert", "--no-edit", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Linear,
    ));
    out.push(Case::new("revert", &["revert", "--continue"], Shape::Linear));
    out.push(Case::new("revert", &["revert", "--abort"], Shape::Linear));
    out.push(Case::new("revert", &["revert", "--quit"], Shape::Linear));
}

/// `rebase` — the largest surface here, and the one whose result is invisible
/// without the state probe: a rebase that prints "Successfully rebased" and
/// writes different commit ids passes every stdout comparison.
fn rebase(out: &mut Vec<Case>) {
    // ---- non-interactive backend ----
    out.push(Case::new("rebase", &["rebase", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "feature"], Shape::Branched));
    // Two-argument form: rebase a branch that is *not* checked out. git checks
    // it out first; anything else leaves HEAD somewhere git would not.
    out.push(Case::new("rebase", &["rebase", "main", "feature"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--onto", "main", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--onto", "main~1", "main~1", "main"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--onto", "feature", "main"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--root"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "-f", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--no-ff", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--merge", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--keep-empty", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--empty=drop", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--autosquash", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--no-autosquash", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--update-refs", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--quiet", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--verbose", "HEAD~1"], Shape::Branched));
    // Date and trailer rewriting change the resulting commit ids, so these are
    // pure state assertions.
    out.push(Case::new("rebase", &["rebase", "--committer-date-is-author-date", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--ignore-date", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "--signoff", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "main"], Shape::Detached));

    // ---- ranges that span a merge commit ----
    // The non-interactive todo must flatten history: a merge commit is not a
    // `pick` candidate. A generator that emits one produces a todo it then has
    // to reject.
    out.push(Case::new("rebase", &["rebase", "main~1"], Shape::Merged));
    out.push(Case::new("rebase", &["rebase", "--onto", "main~2", "main~1"], Shape::Merged));
    out.push(Case::new("rebase", &["rebase", "--onto", "main~1", "main~2", "main"], Shape::Merged));
    out.push(Case::new("rebase", &["rebase", "--rebase-merges", "main~1"], Shape::Merged));

    // ---- interactive sequencer, driven without an editor ----
    // `GIT_SEQUENCE_EDITOR=true` leaves the generated todo untouched, so these
    // exercise the `.git/rebase-merge` machinery with an all-`pick` todo.
    out.push(Case::new("rebase", &["rebase", "-i", "HEAD~1"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "-i", "--root"], Shape::Branched));
    out.push(Case::new("rebase", &["rebase", "-i", "--autosquash", "HEAD~1"], Shape::Branched));
    // `--exec` is the only todo instruction insertable from argv.
    out.push(Case::new("rebase", &["rebase", "-i", "--exec", "true", "HEAD~1"], Shape::Branched));
    out.push(Case::new(
        "rebase",
        &["rebase", "-i", "--exec", "git status --porcelain", "HEAD~1"],
        Shape::Branched,
    ));
    // A failing exec stops the rebase mid-flight: the assertion is the
    // half-finished `.git/rebase-merge` state, not the output.
    out.push(Case::new("rebase", &["rebase", "-i", "--exec", "false", "HEAD~1"], Shape::Branched));
    // Precedence probe: `GIT_SEQUENCE_EDITOR` outranks `sequence.editor`, so
    // stock ignores this `-c` entirely and rebases with an unedited todo. An
    // implementation that reads the config first would rewrite different
    // history here.
    out.push(Case::new(
        "rebase",
        &["-c", "sequence.editor=false", "rebase", "-i", "HEAD~1"],
        Shape::Branched,
    ));

    // ---- error paths ----
    out.push(Case::new("rebase", &["rebase", "does-not-exist"], Shape::Linear));
    out.push(Case::new("rebase", &["rebase", "theirs"], Shape::Conflicted));
    out.push(Case::new("rebase", &["rebase", "--continue"], Shape::Linear));
    out.push(Case::new("rebase", &["rebase", "--abort"], Shape::Linear));
    out.push(Case::new("rebase", &["rebase", "--skip"], Shape::Linear));
    out.push(Case::new("rebase", &["rebase", "--edit-todo"], Shape::Linear));
}

/// `replay` — the bare-repo-capable replay engine. It touches neither index nor
/// worktree, so every case is judged on refs: either the atomic ref update it
/// performs by default, or the `update <ref> <new> <old>` lines it prints under
/// `--ref-action=print`.
fn replay(out: &mut Vec<Case>) {
    out.push(Case::new("replay", &["replay", "--onto", "main~1", "main..feature"], Shape::Branched));
    out.push(Case::new(
        "replay",
        &["replay", "--ref-action=print", "--onto", "main~1", "main..feature"],
        Shape::Branched,
    ));
    out.push(Case::new("replay", &["replay", "--advance", "main", "main..feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--contained", "--onto", "main~1", "main..feature"], Shape::Branched));
    // A whole-branch range replayed onto its own descendant: reaches the root
    // commit, which has no parent to rewrite.
    out.push(Case::new("replay", &["replay", "--onto", "main", "feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--onto", "main~2", "main~1..main"], Shape::Merged));
    out.push(Case::new("replay", &["replay", "--advance", "side", "side..main"], Shape::Merged));

    // Error paths: no range, unresolvable onto, no mode selector.
    out.push(Case::new("replay", &["replay", "--onto", "main~1"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "--onto", "does-not-exist", "main..feature"], Shape::Branched));
    out.push(Case::new("replay", &["replay", "main..feature"], Shape::Branched));
}

/// `filter-branch` — a port of the stock shell script, claimed to reproduce
/// stock's commit ids exactly. That claim is only testable through the state
/// probe, so the cases that matter are the ones that actually rewrite.
///
/// Note on cost and flakiness: the script sleeps ten seconds printing its
/// deprecation banner unless `FILTER_BRANCH_SQUELCH_WARNING` is set, which the
/// corpus cannot set (no per-case env). It also prints
/// `Rewrite <sha> (i/n) (N seconds passed, remaining N predicted)` to stdout,
/// where `N` is wall-clock elapsed time — stock does not always reproduce its
/// own stdout, and the runner's stock-versus-stock re-check is what keeps that
/// from being scored as a zvcs failure. Case count is kept low for both reasons.
fn filter_branch(out: &mut Vec<Case>) {
    // Identity filters: the rewrite must be a no-op and say so.
    out.push(Case::new("filter-branch", &["filter-branch", "-f", "--msg-filter", "cat", "HEAD"], Shape::Branched));
    out.push(Case::new("filter-branch", &["filter-branch", "-f", "--tree-filter", "true", "HEAD"], Shape::Branched));
    out.push(Case::new("filter-branch", &["filter-branch", "-f", "--index-filter", "true", "HEAD"], Shape::Branched));
    out.push(Case::new("filter-branch", &["filter-branch", "-f", "--msg-filter", "cat", "HEAD"], Shape::Merged));
    // Real rewrites: new commit ids on both sides, and they must be the same.
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "-f", "--env-filter", "export GIT_AUTHOR_NAME=other", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "-f", "--subdirectory-filter", "src", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new("filter-branch", &["filter-branch", "-f", "--prune-empty", "HEAD"], Shape::Branched));
    // `-- --all` rewrites branches and tags together.
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "-f", "--tag-name-filter", "cat", "--", "--all"],
        Shape::Branched,
    ));

    // Error paths: no filter at all, and a prefix that does not exist.
    out.push(Case::new("filter-branch", &["filter-branch", "HEAD"], Shape::Linear));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "-f", "--subdirectory-filter", "nosuch", "HEAD"],
        Shape::Branched,
    ));
}

/// `subtree` — also a port of the stock shell script.
///
/// `split` is the load-bearing case: it synthesizes a standalone history from a
/// prefix, and the synthesized commit ids are compared directly. `add`, `merge`,
/// `pull` and `--rejoin` all call `ensure_clean`, which a copied fixture always
/// fails (see the module comment), so those cases pin the refusal.
fn subtree(out: &mut Vec<Case>) {
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src", "--branch=srcbr"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src", "--annotate=[sub] "], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src", "HEAD~1"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src"], Shape::Merged));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=nested"], Shape::AwkwardPaths));

    // Clean-tree gate.
    out.push(Case::new("subtree", &["subtree", "add", "--prefix=vendor", "feature"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "merge", "--prefix=src", "feature"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=src", "--rejoin"], Shape::Branched));

    // Error paths: missing prefix, unknown prefix, unknown subcommand, and a
    // remote that does not resolve.
    out.push(Case::new("subtree", &["subtree", "split"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "split", "--prefix=nosuch"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "bogus", "--prefix=src"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "pull", "--prefix=src", "origin", "main"], Shape::Branched));
    out.push(Case::new("subtree", &["subtree", "push", "--prefix=src", "origin", "main"], Shape::Branched));
}

/// `replace` — grafting by ref. The visible effect is one ref under
/// `refs/replace/`, which the `for-each-ref` probe reports.
fn replace(out: &mut Vec<Case>) {
    read_only("replace", &["replace", "--list"], out);
    out.push(Case::new("replace", &["replace", "-l"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--format=long", "--list"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "HEAD", "HEAD~1"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "-f", "HEAD", "HEAD~1"], Shape::Branched));
    // `--graft` rewrites the parent list, so it creates a *new object* as well
    // as the replace ref.
    out.push(Case::new("replace", &["replace", "--graft", "HEAD"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--graft", "HEAD", "main~1"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--graft", "HEAD", "HEAD^1"], Shape::Merged));
    out.push(Case::new("replace", &["replace", "--edit", "HEAD"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--convert-graft-file"], Shape::Branched));

    // Error paths.
    out.push(Case::new("replace", &["replace", "-d", "HEAD"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "does-not-exist", "HEAD"], Shape::Linear));
    out.push(Case::new("replace", &["replace", "HEAD"], Shape::Linear));
}

/// `notes` — a second commit-shaped ref namespace, `refs/notes/commits`, whose
/// tree the object probe sees.
fn notes(out: &mut Vec<Case>) {
    out.push(Case::new("notes", &["notes", "add", "-m", "hello", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "add", "-m", "hello", "HEAD"], Shape::Branched));
    // Repeated `-m` concatenates with a blank line between paragraphs.
    out.push(Case::new("notes", &["notes", "add", "-m", "a", "-m", "b", "HEAD"], Shape::Branched));
    out.push(Case::new("notes", &["notes", "add", "-f", "-m", "over", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "add", "-C", "HEAD", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "add", "-m", "one", "HEAD~1"], Shape::Branched));
    // `append` on an object with no note is a create.
    out.push(Case::new("notes", &["notes", "append", "-m", "more", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "copy", "HEAD~1", "HEAD"], Shape::Branched));
    read_only("notes", &["notes", "list"], out);
    out.push(Case::new("notes", &["notes", "list", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "get-ref"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "prune"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "prune", "-n"], Shape::Linear));
    // A non-default notes ref must be created and reported under its own name.
    out.push(Case::new("notes", &["notes", "--ref=custom", "add", "-m", "z", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "--ref=custom", "list"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "--ref=refs/notes/x", "append", "-m", "t", "HEAD"], Shape::Branched));
    // `edit` with no `-m`: `GIT_EDITOR=true` supplies an empty message, which
    // git treats as "remove the note".
    out.push(Case::new("notes", &["notes", "edit", "HEAD"], Shape::Linear));

    // Error paths.
    read_only("notes", &["notes", "show", "HEAD"], out);
    out.push(Case::new("notes", &["notes", "show"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "remove", "HEAD"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "remove", "--ignore-missing", "HEAD"], Shape::Linear));
    // An object id that resolves to nothing: `notes show` reports a missing
    // note, it does not fail to parse the argument.
    out.push(Case::new(
        "notes",
        &["notes", "show", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Linear,
    ));
    out.push(Case::new("notes", &["notes", "add", "-m", "x", "does-not-exist"], Shape::Linear));
    out.push(Case::new("notes", &["notes", "merge", "--abort"], Shape::Linear));
    out.push(Case::new("notes", &["notes"], Shape::Linear));
}
