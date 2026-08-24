//! Differential cases for `reset`'s five modes and `checkout`'s index/worktree
//! modes — the two verbs that move {HEAD, index, worktree} independently.
//!
//! `reset` is the only porcelain verb that can destroy uncommitted work, and it
//! has five modes that each move a different pair of the three:
//!
//! | mode      | HEAD | index | worktree | local work                       |
//! |-----------|------|-------|----------|----------------------------------|
//! | `--soft`  | yes  | no    | no       | untouched; refused mid-merge     |
//! | `--mixed` | yes  | yes   | no       | index rewritten, worktree kept   |
//! | `--hard`  | yes  | yes   | yes      | discarded, unconditionally       |
//! | `--merge` | yes  | yes   | yes      | per path; carries or removes     |
//! | `--keep`  | yes  | yes   | yes      | per path; refuses to overwrite   |
//!
//! The last two rows are the whole reason this module exists. `--merge` and
//! `--keep` both go through `unpack_trees()`, both refuse with
//! `error: Entry '<p>' not uptodate. Cannot merge.` at exit 128, and both leave
//! the repository untouched when they do — so a port that implements one of
//! them and aliases the other passes on every shape whose index, worktree and
//! `HEAD` agree, which is most of them. Two fixtures separate them, and both
//! were run against stock git 2.55.0 before being written down:
//!
//! * [`Shape::Dirty`], where the difference is what happens to a *staged
//!   addition* the target tree does not have. `reset --merge` removes
//!   `staged.txt` from the index **and deletes the file**; `reset --keep`
//!   removes the index entry and leaves the file behind as untracked. Same
//!   exit code, same stdout (both silent), different worktree — which
//!   `status --porcelain` reports as the presence or absence of
//!   `?? staged.txt`.
//! * [`Shape::MergeableStaged`], where `keep.txt` is staged and the target tree
//!   changes it. `reset --keep HEAD~1` rewrites the index and leaves the
//!   worktree file alone (` M keep.txt`); `reset --merge HEAD~1` rewrites both
//!   and leaves a clean tree. Those two are [`Case::strict`] because they are
//!   otherwise indistinguishable on stdout and exit code.
//!
//! The refusal itself is reached from [`Shape::MergeableDirty`], where
//! `hot.txt` is edited in the worktree and `div-hot` rewrites it, and where
//! `ff-squat` wants to write over an untracked file — the second refusal class,
//! `error: Untracked working tree file 'squat.txt' would be overwritten by
//! merge.`, which comes from `verify_absent()` rather than `verify_uptodate()`.
//!
//! # What the probes can and cannot see
//!
//! `ls-files --stage` is the instrument here: it prints the stage number, so a
//! mixed reset (stage 0 rewritten), a hard reset (stage 0 plus worktree), a
//! `--merge` that collapsed the 1/2/3 entries and a `--keep` that refused are
//! four distinguishable post-states. `status --porcelain=v1 -uall` separates
//! index-side from worktree-side (`M ` vs ` M` vs `MM`), and the reflog probe
//! sees the `reset: moving to <rev>` line and `ORIG_HEAD` that every mode with
//! a rev leaves behind.
//!
//! Two things it cannot see, stated so their absence is not mistaken for a
//! choice:
//!
//! * **Worktree bytes of an unmerged path.** `checkout --ours/--theirs/--merge/
//!   --conflict=<style>` rewrite `conflict.txt` in the worktree and leave the
//!   index untouched, and `status` reports `AA` for an unmerged path regardless
//!   of its content. So those cases pin exit code, stdout, and the assertion
//!   that the stage 1/2/3 entries *survive* — a port that resolved them to
//!   stage 0 fails — but not which side's bytes landed on disk.
//! * **A linked worktree's reflog and `ORIG_HEAD`.** `probe_reflogs` walks
//!   `.git/logs`, and a reset run inside `wt/` writes
//!   `.git/worktrees/wt/logs/HEAD` and `.git/worktrees/wt/ORIG_HEAD` instead
//!   (verified with stock git). The `wt` case here is still worth its cost: a
//!   port that resolved the *common* `HEAD` would move `refs/heads/main`, which
//!   `for-each-ref` does see.
//!
//! # Fixture constraints these cases work around
//!
//! * A case is one argv against a pristine copy, so nothing can be staged or
//!   edited first. Every mode that needs local work to refuse over uses
//!   [`Shape::Dirty`], [`Shape::MergeableDirty`], [`Shape::MergeableStaged`] or
//!   [`Shape::Stashed`], which ship it.
//! * `HEAD~2` does not exist on [`Shape::Branched`] (`main` carries two
//!   commits) and `HEAD~1` does not exist on [`Shape::Detached`] (HEAD sits on
//!   the root commit), so the multi-step rev cases use [`Shape::Merged`],
//!   [`Shape::MergeableDirty`] and `main` respectively.
//! * `ORIG_HEAD` is absent from `Branched`, `Dirty`, `Linear` and `Sparse` and
//!   present in `Conflicted`, `Merged`, `Octopus`, `BehindRemote` and
//!   `Stashed`; the `ORIG_HEAD` case runs on `Conflicted` for that reason.
//! * `reset.quiet` is **not** a configuration key in git 2.55.0 — it is absent
//!   from the installed `git-reset` documentation and from the binary's string
//!   table, and `-c reset.quiet=true git reset` still prints
//!   `Unstaged changes after reset:`. Its case below therefore pins that
//!   *neither* side invents an effect for it; `-q` is the only quiet control.
//! * `advice.resetNoRefresh` exists, but the advice it gates is emitted only
//!   when the post-reset refresh exceeds a two-second threshold
//!   (`It took %.2f seconds to refresh the index after reset.`), which a
//!   fixture of this size never reaches. Its case pins the config parse, not
//!   the message.
//!
//! Everything below was executed against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) in a copy of the same template the harness uses,
//! under `env::harden`'s environment, before it was written down.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    reset_mode_matrix(out);
    reset_keep_vs_merge(out);
    reset_unmerged_index(out);
    reset_pathspec_forms(out);
    reset_mode_with_paths(out);
    reset_pathspec_magic(out);
    reset_pathspec_from_file(out);
    reset_rev_spellings(out);
    reset_sparse(out);
    reset_submodules(out);
    reset_bare(out);
    reset_location_and_index(out);
    reset_config(out);
    reset_other_shapes(out);

    checkout_paths(out);
    checkout_overlay(out);
    checkout_conflict_modes(out);
    checkout_sparse_bits(out);
    checkout_branch_creation(out);
    checkout_ref_name_vs_pathspec(out);
    checkout_pathspec_from_file(out);
}

/// [`Case::with_stdin`] with stderr compared byte for byte.
///
/// There is no `strict`+`stdin` constructor, and the refusals a
/// `--pathspec-from-file=-` case reaches are refusal *text* — `fatal:
/// '--pathspec-from-file' and pathspec arguments cannot be used together` is
/// the entire contract, since the exit code (128) is shared with a dozen other
/// argument errors. Built by struct update from the public constructor so the
/// case is identical to one `Case::strict` would produce.
fn strict_stdin(
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    stdin: &'static [u8],
) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

// ---------------------------------------------------------------------------
// reset
// ---------------------------------------------------------------------------

/// The five modes against one shape that has all four kinds of local state.
///
/// [`Shape::Dirty`] carries a staged addition (`staged.txt`), an unstaged edit
/// (`README.md`), an unstaged deletion (`src/lib.rs`) and an untracked file, so
/// every mode lands on a different post-state and no two of these cases can be
/// satisfied by the same implementation:
///
/// * `--soft` leaves the index alone: `staged.txt` stays at stage 0, `A ` in
///   status.
/// * `--mixed` drops it to untracked and prints `Unstaged changes after reset:`
///   followed by the refreshed paths.
/// * `--hard` additionally restores `README.md` and `src/lib.rs` and prints
///   `HEAD is now at <abbrev> <subject>`.
/// * `--keep` drops the staged entry like `--mixed` but prints nothing and does
///   *not* restore the worktree — the local edit, the deletion and
///   `staged.txt` itself all survive, the last as an untracked file.
/// * `--merge` drops the same entry and **deletes `staged.txt` from disk**,
///   because the worktree copy matches the index entry it is removing and is
///   therefore not local work to protect. It is the only mode whose post-state
///   has no `staged.txt` at all, and the only difference between it and
///   `--keep` here — same exit code, same silent stdout — so it is the strict
///   case.
///
/// `--no-refresh` is the pair for `--mixed`: it skips the post-reset
/// `refresh_index()` and so suppresses the `Unstaged changes after reset:`
/// block entirely while leaving the same index behind. A port that hard-codes
/// the message fails it; a port that never prints the message fails `--mixed`.
fn reset_mode_matrix(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset", "--soft"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--mixed"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--keep"], Shape::Dirty));
    out.push(Case::strict("reset", &["reset", "--merge"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "-q", "--hard"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--no-refresh"], Shape::Dirty));
}

/// `--merge` and `--keep`: the two modes that refuse, and the fixture that
/// tells them apart.
///
/// Both run `unpack_trees()` with a two-tree merge and both report
/// `error: Entry '<path>' not uptodate. Cannot merge.` followed by
/// `fatal: Could not reset index file to revision '<rev>'.` at exit 128. What
/// differs is which entries they consider:
///
/// * `--keep` refuses to *overwrite* a locally modified path and otherwise
///   leaves the worktree copy exactly as it found it.
/// * `--merge` treats a worktree copy that matches the index as the index's
///   own and writes the target tree over it.
///
/// On [`Shape::MergeableStaged`] — `keep.txt` staged, nothing else dirty —
/// `reset --keep HEAD~1` rewrites the index to the target tree and leaves the
/// worktree file, so status ends at ` M keep.txt`; `reset --merge HEAD~1`
/// rewrites both and ends clean. That is the only observable difference
/// between the two invocations: both exit 0 and neither prints anything. Both
/// are [`Case::strict`] so a stray message on either would fail too.
///
/// [`Shape::MergeableDirty`] supplies the other axis, unstaged work:
/// `hot.txt` is edited in the worktree and rewritten by `div-hot`, so that
/// reset must refuse; `cold.txt` is rewritten by `div-cold` and clean locally,
/// so that one must land while carrying the two local edits through untouched.
/// `ff-squat` adds a path an untracked file already occupies, which is the
/// other refusal class — `error: Untracked working tree file 'squat.txt' would
/// be overwritten by merge.` — and reaches `verify_absent()` rather than
/// `verify_uptodate()`.
fn reset_keep_vs_merge(out: &mut Vec<Case>) {
    // Lands: the footprint (`trunk.txt`, `cold.txt`) is clean locally, so both
    // guarded modes carry `hot.txt`/`keep.txt`/`squat.txt` through untouched.
    out.push(Case::new("reset", &["reset", "--keep", "HEAD~1"], Shape::MergeableDirty));
    out.push(Case::new("reset", &["reset", "--keep", "div-cold"], Shape::MergeableDirty));
    out.push(Case::new("reset", &["reset", "--merge", "HEAD~1"], Shape::MergeableDirty));
    // The unguarded pair over the same history, for contrast: `--hard`
    // discards the two local edits and keeps only the untracked file, `--soft`
    // keeps everything and only moves HEAD.
    out.push(Case::new("reset", &["reset", "--hard", "HEAD~1"], Shape::MergeableDirty));
    out.push(Case::new("reset", &["reset", "--soft", "HEAD~1"], Shape::MergeableDirty));

    // Refuses: the target tree rewrites the locally edited path.
    out.push(Case::strict("reset", &["reset", "--keep", "div-hot"], Shape::MergeableDirty));
    // Refuses in the other layer: an untracked file sits where the tree writes.
    out.push(Case::strict("reset", &["reset", "--keep", "ff-squat"], Shape::MergeableDirty));

    // The pair that separates the two modes. Nothing else in the corpus does.
    out.push(Case::strict("reset", &["reset", "--keep", "HEAD~1"], Shape::MergeableStaged));
    out.push(Case::strict("reset", &["reset", "--merge", "HEAD~1"], Shape::MergeableStaged));
}

/// Reset over an index that holds stage 1/2/3 entries.
///
/// [`Shape::Conflicted`] is mid-merge with `conflict.txt` at stages 2 and 3
/// (both sides *added* the path, so there is no stage 1) and `MERGE_HEAD`,
/// `MERGE_MODE`, `MERGE_MSG` and `AUTO_MERGE` on disk. `cmd_reset()` rejects
/// two of the five modes here before doing anything, by name:
///
/// * `fatal: Cannot do a soft reset in the middle of a merge.`
/// * `fatal: Cannot do a keep reset in the middle of a merge.`
///
/// Both exit 128 and both leave the unmerged entries in place, so the exit code
/// cannot tell them apart and the message is the behaviour — hence
/// [`Case::strict`]. The three that are allowed each resolve the conflict a
/// different way, and `ls-files --stage` is what shows it: `--mixed` collapses
/// to stage 0 at `HEAD`'s blob and leaves the worktree's conflict markers
/// behind (` M conflict.txt`), `--hard` collapses *and* rewrites the file
/// (clean status), and `reset -- conflict.txt` does the same for one path
/// through the pathspec route rather than the whole-tree one.
///
/// `--mixed HEAD~1` is the case that removes the path from the index
/// altogether: `HEAD~1` predates `conflict.txt`, so the entry disappears and
/// the worktree copy becomes untracked. A port that collapses stages by
/// resolving to `HEAD` rather than by reading the named tree lands on a
/// different index here and nowhere else.
fn reset_unmerged_index(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset"], Shape::Conflicted));
    out.push(Case::new("reset", &["reset", "--hard"], Shape::Conflicted));
    out.push(Case::strict("reset", &["reset", "--soft"], Shape::Conflicted));
    out.push(Case::strict("reset", &["reset", "--keep"], Shape::Conflicted));
    out.push(Case::new("reset", &["reset", "--mixed", "HEAD~1"], Shape::Conflicted));
    out.push(Case::new("reset", &["reset", "--", "conflict.txt"], Shape::Conflicted));
    // `ORIG_HEAD` is written by the merge that built the shape, so this is the
    // documented way out of a conflicted merge spelled with the ref rather than
    // with `HEAD`. It exercises a ref lookup the `HEAD` spelling never does.
    out.push(Case::new("reset", &["reset", "--merge", "ORIG_HEAD"], Shape::Conflicted));
}

/// The pathspec forms of a mixed reset, which is the `git unstage` most people
/// mean.
///
/// Five spellings reach the same code and only one of them is unambiguous:
///
/// * `reset -- <path>` — explicit, no rev.
/// * `reset HEAD -- <path>` — explicit rev and explicit separator.
/// * `reset <path>` — no separator at all. `cmd_reset()` has to decide whether
///   the lone argument is a rev or a pathspec, and it decides *pathspec*
///   because `staged.txt` does not resolve as a rev but does exist. A port that
///   tries `rev-parse` and dies on failure fails this and passes every other
///   case in this group.
/// * `reset --` — a separator with nothing after it, which is a whole-tree
///   mixed reset and not an error.
/// * `reset -- --hard` — a pathspec that *looks* like an option. Nothing
///   matches it, so the reset is a no-op over the paths and the index keeps
///   `staged.txt`; a port that parses arguments after `--` fails.
///
/// `-- nosuch.txt` is the pathspec that matches nothing: git still refreshes
/// and still prints the `Unstaged changes after reset:` block, and the staged
/// entry survives. `-N` adds the entry back as an intent-to-add record — the
/// empty blob `e69de29b` at stage 0 with ` A` in status, which no other case in
/// the corpus produces.
///
/// `--mixed` *with* paths is the deprecated spelling and is the one case here
/// whose whole content is on stderr:
/// `warning: --mixed with paths is deprecated; use 'git reset -- <paths>'
/// instead.` at exit 0. Strict, because a port that silently accepts the form
/// without warning is indistinguishable from a correct one on every other
/// dimension.
fn reset_pathspec_forms(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset", "--", "staged.txt"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "HEAD", "--", "staged.txt"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "staged.txt"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--", "--hard"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "--", "nosuch.txt"], Shape::Dirty));
    out.push(Case::new("reset", &["reset", "-N", "--", "staged.txt"], Shape::Dirty));
    out.push(Case::strict("reset", &["reset", "--mixed", "--", "staged.txt"], Shape::Dirty));
}

/// A mode and a pathspec together, which git rejects by name.
///
/// `cmd_reset()` refuses every mode except `--mixed` as soon as a pathspec is
/// present: `fatal: Cannot do hard reset with paths.` The refusal is the safety
/// contract — `git reset --hard -- <path>` reads like "throw away my changes to
/// this one file" and does not mean that — so it is compared byte for byte.
/// One case rather than four, because all four modes reach the same `die()`
/// with only the mode's name substituted.
fn reset_mode_with_paths(out: &mut Vec<Case>) {
    out.push(Case::strict("reset", &["reset", "--hard", "--", "staged.txt"], Shape::Dirty));
}

/// Pathspec magic, on the one shape whose paths need quoting.
///
/// [`Shape::AwkwardPaths`] tracks `with space.txt`, `üñïçødé.txt`,
/// `quote"name.txt` and `nested/deep/path.txt`, all added in the second commit,
/// so `reset HEAD~1 -- <spec>` removes exactly the matched subset from the
/// index and leaves the rest. Each magic prefix selects a different subset, and
/// `ls-files --stage` names which one survived — with `core.quotePath`'s
/// C-escaping applied to three of the four, so the case also pins that the
/// port's index writer and git's quoting agree:
///
/// * `:(glob)nested/**/*.txt` — `**` crosses directory boundaries only under
///   `:(glob)`; the default wildmatch would not match `nested/deep/path.txt`.
/// * `:!nested/` and `:(exclude)nested/deep/path.txt` — the short and long
///   spellings of exclusion, which invert the set: the three root paths go and
///   `nested/deep/path.txt` stays.
/// * `:(icase)WITH SPACE.TXT` — matches a path whose recorded name is entirely
///   lower case, and which the argv does not otherwise resemble.
///
/// The bare `reset HEAD~1` is the control: it removes all four.
fn reset_pathspec_magic(out: &mut Vec<Case>) {
    let s = Shape::AwkwardPaths;
    out.push(Case::new("reset", &["reset", "HEAD~1"], s));
    out.push(Case::new("reset", &["reset", "HEAD~1", "--", ":(glob)nested/**/*.txt"], s));
    out.push(Case::new("reset", &["reset", "HEAD~1", "--", ":!nested/"], s));
    out.push(Case::new("reset", &["reset", "HEAD~1", "--", ":(exclude)nested/deep/path.txt"], s));
    out.push(Case::new("reset", &["reset", "HEAD~1", "--", ":(icase)WITH SPACE.TXT"], s));
}

/// `--pathspec-from-file=-`, fed from stdin.
///
/// The list arrives on stdin rather than in argv, so a port that reads the
/// pathspec only from `argv` produces a whole-tree reset where git produces a
/// two-path one — a difference `ls-files --stage` reports and stdout does not.
/// [`Shape::AwkwardPaths`] is where the two separators diverge: the LF form
/// applies `core.quotePath` unquoting to each line while the NUL form takes
/// every byte literally, so both spellings of the same two paths must land on
/// the identical index.
///
/// An empty payload is its own contract: git parses zero pathspecs and performs
/// the whole-tree reset rather than failing, which is not obvious from the
/// option's name.
///
/// The two refusals are text-only — both exit 128, shared with every other
/// argument error — so both compare stderr:
/// `fatal: '--pathspec-from-file' and pathspec arguments cannot be used
/// together` and `fatal: the option '--pathspec-file-nul' requires
/// '--pathspec-from-file'`.
fn reset_pathspec_from_file(out: &mut Vec<Case>) {
    out.push(Case::with_stdin(
        "reset",
        &["reset", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"staged.txt\n",
    ));
    out.push(Case::with_stdin("reset", &["reset", "--pathspec-from-file=-"], Shape::Dirty, b""));
    out.push(Case::with_stdin(
        "reset",
        &["reset", "-N", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"staged.txt\n",
    ));
    out.push(Case::with_stdin(
        "reset",
        &["reset", "HEAD~1", "--pathspec-from-file=-"],
        Shape::AwkwardPaths,
        b"with space.txt\nnested/deep/path.txt\n",
    ));
    out.push(Case::with_stdin(
        "reset",
        &["reset", "HEAD~1", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::AwkwardPaths,
        b"with space.txt\0nested/deep/path.txt\0",
    ));
    out.push(strict_stdin(
        "reset",
        &["reset", "--pathspec-from-file=-", "--", "staged.txt"],
        Shape::Dirty,
        b"staged.txt\n",
    ));
    out.push(Case::strict("reset", &["reset", "--pathspec-file-nul"], Shape::Dirty));
}

/// The rev spellings a reset has to resolve, and the two object types it must
/// refuse.
///
/// [`Shape::Branched`] carries two commits on `main`, a lightweight tag
/// (`v0.1.0`) and an annotated one (`v0.2.0`) on the tip, a `feature` branch
/// one commit further along, and a five-entry `HEAD` reflog — the only shape
/// with enough reflog depth for `HEAD@{1}` and `main@{1}` to name *different*
/// commits (`feature commit` and the root commit respectively). That
/// distinction is the point: a port that resolves `<ref>@{n}` by walking
/// commits instead of reading `.git/logs/refs/heads/main` lands on the wrong
/// object for `main@{1}` while passing `HEAD@{1}`.
///
/// The two refusals are the type check in `cmd_reset()`, which peels through
/// `^{tree}` and `<rev>:<path>` and then insists on a commit when no pathspec
/// is present:
///
/// * `error: object <oid> is a tree, not a commit` for `HEAD^{tree}`
/// * `error: object <oid> is a blob, not a commit` for `HEAD:README.md`
///
/// both followed by `fatal: Could not parse object '<spec>'.` at exit 128. The
/// same `HEAD^{tree}` *with* a pathspec is accepted, because the path form
/// reads a tree and never moves `HEAD` — the pair is what proves the check is
/// conditional rather than unconditional.
///
/// The abbreviated oid is deliberately eight hex digits of a full-length id
/// that no other object shares in this fixture, so it resolves without
/// ambiguity and pins that a port accepts a short id at all.
fn reset_rev_spellings(out: &mut Vec<Case>) {
    let b = Shape::Branched;
    out.push(Case::new("reset", &["reset", "--soft", "HEAD^"], b));
    out.push(Case::new("reset", &["reset", "--hard", "v0.1.0"], b));
    out.push(Case::new("reset", &["reset", "--hard", "HEAD@{1}"], b));
    out.push(Case::new("reset", &["reset", "--soft", "main@{1}"], b));
    out.push(Case::new("reset", &["reset", "--hard", "edfab1b7"], b));
    out.push(Case::new("reset", &["reset", "HEAD^{tree}", "--", "src/lib.rs"], b));
    out.push(Case::strict("reset", &["reset", "--hard", "HEAD^{tree}"], b));
    out.push(Case::strict("reset", &["reset", "--hard", "HEAD:README.md"], b));
    // A second parent, which only a real merge commit has. `Merged` is the only
    // two-parent shape; `HEAD^2` is `side commit` and `HEAD^1` is `main commit`.
    out.push(Case::new("reset", &["reset", "--hard", "HEAD^2"], Shape::Merged));
}

/// Reset over a cone-mode sparse checkout, where half the index is
/// skip-worktree and the matching files are not on disk.
///
/// [`Shape::Sparse`] keeps `inside/` and the root files in the worktree and
/// excludes `outside/`, whose two entries carry the skip-worktree bit and have
/// no worktree copy. That makes the modes diverge in a way no dense shape can
/// show:
///
/// * `--hard HEAD~1` removes the entries and leaves the worktree clean apart
///   from the untracked `outside/stray.txt` — it must *not* try to unlink the
///   absent `outside/*` files.
/// * `--mixed HEAD~1` removes the same entries but leaves the files, so
///   `inside/keep.txt`, `inside/nested/also.txt` and `root.txt` become
///   untracked while `outside/drop.txt` and `outside/nested/deep.txt` do not
///   appear at all — they were never written out. A port that ignores
///   skip-worktree reports five untracked paths where git reports three.
/// * `--keep HEAD~1` reaches `unpack_trees()` with `CE_SKIP_WORKTREE` entries
///   in the source index, which `verify_uptodate()` must skip rather than stat.
/// * `reset HEAD~1 -- outside/drop.txt` targets a skip-worktree path by name
///   through the pathspec route: git removes the index entry and reports
///   `D  outside/drop.txt` without ever touching the worktree.
fn reset_sparse(out: &mut Vec<Case>) {
    let s = Shape::Sparse;
    out.push(Case::new("reset", &["reset", "--hard", "HEAD~1"], s));
    out.push(Case::new("reset", &["reset", "--mixed", "HEAD~1"], s));
    out.push(Case::new("reset", &["reset", "--keep", "HEAD~1"], s));
    out.push(Case::new("reset", &["reset", "HEAD~1", "--", "outside/drop.txt"], s));
}

/// `--recurse-submodules`, which changes what is left on disk rather than what
/// is printed.
///
/// [`Shape::Submodule`]'s second commit adds `.gitmodules` and the `sub`
/// gitlink, so `reset --hard HEAD~1` has to remove both. Without recursion git
/// cannot empty the submodule's own checkout, so it warns
/// `warning: unable to rmdir 'sub': Directory not empty` on stderr and leaves
/// `?? sub/` behind; with `--recurse-submodules`, or with `submodule.recurse`
/// set, the directory goes and status is clean. Those two post-states are the
/// assertion — the stdout (`HEAD is now at <abbrev> initial`) is identical in
/// all three.
///
/// `--keep` lands on the *non-recursive* answer even though nothing asked it to
/// recurse or not: it removes `.gitmodules` and the gitlink from the index,
/// cannot empty `sub/`, and emits the same `unable to rmdir` warning at exit 0.
/// It is [`Case::strict`] for that warning — a port that recursed here, or that
/// refused because a gitlink was in the way, would be caught by nothing else.
fn reset_submodules(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("reset", &["reset", "--hard", "HEAD~1"], s));
    out.push(Case::new("reset", &["reset", "--recurse-submodules", "--hard", "HEAD~1"], s));
    out.push(
        Case::new("reset", &["reset", "--hard", "HEAD~1"], s)
            .with_config(&[("submodule.recurse", "true")]),
    );
    out.push(Case::strict("reset", &["reset", "--keep", "HEAD~1"], s));
}

/// Reset in a bare repository, completing the pair `corpus.rs` starts.
///
/// `corpus.rs` already covers `--mixed` (rejected by name:
/// `mixed reset is not allowed in a bare repository`) and `--hard` (rejected
/// earlier, inside `setup_work_tree()`: `this operation must be run in a work
/// tree`). The two remaining answers are here:
///
/// * `--soft` **succeeds**, at exit 0, and changes nothing at all: it touches
///   neither index nor worktree, and `.remote.git`'s `HEAD` names an unborn
///   `refs/heads/master` (the fixture pushed `main` and `div`, not `master`),
///   so there is no ref to move either. A byte-for-byte `diff -r` of the git
///   directory before and after is empty. The case is the exit code: it is the
///   one mode a bare repository accepts, and a port that refuses every reset
///   there passes the two refusals `corpus.rs` already has and fails this.
/// * `--keep` fails in the same place `--hard` does, with the same message, and
///   proves the gate is `reset_type != SOFT` rather than a `--hard` special
///   case.
///
/// [`Shape::BehindRemote`]'s `.remote.git` is the bare repository, already in
/// the fixture, so no shape is added.
fn reset_bare(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset", "--soft"], Shape::BehindRemote).in_dir(".remote.git"));
    out.push(
        Case::strict("reset", &["reset", "--keep"], Shape::BehindRemote).in_dir(".remote.git"),
    );
}

/// Where the reset runs from, and which index it writes.
///
/// * From a subdirectory, `--hard` still resets the whole tree: the mode is
///   not scoped to the working directory the way a pathspec would be.
/// * `GIT_INDEX_FILE` pointed at a path that does not exist yet is the case the
///   corpus has no other instance of for a *writing* verb. Stock git creates
///   the named file, resets it from `HEAD`, and — for `--hard` — checks the
///   worktree out from it, while `.git/index` is left exactly as the fixture
///   built it. The probe therefore sees `README.md` and `src/lib.rs` restored
///   on disk *and* `staged.txt` still staged, a combination no ordinary reset
///   produces. A port that ignores the variable rewrites the real index and
///   fails on `ls-files --stage`; one that honours it but forgets the worktree
///   half fails on `status`.
/// * Inside the linked worktree, `reset --hard main` must move
///   `refs/heads/linked` and nothing else. The `for-each-ref` probe is what
///   proves `refs/heads/main` stayed put; the worktree's own reflog and
///   `ORIG_HEAD` land under `.git/worktrees/wt/` and are outside every probe
///   (see the module note).
fn reset_location_and_index(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset", "--hard"], Shape::Dirty).in_dir("src"));
    out.push(
        Case::new("reset", &["reset", "--hard"], Shape::Dirty)
            .with_env(&[("GIT_INDEX_FILE", "{repo}/.git/alt-index")]),
    );
    out.push(
        Case::new("reset", &["reset", "--mixed"], Shape::Dirty)
            .with_env(&[("GIT_INDEX_FILE", "{repo}/.git/alt-index")]),
    );
    out.push(Case::new("reset", &["reset", "--hard", "main"], Shape::Worktree).in_dir("wt"));
}

/// Configuration `reset` consults, including one key that no longer exists.
///
/// * `reset.quiet` is gone from git 2.55.0 (see the module note): the reset
///   still prints `Unstaged changes after reset:`. The case pins that the port
///   does not resurrect it — an implementation that honours a key stock git
///   ignores diverges on stdout, and this is the only case that would catch it.
/// * `advice.resetNoRefresh` is live but time-gated above two seconds, so the
///   advice is unreachable on a fixture this size and the case measures the
///   config parse: an unknown-key error or a spuriously emitted hint both fail.
/// * `core.fsmonitor=false` is the explicit-off spelling of the default. It
///   goes through a different code path than an absent key (`git_config_bool`
///   on a set value versus no value at all) and a port that treats the string
///   `false` as a hook path would try to run it.
fn reset_config(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset"], Shape::Dirty).with_config(&[("reset.quiet", "true")]));
    out.push(
        Case::new("reset", &["reset"], Shape::Dirty)
            .with_config(&[("advice.resetNoRefresh", "false")]),
    );
    out.push(
        Case::new("reset", &["reset", "--hard"], Shape::Dirty)
            .with_config(&[("core.fsmonitor", "false")]),
    );
}

/// Three shapes that each expose one thing the others cannot.
///
/// * [`Shape::NoIndexTrees`] carries `core.abbrev = 10` in its repository
///   config, and `reset --hard` prints `HEAD is now at <abbrev> <subject>`.
///   Every other shape is small enough that git's `auto` width and the
///   built-in 7 coincide, so this is the only reset case where a port that
///   ignores `core.abbrev` produces different stdout.
/// * [`Shape::Stashed`] has three stash entries. `reset --hard HEAD~1` must
///   leave `refs/stash` and its reflog alone — the `stash list` probe is the
///   assertion, and a port that resets `refs/stash` along with the branch
///   destroys three commits' worth of work that nothing else would report.
/// * [`Shape::Detached`] has no branch to move, so `reset --keep main` has to
///   write `HEAD` directly rather than through a symref. It also lands on the
///   one post-state that separates `--keep` from `--mixed` on a clean tree:
///   `--keep` writes `second.txt` out and leaves a clean status, where
///   `--mixed` would leave ` D second.txt` behind.
fn reset_other_shapes(out: &mut Vec<Case>) {
    out.push(Case::new("reset", &["reset", "--hard", "HEAD~1"], Shape::NoIndexTrees));
    out.push(Case::new("reset", &["reset", "--hard", "HEAD~1"], Shape::Stashed));
    out.push(Case::new("reset", &["reset", "--keep", "main"], Shape::Detached));
}

// ---------------------------------------------------------------------------
// checkout — index and worktree modes only
// ---------------------------------------------------------------------------
//
// The branch-switching half of `checkout` is covered by `corpus.rs`
// (`checkout feature`, `checkout -b newbranch`, `checkout --detach HEAD`),
// `worktree_index` (the eight `-f` cases) and `shape_reach` (three on
// `Sparse`); `switch` and `restore` are a separate module. What is left, and is
// what follows, is the path-restoring half and the argument handling around it.

/// `checkout -- <path>` and `checkout <tree-ish> -- <path>`: restore from the
/// index, and restore from a tree.
///
/// The two forms differ in which of {index, worktree} moves, and only
/// `ls-files --stage` reports it:
///
/// * `checkout -- <path>` copies the *index* entry to the worktree. On
///   [`Shape::Dirty`] that reverts `README.md` and leaves `staged.txt` staged.
/// * `checkout <rev> -- <path>` copies the tree's entry into *both*. On
///   `Branched`, `checkout HEAD~1 -- src/lib.rs` leaves `M  src/lib.rs` — index
///   changed, worktree matching it — which is the opposite of what the first
///   form produces.
///
/// A bare `checkout --` is not an error: it lists the modified paths on stdout
/// (`M\tREADME.md` and so on) and changes nothing, which is a form no other
/// case in the corpus reaches. `-- nosuch.txt` is the refusal:
/// `error: pathspec 'nosuch.txt' did not match any file(s) known to git` at
/// exit 1 — not 128, which is what separates a pathspec miss from an argument
/// error, so it is strict.
///
/// `HEAD^{tree}` with a pathspec is accepted where `reset` accepted it too, and
/// a lightweight tag and a branch name both stand in for the tree-ish, so the
/// peeling is exercised on three object routes.
fn checkout_paths(out: &mut Vec<Case>) {
    out.push(Case::new("checkout", &["checkout", "--", "README.md"], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "--", "."], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "HEAD", "--", "."], Shape::Dirty));
    out.push(Case::new("checkout", &["checkout", "--"], Shape::Dirty));
    out.push(Case::strict("checkout", &["checkout", "--", "nosuch.txt"], Shape::Dirty));

    let b = Shape::Branched;
    out.push(Case::new("checkout", &["checkout", "HEAD~1", "--", "src/lib.rs"], b));
    out.push(Case::new("checkout", &["checkout", "v0.1.0", "--", "src/lib.rs"], b));
    out.push(Case::new("checkout", &["checkout", "feature", "--", "feature.txt"], b));
    out.push(Case::new("checkout", &["checkout", "HEAD^{tree}", "--", "src/lib.rs"], b));
}

/// `--overlay` versus `--no-overlay`, which decide whether paths the tree does
/// *not* carry are removed.
///
/// Overlay is the default and only ever adds or updates. `--no-overlay` makes
/// the checkout an exact materialisation of the tree over the pathspec, so
/// entries the tree lacks are deleted from index and worktree both. The
/// difference is invisible unless the current index holds something the target
/// tree does not, which is why each pair is on a shape that has one:
///
/// * `Dirty` with `HEAD -- .`: `staged.txt` is staged and absent from `HEAD`,
///   so `--no-overlay` unstages *and* deletes it while `--overlay` keeps it.
/// * `AwkwardPaths` with `HEAD~1 -- .`: the four awkward paths are added in the
///   second commit, so `--no-overlay` stages four deletions and `--overlay`
///   leaves a clean tree. This is also where the removal has to handle quoted
///   and non-ASCII names.
/// * `Sparse` with `HEAD~1 -- .`: `--no-overlay` must delete the three present
///   paths and leave the two skip-worktree entries alone — they are in `HEAD~1`
///   too, and there is no worktree file to unlink.
fn checkout_overlay(out: &mut Vec<Case>) {
    out.push(Case::new("checkout", &["checkout", "--no-overlay", "HEAD", "--", "."], Shape::Dirty));
    out.push(Case::new(
        "checkout",
        &["checkout", "--no-overlay", "HEAD~1", "--", "."],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "checkout",
        &["checkout", "--overlay", "HEAD~1", "--", "."],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "checkout",
        &["checkout", "--no-overlay", "HEAD~1", "--", "."],
        Shape::Sparse,
    ));
}

/// Checking a path out while the index holds stage 1/2/3 entries.
///
/// This is the group whose worktree bytes the probes cannot read (see the
/// module note), and it is written to assert the half they can. On
/// [`Shape::Conflicted`], `conflict.txt` sits at stages 2 and 3:
///
/// * `--ours`, `--theirs`, `-m` and `--conflict=<style>` all write the worktree
///   file and **leave the index unmerged**, exit 0, no output. A port that
///   collapses the entry to stage 0 — the intuitive "resolve it" reading —
///   fails every one of them on `ls-files --stage`.
/// * A plain `checkout -- conflict.txt` refuses: `error: path 'conflict.txt' is
///   unmerged`, exit 1. That refusal is the reason `--ours` exists, so it is
///   compared byte for byte.
/// * `checkout HEAD -- conflict.txt` is the one form that *does* resolve, to
///   `HEAD`'s blob at stage 0, because it reads a tree rather than the index.
/// * `checkout main` — a branch switch rather than a path checkout — is
///   refused before anything moves, with `conflict.txt: needs merge` on
///   **stdout** and `error: you need to resolve your current index first` on
///   stderr, at exit 1. The split across the two streams is unusual enough to
///   be worth pinning strictly.
/// * `--conflict=bogus` is the option-parse refusal:
///   `error: unknown conflict style 'bogus'` at exit **129**, the
///   `parse_options()` usage code rather than 128 — a distinction a port that
///   dies with its own error message gets wrong.
///
/// `--ours` on a path that is *not* unmerged (`README.md` on `Dirty`) is the
/// fallback case: git silently checks the stage 0 entry out, exit 0. A port
/// that requires an unmerged entry for `--ours` fails it.
fn checkout_conflict_modes(out: &mut Vec<Case>) {
    let c = Shape::Conflicted;
    out.push(Case::new("checkout", &["checkout", "--ours", "--", "conflict.txt"], c));
    out.push(Case::new("checkout", &["checkout", "--theirs", "--", "conflict.txt"], c));
    out.push(Case::new("checkout", &["checkout", "-m", "--", "conflict.txt"], c));
    out.push(Case::new("checkout", &["checkout", "--conflict=diff3", "--", "conflict.txt"], c));
    out.push(Case::new("checkout", &["checkout", "HEAD", "--", "conflict.txt"], c));
    out.push(Case::strict("checkout", &["checkout", "--", "conflict.txt"], c));
    out.push(Case::strict("checkout", &["checkout", "--conflict=bogus", "--", "conflict.txt"], c));
    out.push(Case::strict("checkout", &["checkout", "main"], c));
    out.push(Case::new("checkout", &["checkout", "--ours", "--", "README.md"], Shape::Dirty));
}

/// `--ignore-skip-worktree-bits`, the flag that decides whether a
/// sparse-excluded path is addressable at all.
///
/// On [`Shape::Sparse`], `outside/drop.txt` is in the index with the
/// skip-worktree bit set and is not on disk. Naming it without the flag is a
/// pathspec *miss* — `error: pathspec 'outside/drop.txt' did not match any
/// file(s) known to git`, exit 1 — because the pathspec match skips
/// skip-worktree entries; with the flag it matches, the file is written out,
/// and the command exits 0. Two invocations one token apart with different exit
/// codes, so the refusal half is strict.
fn checkout_sparse_bits(out: &mut Vec<Case>) {
    let s = Shape::Sparse;
    out.push(Case::new(
        "checkout",
        &["checkout", "--ignore-skip-worktree-bits", "--", "outside/drop.txt"],
        s,
    ));
    out.push(Case::strict("checkout", &["checkout", "--", "outside/drop.txt"], s));
}

/// Branch creation through `checkout`, where the three creating flags each
/// leave a different ref state and a different message.
///
/// * `-b topic HEAD~1` creates and switches: `Switched to a new branch 'topic'`.
/// * `-B feature HEAD~1` force-creates over an existing branch:
///   `Switched to and reset branch 'feature'` — and `refs/heads/feature` moves,
///   which `for-each-ref` sees.
/// * `-B main HEAD~1` resets the branch already checked out, which git reports
///   differently again: `Reset branch 'main'`, with no "switched" at all.
/// * `--orphan fresh` creates an unparented branch: `HEAD` becomes a symref to
///   a ref that does not exist yet, the index is kept, and every tracked path
///   turns into a staged addition. On [`Shape::Dirty`] it additionally has to
///   keep the staged, unstaged and deleted entries distinct through that
///   transition, and it prints the modified-path list on stdout.
/// * `--detach feature` detaches at a branch tip rather than at a rev, which
///   is the spelling `corpus.rs`'s `--detach HEAD` does not cover.
///
/// The state probe carries the weight for all of them: `rev-parse --abbrev-ref
/// HEAD`, `rev-parse HEAD` and `for-each-ref` between them pin which ref was
/// created, where it points, and which one `HEAD` now names.
fn checkout_branch_creation(out: &mut Vec<Case>) {
    let b = Shape::Branched;
    out.push(Case::new("checkout", &["checkout", "-b", "topic", "HEAD~1"], b));
    out.push(Case::new("checkout", &["checkout", "-B", "feature", "HEAD~1"], b));
    out.push(Case::new("checkout", &["checkout", "-B", "main", "HEAD~1"], b));
    out.push(Case::new("checkout", &["checkout", "--orphan", "fresh"], b));
    out.push(Case::new("checkout", &["checkout", "--detach", "feature"], b));
    out.push(Case::new("checkout", &["checkout", "--orphan", "fresh"], Shape::Dirty));
}

/// An argument that is not a valid *ref name* must fall through to pathspec.
///
/// `foo.lock` cannot be a ref name — `check_refname_format()` rejects any
/// component ending in `.lock` — but it is a perfectly ordinary pathspec.
/// `cmd_checkout()` therefore has to distinguish "this is a ref that does not
/// exist" from "this is not a ref name at all", and both answers end at the
/// same place: `error: pathspec 'foo.lock' did not match any file(s) known to
/// git`, exit **1**. A port that validates the argument as a ref name and dies
/// — `cannot lock ref`, `not a valid ref`, exit 128 — is the known defect class
/// this case exists for, and it is invisible to every argument that happens to
/// be a legal ref name.
///
/// The same string after `-b` is the opposite answer, because there it *is*
/// being used as a ref name: `fatal: 'foo.lock' is not a valid branch name` at
/// exit 128, followed by two `hint:` lines. `--no-advice` suppresses the hints
/// and nothing else, which is what its pair pins — a port that implements the
/// flag as "be quiet" would drop the `fatal:` line too.
///
/// The detached-HEAD note is the same mechanism on the success path:
/// `checkout v0.1.0` writes eighteen lines to stderr, `--no-advice` reduces
/// them to the one line `HEAD is now at <abbrev> <subject>`. Strict, because
/// both are entirely stderr at exit 0 and nothing else distinguishes them.
fn checkout_ref_name_vs_pathspec(out: &mut Vec<Case>) {
    let r = Shape::BehindRemote;
    out.push(Case::strict("checkout", &["checkout", "foo.lock"], r));
    out.push(Case::strict("checkout", &["checkout", "-b", "foo.lock"], r));
    out.push(
        Case::strict("checkout", &["checkout", "-b", "foo.lock"], r)
            .with_globals(&[&["--no-advice"]]),
    );
    out.push(
        Case::strict("checkout", &["checkout", "v0.1.0"], Shape::Branched)
            .with_globals(&[&["--no-advice"]]),
    );
}

/// `checkout --pathspec-from-file=-`, fed from stdin.
///
/// `checkout -p` is the other stdin-driven mode and is unreachable here — it is
/// interactive and the harness has no terminal — so this is the only way the
/// verb's stdin path is measured at all.
///
/// The three accepting cases separate the two halves of the option: with no
/// tree-ish the list drives an index-to-worktree restore (silent), with one it
/// drives a tree-to-index-and-worktree restore and git reports
/// `Updated <n> path(s) from <abbrev>` on stderr, and combined with
/// `--no-overlay` on [`Shape::AwkwardPaths`] it drives a *removal* of the two
/// named paths — `Updated 0 paths`, two staged deletions. That last one is
/// where a port that treats the file's lines as literal bytes rather than as
/// `core.quotePath`-quoted pathspecs lands on a different index.
///
/// The refusal is text-only at exit 128, so it compares stderr:
/// `fatal: '--pathspec-from-file' and pathspec arguments cannot be used
/// together`.
fn checkout_pathspec_from_file(out: &mut Vec<Case>) {
    out.push(Case::with_stdin(
        "checkout",
        &["checkout", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"README.md\n",
    ));
    out.push(Case::with_stdin(
        "checkout",
        &["checkout", "HEAD", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"README.md\n",
    ));
    out.push(Case::with_stdin(
        "checkout",
        &["checkout", "--no-overlay", "HEAD~1", "--pathspec-from-file=-"],
        Shape::AwkwardPaths,
        b"with space.txt\nnested/deep/path.txt\n",
    ));
    out.push(strict_stdin(
        "checkout",
        &["checkout", "--pathspec-from-file=-", "--", "README.md"],
        Shape::Dirty,
        b"README.md\n",
    ));
}
