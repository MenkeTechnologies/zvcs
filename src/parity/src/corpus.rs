//! The hand-written case corpus: known-interesting invocations per subcommand.
//!
//! This is the targeted half of the harness. It encodes the flags and repo
//! states a human knows are load-bearing, so a regression in a common path
//! fails loudly instead of waiting for the fuzzer to stumble into it.
//!
//! Cases here are curated, not exhaustive — `fuzz` covers combinatorial breadth.

//! The commands below are grouped into submodules by subsystem so the corpus can
//! grow per-area without one file becoming a merge point. Each submodule exposes
//! `pub fn cases(out: &mut Vec<Case>)` and is called from [`cases`].

use crate::fixture::Shape;
use crate::runner::{Case, Sequence};

mod diff_family;
mod discovery;
mod history_rewrite;
mod info_attrs;
mod mail_patch;
mod maintenance;
mod merge_dirty;
mod merge_family;
mod misc_commands;
mod plumbing_objects;
mod plumbing_refs;
mod sequences;
mod shape_reach;
mod transport_local;
mod worktree_index;

/// Shapes every read-only command should agree on regardless of history layout.
pub(crate) const READ_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Detached,
];

/// Expand one read-only invocation across the standard shapes.
pub(crate) fn read_only(cmd: &'static str, args: &[&str], out: &mut Vec<Case>) {
    for &shape in READ_SHAPES {
        out.push(Case::new(cmd, args, shape));
    }
}

/// An index file that is not there. `{repo}` is [`crate::runner::REPO_PLACEHOLDER`],
/// replaced with the running side's own fixture root.
const NO_INDEX: &[(&str, &str)] = &[("GIT_INDEX_FILE", "{repo}/.git/no-such-index")];

/// The curated **multi-step** corpus: workflows whose steps run against one
/// repository, compared after every step.
///
/// Separate from [`cases`] rather than folded into it because a sequence is a
/// different unit of work, not a `Case` with more fields — see
/// [`crate::runner::Sequence`]. Both lists are merged into one `Job` list by
/// `main`, so a sequence is scheduled, filtered by `--only` and scored exactly
/// like every other case.
pub fn sequences() -> Vec<Sequence> {
    sequences::sequences()
}

/// The full curated corpus.
pub fn cases() -> Vec<Case> {
    let mut c = Vec::new();

    // The nested-option matrices, which cover the commands whose flags *are* the
    // semantics — stash, pull, merge, fetch — by expanding their interacting flags
    // rather than by listing invocations someone thought of. See
    // [`crate::nested`]; those cases also compare stderr, because a refusal's
    // message is the whole behaviour being tested.
    crate::nested::cases(&mut c);

    // ---- rev-parse: the most-called plumbing in any script ----
    read_only("rev-parse", &["rev-parse", "HEAD"], &mut c);
    read_only("rev-parse", &["rev-parse", "--abbrev-ref", "HEAD"], &mut c);
    read_only("rev-parse", &["rev-parse", "--short", "HEAD"], &mut c);
    read_only("rev-parse", &["rev-parse", "--verify", "HEAD"], &mut c);
    read_only("rev-parse", &["rev-parse", "--git-dir"], &mut c);
    read_only("rev-parse", &["rev-parse", "--show-toplevel"], &mut c);
    read_only("rev-parse", &["rev-parse", "--is-inside-work-tree"], &mut c);
    read_only("rev-parse", &["rev-parse", "HEAD^"], &mut c);
    read_only("rev-parse", &["rev-parse", "HEAD~1"], &mut c);
    c.push(Case::new("rev-parse", &["rev-parse", "main"], Shape::Branched));
    c.push(Case::new("rev-parse", &["rev-parse", "v0.1.0"], Shape::Branched));
    c.push(Case::new("rev-parse", &["rev-parse", "v0.2.0^{commit}"], Shape::Branched));

    // ---- status: the most-read porcelain by humans and tooling alike ----
    read_only("status", &["status"], &mut c);
    read_only("status", &["status", "--porcelain"], &mut c);
    read_only("status", &["status", "--porcelain=v1"], &mut c);
    read_only("status", &["status", "--short"], &mut c);
    read_only("status", &["status", "--short", "--branch"], &mut c);
    read_only("status", &["status", "--untracked-files=all"], &mut c);
    read_only("status", &["status", "--untracked-files=no"], &mut c);
    c.push(Case::new("status", &["status", "--porcelain"], Shape::Conflicted));
    c.push(Case::new("status", &["status", "--porcelain"], Shape::AwkwardPaths));
    c.push(Case::new("status", &["status", "--porcelain"], Shape::Submodule));

    // ---- log / rev-list: history traversal and formatting ----
    read_only("log", &["log", "--oneline"], &mut c);
    read_only("log", &["log", "-1"], &mut c);
    read_only("log", &["log", "--format=%H"], &mut c);
    read_only("log", &["log", "--format=%H %s"], &mut c);
    read_only("log", &["log", "--pretty=format:%an <%ae>"], &mut c);
    read_only("log", &["log", "--max-count=2", "--oneline"], &mut c);
    c.push(Case::new("log", &["log", "--oneline", "--all"], Shape::Branched));
    c.push(Case::new("log", &["log", "--oneline", "--graph"], Shape::Merged));
    c.push(Case::new("log", &["log", "--merges", "--oneline"], Shape::Merged));
    // The graph renderer, on histories that make its rows do work.
    //
    // `--oneline` prints one row per commit, so it exercises almost none of
    // `graph.c`: the pre-commit expansion rows, the padding row between records,
    // and the collapsing rows only appear once a record spans several lines or a
    // commit has more than two parents. A separator-row bug survived in ordinary
    // two-parent merges precisely because no case here rendered a multi-line
    // record, and the octopus rows had no fixture at all.
    c.push(Case::new("log", &["log", "--graph", "--pretty=medium", "--all"], Shape::Merged));
    c.push(Case::new("log", &["log", "--graph", "--oneline", "--all"], Shape::Octopus));
    c.push(Case::new("log", &["log", "--graph", "--pretty=medium", "--all"], Shape::Octopus));
    c.push(Case::new("log", &["log", "--graph", "--oneline"], Shape::Octopus));
    c.push(Case::new("log", &["log", "--graph", "--oneline", "--all", "--date-order"], Shape::Octopus));
    c.push(Case::new("log", &["log", "--graph", "--oneline", "--all", "--topo-order"], Shape::Octopus));
    c.push(Case::new("log", &["log", "--graph", "--oneline", "--first-parent"], Shape::Octopus));
    read_only("rev-list", &["rev-list", "HEAD"], &mut c);
    read_only("rev-list", &["rev-list", "--count", "HEAD"], &mut c);
    read_only("rev-list", &["rev-list", "--max-count=1", "HEAD"], &mut c);

    // ---- object inspection ----
    read_only("cat-file", &["cat-file", "-t", "HEAD"], &mut c);
    read_only("cat-file", &["cat-file", "-p", "HEAD"], &mut c);
    read_only("cat-file", &["cat-file", "-s", "HEAD"], &mut c);
    read_only("cat-file", &["cat-file", "-e", "HEAD"], &mut c);
    read_only("cat-file", &["cat-file", "commit", "HEAD"], &mut c);
    read_only("ls-tree", &["ls-tree", "HEAD"], &mut c);
    read_only("ls-tree", &["ls-tree", "-r", "HEAD"], &mut c);
    read_only("ls-tree", &["ls-tree", "-r", "--name-only", "HEAD"], &mut c);
    read_only("ls-tree", &["ls-tree", "--full-tree", "-r", "HEAD"], &mut c);
    read_only("ls-files", &["ls-files"], &mut c);
    read_only("ls-files", &["ls-files", "--stage"], &mut c);
    read_only("ls-files", &["ls-files", "--full-name"], &mut c);
    c.push(Case::new("ls-files", &["ls-files", "--unmerged"], Shape::Conflicted));
    c.push(Case::new("ls-files", &["ls-files"], Shape::AwkwardPaths));

    // ---- show / diff / blame ----
    read_only("show", &["show", "--oneline", "--no-patch"], &mut c);
    read_only("show", &["show", "--stat"], &mut c);
    read_only("show", &["show", "HEAD"], &mut c);
    read_only("diff", &["diff"], &mut c);
    read_only("diff", &["diff", "--stat"], &mut c);
    read_only("diff", &["diff", "--name-only"], &mut c);
    read_only("diff", &["diff", "--name-status"], &mut c);
    read_only("diff", &["diff", "--cached"], &mut c);
    c.push(Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Detached));
    c.push(Case::new("diff", &["diff", "main", "feature"], Shape::Branched));
    // `-R` against a *worktree*, which used to be refused outright.
    //
    // Reversing a worktree diff is not the same code as reversing tree-vs-tree:
    // the pre-image is the file on disk, so the blob platform's worktree root has
    // to move to the old side, and the pair's ids, modes and submodule dirt swap
    // with it. Tree-vs-tree `-R` already worked, so only these reach it.
    //
    // Deliberately not covered here: the bug that survived longest lived in the
    // *parallel* analyze path, which needs four or more changed paths before a
    // second worker starts, and no shape has that many worktree deltas. A shape
    // added for it would cost every run, so it is pinned in the unit tests
    // instead (`reversing_moves_the_worktree_side_onto_the_pre_image`).
    // A repository that has hooks, run from a directory below its top level.
    //
    // No shape carried a hook and no case combined one with a working directory,
    // so this pair was unreachable — and it was not a subtle gap: committing from
    // a subdirectory of a repository with *any* hook exited 1 with `No such file
    // or directory`, because the hook's path and cwd were resolved against a
    // directory that had already moved. A `pre-commit` that only runs `exit 0`
    // reproduces it, so these cases need nothing clever, only the combination.
    for (cmd, args) in [
        ("status", &["status", "--porcelain"][..]),
        ("commit", &["commit", "-q", "--allow-empty", "-m", "from a subdirectory"][..]),
        ("add", &["add", "nested.txt"][..]),
        ("stash", &["stash", "list"][..]),
    ] {
        c.push(Case::new(cmd, args, Shape::Hooked).in_dir("sub"));
        // The same verb at the top level, so a failure names the working
        // directory as the difference rather than leaving it as one of two.
        c.push(Case::new(cmd, args, Shape::Hooked));
    }
    // Two argument checks git makes before it does anything, both found by a
    // generated draw and pinned here so they are measured every run rather than
    // whenever a seed happens to reach them. Git refuses each with `fatal:` and
    // 128 before touching the repository; the port runs the command and exits 1.
    //
    //   --author '-1'   git: fatal, 128, after searching existing authors and
    //                   finding none. The port refuses honestly — "only the
    //                   `Name <email>` form is supported (author search is not
    //                   ported)" — so this scores as `unsupported`, which is a
    //                   failure here by design: an unported feature is the gap
    //                   being measured, not a skip.
    //   --reset-author  git: fatal, 128, "can be used only with -C, -c or --amend".
    //                   The port exits **0 and writes the commit**, so an invalid
    //                   flag combination silently produces one nobody asked for.
    //
    // Deliberately curated while still failing: a defect that only a lucky seed
    // reveals is one a green run can hide.
    c.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "x", "--author=-1"],
        Shape::Linear,
    ));
    c.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "x", "--reset-author"],
        Shape::Linear,
    ));

    // `--no-verify` must skip the hooks, and the `commit-msg` hook's trailer is
    // what makes that observable: with it the message gains a line, without it
    // the message is exactly what was passed.
    c.push(Case::new(
        "commit",
        &["commit", "-q", "--allow-empty", "--no-verify", "-m", "unverified"],
        Shape::Hooked,
    ));

    c.push(Case::new("diff", &["diff", "-R"], Shape::Dirty));
    c.push(Case::new("diff", &["diff", "-R", "--stat"], Shape::Dirty));
    c.push(Case::new("diff", &["diff", "-R", "--raw"], Shape::Dirty));
    c.push(Case::new("diff", &["diff", "-R", "--name-status"], Shape::Dirty));
    c.push(Case::new("diff", &["diff", "-R", "HEAD"], Shape::Dirty));
    c.push(Case::new("diff", &["diff", "-R", "-w"], Shape::Whitespace));
    c.push(Case::new("diff", &["diff", "-R"], Shape::Submodule));
    // `apply` over-stripping names the *header's* line, not the name line: git
    // returns NULL from `find_name_common()` and the caller reports. `-p1` says
    // "component" where `-p9` says "components".
    c.push(Case::new("apply", &["apply", "-p9", "patches/valid.patch"], Shape::Patches));
    c.push(Case::new("apply", &["apply", "-p1", "patches/valid.patch"], Shape::Patches));
    read_only("blame", &["blame", "README.md"], &mut c);
    read_only("blame", &["blame", "--porcelain", "README.md"], &mut c);

    // ---- refs ----
    read_only("branch", &["branch"], &mut c);
    read_only("branch", &["branch", "--list"], &mut c);
    read_only("branch", &["branch", "-a"], &mut c);
    read_only("branch", &["branch", "--show-current"], &mut c);
    read_only("tag", &["tag"], &mut c);
    c.push(Case::new("tag", &["tag", "--list"], Shape::Branched));
    c.push(Case::new("tag", &["tag", "-l", "v0.*"], Shape::Branched));
    read_only("describe", &["describe", "--always"], &mut c);
    c.push(Case::new("describe", &["describe", "--tags"], Shape::Branched));

    // ---- config / remote ----
    read_only("config", &["config", "--get", "core.bare"], &mut c);
    read_only("config", &["config", "--list"], &mut c);
    read_only("remote", &["remote"], &mut c);
    read_only("remote", &["remote", "-v"], &mut c);

    // ---- an index file that is not there, which readers must treat as empty ----
    // A repository that has never staged anything has no index file, and every
    // `ls-files` mode still has to work: `do_read_index()` only dies on an
    // `open()` failure when the caller demanded the file, and `ls-files` reaches
    // it through `read_index_from()` with `must_exist == 0`, so `ENOENT` yields
    // an initialized-but-empty index — the cached list prints nothing and exits
    // 0 while `-o` still walks the work tree. Treating it as an error instead is
    // a defect the port shipped, and until now it was pinned only by an
    // integration test, which measures one binary rather than two.
    //
    // No shape is added for it. Three routes to a repository whose index file is
    // absent were measured against stock 2.55.0 first, and only the third has
    // both halves agreeing today:
    //
    //  * `cwd` into [`Shape::BehindRemote`]'s bare `.remote.git`, which genuinely
    //    has no index. Plain `ls-files` agrees there (nothing, exit 0), but `-o`
    //    needs a work tree and the two sides refuse differently — stock
    //    `fatal: this operation must be run in a work tree` at 128, the port
    //    `zvcs: ls-files: …` at 1. That is a live divergence of its own and not
    //    this group's to pin, so the route cannot carry the `-o` half.
    //  * the same bare repository handed a work tree through `GIT_WORK_TREE`,
    //    which stock accepts and the port refuses in the same words as above.
    //  * `GIT_INDEX_FILE` naming a file that is not there, over an ordinary
    //    shape. It is the same `open()` returning `ENOENT` on the index path, so
    //    it reaches the same code, and both sides agree on both halves: nothing
    //    from `ls-files`, and `README.md`/`src/lib.rs` — every tracked file, now
    //    untracked because the index is empty — from `ls-files -o`.
    c.push(Case::new("ls-files", &["ls-files"], Shape::Linear).with_env(NO_INDEX));
    c.push(Case::new("ls-files", &["ls-files", "-o"], Shape::Linear).with_env(NO_INDEX));

    // ---- mutating: each case runs against its own pristine copy ----
    c.push(Case::new("add", &["add", "untracked.txt"], Shape::Dirty));
    c.push(Case::new("add", &["add", "-A"], Shape::Dirty));
    c.push(Case::new("add", &["add", "."], Shape::Dirty));
    c.push(Case::new("rm", &["rm", "--cached", "README.md"], Shape::Linear));
    c.push(Case::new("rm", &["rm", "-f", "README.md"], Shape::Linear));
    c.push(Case::new("mv", &["mv", "README.md", "DOCS.md"], Shape::Linear));
    c.push(Case::new("restore", &["restore", "README.md"], Shape::Dirty));
    c.push(Case::new("restore", &["restore", "--staged", "staged.txt"], Shape::Dirty));
    c.push(Case::new("reset", &["reset"], Shape::Dirty));
    c.push(Case::new("reset", &["reset", "--hard"], Shape::Dirty));
    c.push(Case::new("reset", &["reset", "--soft", "HEAD~1"], Shape::Branched));
    c.push(Case::new("reset", &["reset", "--mixed", "HEAD"], Shape::Dirty));
    // `reset --merge` over an unmerged index is the documented way out of a
    // conflicted merge, and it is the path `revert --abort` takes. It needs
    // `repo_read_index_unmerged()`, which collapses the stage 1/2/3 entries to a
    // `CE_CONFLICTED` marker that `unpack_trees` skips `verify_uptodate` for;
    // without it the command refused with `Entry '<p>' not uptodate` and exit 128
    // where stock succeeds. No other case reaches it — the whole `reset` family
    // ran against clean or merely dirty shapes.
    c.push(Case::new("reset", &["reset", "--merge"], Shape::Conflicted));
    c.push(Case::new("reset", &["reset", "--merge", "HEAD"], Shape::Conflicted));
    // `reset` in a bare repository, where there is no work tree to reset. Both
    // refusals are decided by two adjacent lines of `cmd_reset()`, in this order:
    //
    //     if (reset_type != SOFT && (reset_type != MIXED || repo_get_work_tree(…)))
    //             setup_work_tree(the_repository);
    //
    //     if (reset_type == MIXED && is_bare_repository())
    //             die(_("%s reset is not allowed in a bare repository"), …);
    //
    // `--hard` satisfies the first condition, so it dies inside
    // `setup_work_tree()` with `this operation must be run in a work tree` and
    // never reaches the second. `--mixed` is excused from the first *because*
    // `repo_get_work_tree()` is NULL here, falls through, and is then rejected by
    // name: `mixed reset is not allowed in a bare repository`. Both exit 128, so
    // the exit code cannot tell the two apart and the message *is* the behaviour
    // — these are `Case::strict`, compared on stderr byte for byte, the same rule
    // the refusal cases in `nested` follow. `Shape::BehindRemote`'s `.remote.git`
    // is the bare repository and is already in the fixture, so no shape is added.
    c.push(Case::strict("reset", &["reset", "--mixed"], Shape::BehindRemote).in_dir(".remote.git"));
    c.push(Case::strict("reset", &["reset", "--hard"], Shape::BehindRemote).in_dir(".remote.git"));

    // ---- the work-tree gate, in a bare repository ----
    //
    // git runs `setup_work_tree()` from `run_builtin()` for the 24 commands
    // flagged `NEED_WORK_TREE` (git.c:499-500), before the builtin is entered and
    // with a lone `-h` exempted (git.c:474-477). Without that gate `commit -m`
    // *created a commit* in a bare repository, `switch` printed git's fatal and
    // then exited 0 anyway, and `merge` reported `Already up to date.` — none of
    // it reachable by a case, because every other case runs in a work tree.
    //
    // All 24 verbs now pass through one `if`, so 24 rows would test one branch 24
    // times. These are chosen for what each *alone* would catch if the gate came
    // undone, which is the property worth paying fixture time for.
    let bare = |cmd: &'static str, args: &'static [&'static str]| -> Case {
        Case::strict(cmd, args, Shape::BehindRemote).in_dir(".remote.git")
    };
    // The mutation, and the reason the gate exists. Caught twice over: the exit
    // code, and a post-state that must not have gained a commit.
    c.push(bare("commit", &["commit", "-m", "bare commit"]));
    // The other mutating verb, and a different pre-fix failure shape.
    c.push(bare("stash", &["stash"]));
    // "Succeeded plausibly": exit 0, and no state change to betray it, so only
    // stdout and the exit code can catch a regression here.
    c.push(bare("merge", &["merge", "main"]));
    // Printed git's fatal on stderr and *then* exited 0 — stderr looks right, so
    // the exit code is the only witness. `switch` over `checkout`: same path,
    // and far fewer existing cases competing to notice.
    c.push(bare("switch", &["switch", "main"]));
    // Ordering: the gate must outrank parse-options, which used to answer 129
    // first. This is what regresses when a verb's argument parsing is rewritten.
    c.push(bare("merge-recursive", &["merge-recursive"]));
    // The cheapest row on the most-used gated verb.
    c.push(bare("status", &["status"]));
    // The exemption. Without it, deleting the `-h` check still passes everything.
    c.push(bare("status", &["status", "-h"]));
    // Not the table gate — `ls-files` gates per *option* (builtin/ls-files.c:707),
    // and the gate must also sit ahead of the `-i` guards it used to lose to.
    c.push(bare("ls-files", &["ls-files", "-o"]));
    c.push(bare("ls-files", &["ls-files", "-i"]));
    // `grep`'s gate is conditional too, and `verify_non_filename` must stand down
    // outside a work tree (setup.c:299) or a revision collides with a stray file.
    c.push(bare("grep", &["grep", "-n", "line"]));
    c.push(bare("grep", &["grep", "-n", "line", "HEAD"]));
    // The positive case. Without it the whole group is satisfiable by a port that
    // simply refuses everything in a bare repository.
    c.push(
        Case::new("ls-files", &["ls-files", "-o"], Shape::BehindRemote)
            .in_dir(".remote.git")
            .with_env(&[("GIT_WORK_TREE", "{repo}")]),
    );
    c.push(Case::new("commit", &["commit", "-m", "parity commit"], Shape::Dirty));
    c.push(Case::new("commit", &["commit", "-am", "parity commit all"], Shape::Dirty));
    c.push(Case::new("commit", &["commit", "--allow-empty", "-m", "empty"], Shape::Linear));
    c.push(Case::new("checkout", &["checkout", "feature"], Shape::Branched));
    c.push(Case::new("checkout", &["checkout", "-b", "newbranch"], Shape::Linear));
    c.push(Case::new("checkout", &["checkout", "--detach", "HEAD"], Shape::Linear));
    c.push(Case::new("switch", &["switch", "feature"], Shape::Branched));
    c.push(Case::new("switch", &["switch", "-c", "created"], Shape::Linear));
    c.push(Case::new("branch", &["branch", "topic"], Shape::Linear));
    c.push(Case::new("branch", &["branch", "-d", "feature"], Shape::Branched));
    c.push(Case::new("branch", &["branch", "-m", "renamed"], Shape::Linear));
    c.push(Case::new("tag", &["tag", "v9.9.9"], Shape::Linear));
    c.push(Case::new("tag", &["tag", "-a", "v9.9.9", "-m", "annotated"], Shape::Linear));
    c.push(Case::new("tag", &["tag", "-d", "v0.1.0"], Shape::Branched));
    c.push(Case::new("stash", &["stash"], Shape::Dirty));
    c.push(Case::new("stash", &["stash", "list"], Shape::Dirty));
    c.push(Case::new("stash", &["stash", "push", "-m", "wip"], Shape::Dirty));
    c.push(Case::new("merge", &["merge", "feature"], Shape::Branched));
    c.push(Case::new("merge", &["merge", "--no-ff", "feature"], Shape::Branched));
    c.push(Case::new("merge", &["merge", "--abort"], Shape::Conflicted));
    c.push(Case::new("config", &["config", "user.name", "someone"], Shape::Linear));
    c.push(Case::new("remote", &["remote", "add", "upstream", "https://example.invalid/r.git"], Shape::Linear));
    c.push(Case::new("init", &["init"], Shape::Linear));

    // ---- error paths: agreeing on failure is part of compatibility ----
    read_only("rev-parse", &["rev-parse", "does-not-exist"], &mut c);
    read_only("cat-file", &["cat-file", "-t", "does-not-exist"], &mut c);
    read_only("log", &["log", "does-not-exist"], &mut c);
    read_only("branch", &["branch", "-d", "no-such-branch"], &mut c);
    read_only("show", &["show", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"], &mut c);

    // ---- commands whose input arrives on stdin ----
    stdin_driven(&mut c);

    // ---- per-subsystem corpora, one module each ----
    diff_family::cases(&mut c);
    discovery::cases(&mut c);
    history_rewrite::cases(&mut c);
    info_attrs::cases(&mut c);
    mail_patch::cases(&mut c);
    maintenance::cases(&mut c);
    merge_dirty::cases(&mut c);
    merge_family::cases(&mut c);
    misc_commands::cases(&mut c);
    plumbing_objects::cases(&mut c);
    plumbing_refs::cases(&mut c);
    shape_reach::cases(&mut c);
    transport_local::cases(&mut c);
    worktree_index::cases(&mut c);
    config_and_globals(&mut c);

    // ---- format-patch: the three ways of naming where the series goes ----
    //
    // `-o`/`--output-directory` is not an `OPT_STRING` — it is a callback that
    // refuses to overwrite a value it already holds
    // (`output_directory_callback()`, builtin/log.c:1593-1603), so a second one
    // is `fatal: two output directories?` (128) rather than "last one wins".
    // Every case below was exit 0 here until that callback was ported, which is
    // the shape of failure the corpus is least likely to catch by accident:
    // `-o` alone already had a case, and it passed.
    //
    // The die fires inside `parse_options()`, which walks the whole argv before
    // `setup_revisions()` runs, so it preempts what comes after it on the line —
    // hence the trailing revision-shaped junk on one of them.
    for shape in [Shape::Branched, Shape::Dirty] {
        // The reported fuzz case, plus the same duplicate in each of the four
        // spellings the option has. All four reach the one callback, so a port
        // that guards only the separate-argument form still fails here.
        c.push(Case::new("format-patch", &["format-patch", "-o", "patches", "-o", "patches"], shape));
        c.push(Case::new(
            "format-patch",
            &["format-patch", "-o", "patches", "--indent-heuristic", "-o", "patches"],
            shape,
        ));
        c.push(Case::new(
            "format-patch",
            &["format-patch", "--output-directory=patches", "-o", "other", "-1"],
            shape,
        ));
        c.push(Case::new("format-patch", &["format-patch", "-opatches", "-oother", "-1"], shape));
        // `*dir` is a pointer test, so the empty string is a directory too.
        c.push(Case::new("format-patch", &["format-patch", "-o", "", "-o", "patches", "-1"], shape));
        // Preemption: the second `-o` is reached during option parsing, before
        // the revision `nosuchrev` is resolved.
        c.push(Case::new(
            "format-patch",
            &["format-patch", "-o", "patches", "-o", "other", "nosuchrev"],
            shape,
        ));
        // …and before the `--stdout` incompatibility check, which runs after
        // `parse_options()` returns (builtin/log.c:2250-2251).
        c.push(Case::new(
            "format-patch",
            &["format-patch", "--stdout", "-o", "patches", "-o", "other", "-1"],
            shape,
        ));

        // `die_for_incompatible_opt3(use_stdout, "--stdout", close_file,
        // "--output", !!output_directory, "--output-directory")` — the wording
        // depends on how many of the three were named
        // (`die_for_incompatible_opt4()`, parse-options.c:1528-1558), and
        // `--output` with `--output-directory` was not being caught at all.
        c.push(Case::new(
            "format-patch",
            &["format-patch", "--stdout", "--output", "s.patch", "--output-directory", "d", "-1"],
            shape,
        ));
        c.push(Case::new(
            "format-patch",
            &["format-patch", "--output", "s.patch", "--output-directory", "d", "-1"],
            shape,
        ));
        c.push(Case::new(
            "format-patch",
            &["format-patch", "--stdout", "--output-directory", "d", "-1"],
            shape,
        ));

        // The two that must still be accepted, so the guard cannot be "refuse
        // `-o`": one `-o`, and `format.outputDirectory` beside it. The config is
        // a *different* variable (`cfg.config_output_directory`, builtin/log.c:895)
        // merged in only at builtin/log.c:2261-2262 — after both checks above —
        // so it is neither a second output directory nor a reason to refuse
        // `--stdout`.
        c.push(Case::new("format-patch", &["format-patch", "-o", "patches", "-1"], shape));
        c.push(Case::new(
            "format-patch",
            &["-c", "format.outputDirectory=cfgdir", "format-patch", "-o", "patches", "-1"],
            shape,
        ));
        c.push(Case::new(
            "format-patch",
            &["-c", "format.outputDirectory=cfgdir", "format-patch", "--stdout", "-1"],
            shape,
        ));
        c.push(Case::new(
            "format-patch",
            &["-c", "format.outputDirectory=cfgdir", "format-patch", "-1"],
            shape,
        ));
    }

    c
}

// ---------------------------------------------------------------------------
// stdin-driven commands
// ---------------------------------------------------------------------------
//
// Until `Case` grew a stdin field, every case ran with stdin on `/dev/null`.
// That left two blind spots this section closes:
//
//   * commands whose *entire* payload is stdin — `mktag`, `mktree`,
//     `unpack-objects`, `get-tar-commit-id`, `show-index`, `stripspace`,
//     `patch-id`, `mailinfo`, `column`. They could only ever be exercised on
//     empty input, so a 100% score for them meant "agrees on nothing".
//   * the `--stdin` / `--stdin-paths` mode of `hash-object`, `index-pack`,
//     `pack-objects`, `update-ref`, `rev-list`, `for-each-ref`, `name-rev`,
//     `diff-tree`, `apply`, `am`, `fmt-merge-msg`, `check-ignore` and
//     `check-attr` — most notably `update-ref --stdin`, whose whole point is
//     transactional multi-ref update and which had never been run at all.
//
// Every payload below is a literal, so a case is reproducible from the source
// alone. Where a payload has to name an object, it names one that is *content*
// addressed (the fixture's `README.md` blob) rather than a commit id: a blob id
// is a function of file content only, so it survives any change to the fixture's
// history, dates or branch layout. If the fixture's file content ever does
// change, those cases degrade to an error-path comparison on both sides rather
// than silently passing.

/// Fixture text with comments, trailing whitespace and runs of blank lines —
/// every class of edit `stripspace` is specified to make.
const MESSY_TEXT: &[u8] = b"# a comment\n\n\n  \nhello   \n\n\nworld\t\n\n\n";

/// Four short words, one per line: enough for `column` to lay out.
const WORD_LINES: &[u8] = b"alpha\nbeta\ngamma\ndelta\nepsilon\n";

/// Paths that exist in every shape, for the `--stdin` path filters.
const FIXTURE_PATHS: &[u8] = b"README.md\nsrc/lib.rs\nno/such/file.log\n";

/// Arbitrary blob content, so `hash-object --stdin` has a hash to get wrong.
const BLOB_PAYLOAD: &[u8] = b"parity stdin payload\n";

/// One `ls-tree`-format line naming the canonical empty blob. `--missing` lets
/// `mktree` accept it without the object being present, which keeps the case
/// independent of what the fixture happens to contain.
const TREE_LINE: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tfoo.txt\n";

/// Same, NUL-terminated, for `mktree -z`.
const TREE_LINE_Z: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tfoo.txt\0";

/// A well-formed tag object over the fixture's README blob.
const TAG_BODY: &[u8] = b"object 9741694d75caeb49d3b7c1f59451c0c56bf6216c\n\
type blob\n\
tag parity-blob-tag\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
tagging the fixture readme\n";

/// A tag object missing its `type` line: git rejects it via fsck.
const TAG_BODY_MALFORMED: &[u8] = b"object 9741694d75caeb49d3b7c1f59451c0c56bf6216c\n\
tag broken\n";

/// A unified diff that applies cleanly to the fixture's `README.md`.
const README_PATCH: &[u8] = b"diff --git a/README.md b/README.md\n\
index 9741694..2a1b3c4 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n\
 # fixture\n\
+added line\n";

/// A diff whose hunk header does not match its body — `apply` must reject it.
const BROKEN_PATCH: &[u8] = b"diff --git a/README.md b/README.md\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1,5 +1,5 @@\n\
 nothing like the real file\n";

/// A single-patch mbox with fixed headers, so `am` produces a fixed commit id.
///
/// `pub(crate)` because [`crate::fuzz`] feeds it to the generated `am` walks as
/// well: applying it twice is the cheapest way to park a repository in
/// `.git/rebase-apply/` with no corrupt input to manufacture, and a second
/// mailbox literal written beside this one would be two things to keep in step
/// for no gain.
pub(crate) const MBOX: &[u8] = b"From 1234567890abcdef1234567890abcdef12345678 Mon Sep 17 00:00:00 2001\n\
From: Example Author <author@example.invalid>\n\
Date: Tue, 14 Nov 2023 22:13:20 +0000\n\
Subject: [PATCH] add a line to README\n\
\n\
commit body\n\
---\n\
 README.md | 1 +\n\
 1 file changed, 1 insertion(+)\n\
\n\
diff --git a/README.md b/README.md\n\
index 9741694..2a1b3c4 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n\
 # fixture\n\
+added line\n\
-- \n\
2.55.0\n\
\n";

/// The canonical empty packfile: `PACK`, version 2, zero objects, trailing
/// SHA-1 of those twelve bytes. Every git that can write a pack writes exactly
/// these 32 bytes for an empty object list, so it is safe as a literal.
const EMPTY_PACK: &[u8] = b"PACK\x00\x00\x00\x02\x00\x00\x00\x00\
\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Bytes that are not a pack, an index, or a tar: the reject path for the three
/// commands whose real input is binary and too large to embed.
const NOT_BINARY: &[u8] = b"this is not the binary format you are looking for\n";

/// Push one stdin-fed case.
fn si(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, shape, input));
}

fn stdin_driven(c: &mut Vec<Case>) {
    // ---- pure stdin→stdout filters: the strongest evidence the bytes arrive ----
    si("stripspace", &["stripspace"], Shape::Linear, MESSY_TEXT, c);
    si("stripspace", &["stripspace", "--strip-comments"], Shape::Linear, MESSY_TEXT, c);
    si("stripspace", &["stripspace", "--comment-lines"], Shape::Linear, WORD_LINES, c);
    si("column", &["column", "--mode=plain"], Shape::Linear, WORD_LINES, c);
    si("column", &["column", "--mode=row", "--width=20"], Shape::Linear, WORD_LINES, c);
    si("column", &["column", "--mode=column", "--width=30", "--padding=2"], Shape::Linear, WORD_LINES, c);
    si("patch-id", &["patch-id"], Shape::Linear, README_PATCH, c);
    si("patch-id", &["patch-id", "--stable"], Shape::Linear, README_PATCH, c);
    si("patch-id", &["patch-id", "--raw"], Shape::Linear, README_PATCH, c);

    // ---- hash-object: stdout is a hash *of the stdin bytes* ----
    si("hash-object", &["hash-object", "--stdin"], Shape::Linear, BLOB_PAYLOAD, c);
    si("hash-object", &["hash-object", "-t", "blob", "--stdin"], Shape::Linear, BLOB_PAYLOAD, c);
    // `-w` also has to leave the object behind, which the state probe checks.
    si("hash-object", &["hash-object", "-w", "--stdin"], Shape::Linear, BLOB_PAYLOAD, c);
    si("hash-object", &["hash-object", "--stdin-paths"], Shape::Linear, b"README.md\n", c);
    si("hash-object", &["hash-object", "-w", "--stdin-paths"], Shape::Linear, b"README.md\nsrc/lib.rs\n", c);
    si("hash-object", &["hash-object", "--stdin-paths"], Shape::Linear, b"no/such/file\n", c);

    // ---- tree and tag construction ----
    si("mktree", &["mktree", "--missing"], Shape::Linear, TREE_LINE, c);
    si("mktree", &["mktree", "--missing", "-z"], Shape::Linear, TREE_LINE_Z, c);
    si("mktree", &["mktree"], Shape::Linear, b"garbage\n", c);
    si("mktag", &["mktag"], Shape::Linear, TAG_BODY, c);
    si("mktag", &["mktag"], Shape::Linear, TAG_BODY_MALFORMED, c);

    // ---- patch application ----
    si("apply", &["apply", "--stat"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply", "--numstat"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply", "--check"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply", "--cached"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply", "--reverse", "--check"], Shape::Linear, README_PATCH, c);
    si("apply", &["apply"], Shape::Linear, BROKEN_PATCH, c);

    // ---- mail handling ----
    si("mailinfo", &["mailinfo", "msg.txt", "patch.txt"], Shape::Linear, MBOX, c);
    si("mailinfo", &["mailinfo", "-k", "msg.txt", "patch.txt"], Shape::Linear, MBOX, c);
    si("am", &["am"], Shape::Linear, MBOX, c);
    si("am", &["am", "--signoff"], Shape::Linear, MBOX, c);

    // ---- update-ref --stdin: transactional, and previously never run ----
    si("update-ref", &["update-ref", "--stdin"],
       Shape::Linear, b"create refs/heads/parity-a HEAD\ncreate refs/heads/parity-b HEAD\n", c);
    // The second command fails, so an implementation with a real transaction
    // leaves `parity-a` uncreated. Anything that applies updates as it reads
    // them shows up as a state diff.
    si("update-ref", &["update-ref", "--stdin"],
       Shape::Linear, b"create refs/heads/parity-a HEAD\ncreate refs/heads/main HEAD\n", c);
    si("update-ref", &["update-ref", "--stdin"],
       Shape::Branched, b"start\ncreate refs/heads/parity-a HEAD\ncommit\n", c);
    si("update-ref", &["update-ref", "--stdin"], Shape::Branched, b"delete refs/heads/feature\n", c);
    si("update-ref", &["update-ref", "--stdin"], Shape::Branched, b"update refs/heads/feature HEAD\n", c);
    si("update-ref", &["update-ref", "--stdin"], Shape::Linear, b"verify refs/heads/main\n", c);

    // ---- revision and ref listing fed from stdin ----
    si("rev-list", &["rev-list", "--stdin"], Shape::Branched, b"HEAD\n", c);
    si("rev-list", &["rev-list", "--stdin", "--count"], Shape::Branched, b"HEAD\n^HEAD\n", c);
    si("rev-list", &["rev-list", "--stdin", "--format=%H"], Shape::Merged, b"HEAD\n", c);
    si("for-each-ref", &["for-each-ref", "--stdin", "--format=%(refname)"],
       Shape::Branched, b"refs/heads/*\nrefs/tags/*\n", c);
    si("for-each-ref", &["for-each-ref", "--stdin"], Shape::Branched, b"refs/heads/*\n", c);
    si("name-rev", &["name-rev", "--annotate-stdin"], Shape::Branched, b"no object id on this line\n", c);
    si("name-rev", &["name-rev", "--annotate-stdin"], Shape::Branched, b"0000000000000000000000000000000000000000 zero\n", c);
    si("diff-tree", &["diff-tree", "--stdin", "-r", "--root"], Shape::Linear, b"HEAD\n", c);
    si("diff-tree", &["diff-tree", "--stdin", "-r"], Shape::Linear, b"0000000000000000000000000000000000000000\n", c);

    // ---- attribute and ignore lookups fed from stdin ----
    // `-n` makes non-matching paths print too, so stdout depends on the payload
    // even in a fixture with no ignore rules.
    si("check-ignore", &["check-ignore", "--stdin", "-v", "-n"], Shape::Linear, FIXTURE_PATHS, c);
    si("check-ignore", &["check-ignore", "--stdin"], Shape::Linear, FIXTURE_PATHS, c);
    si("check-ignore", &["check-ignore", "--stdin", "--no-index", "-v", "-n"], Shape::Dirty, FIXTURE_PATHS, c);
    si("check-attr", &["check-attr", "text", "--stdin"], Shape::Linear, FIXTURE_PATHS, c);
    si("check-attr", &["check-attr", "text", "eol", "--stdin"], Shape::Linear, FIXTURE_PATHS, c);
    si("check-attr", &["check-attr", "-a", "--stdin"], Shape::AwkwardPaths, FIXTURE_PATHS, c);

    // ---- packfile ingestion ----
    // The empty pack is the only pack small enough to be a literal; it still
    // separates "parses a pack" from "reads nothing and exits 0".
    si("unpack-objects", &["unpack-objects"], Shape::Linear, EMPTY_PACK, c);
    si("unpack-objects", &["unpack-objects", "-n"], Shape::Linear, EMPTY_PACK, c);
    si("unpack-objects", &["unpack-objects"], Shape::Linear, NOT_BINARY, c);
    si("index-pack", &["index-pack", "--stdin"], Shape::Linear, EMPTY_PACK, c);
    si("index-pack", &["index-pack", "--stdin"], Shape::Linear, NOT_BINARY, c);
    si("pack-objects", &["pack-objects", "--revs", "--stdout"], Shape::Linear, b"HEAD\n", c);
    si("pack-objects", &["pack-objects", "--stdout"], Shape::Linear, b"", c);
    si("pack-objects", &["pack-objects", "--stdout"], Shape::Linear, b"0000000000000000000000000000000000000000\n", c);

    // ---- the three whose real input is binary and too large to embed ----
    // Only the reject path is reachable from a literal. Reading a real `.idx` or
    // tar from disk would make the case depend on run-time state, which is worse
    // than measuring less; noted so the gap is visible rather than inferred.
    si("show-index", &["show-index"], Shape::Linear, NOT_BINARY, c);
    si("get-tar-commit-id", &["get-tar-commit-id"], Shape::Linear, NOT_BINARY, c);

    // ---- merge message formatting from a FETCH_HEAD-shaped payload ----
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched,
       b"0000000000000000000000000000000000000000\t\tbranch 'feature' of ../upstream\n", c);
    si("fmt-merge-msg", &["fmt-merge-msg"], Shape::Branched, b"not a fetch-head line\n", c);
}

// ---------------------------------------------------------------------------
// Configuration and global options
// ---------------------------------------------------------------------------
//
// `-c key=value` and the options `git.c:handle_options` parses before the verb
// are sampled across every grammar by the fuzzer, which is breadth. This is the
// floor underneath it: a handful of settings and options whose effect on stdout
// is unmistakable, on shapes chosen so the effect has something to act on.
//
// The point of curating them as well as sampling them is that a port which
// ignores `-c` outright, or which never reaches `handle_options` at all, fails
// here on a named case in every run — instead of failing on whichever sampled
// combination happens to be drawn under this seed.

/// One case per (config, argv, shape) triple.
fn cfg(
    cmd: &'static str,
    config: &[(&str, &str)],
    args: &[&str],
    shape: Shape,
    out: &mut Vec<Case>,
) {
    out.push(Case::new(cmd, args, shape).with_config(config));
}

/// One case per (global options, argv, shape) triple.
fn glob(
    cmd: &'static str,
    globals: &[&[&str]],
    args: &[&str],
    shape: Shape,
    out: &mut Vec<Case>,
) {
    out.push(Case::new(cmd, args, shape).with_globals(globals));
}

/// Configuration keys and global options whose effect is visible on stdout.
fn config_and_globals(out: &mut Vec<Case>) {
    // ---- core.abbrev: the width of every id git prints ----
    // Two widths, so a port that hard-codes 7 fails one of them whichever it
    // picked, and `40`/`no` which ask for no abbreviation at all.
    cfg("log", &[("core.abbrev", "12")], &["log", "--oneline", "-1"], Shape::Branched, out);
    cfg("log", &[("core.abbrev", "4")], &["log", "--oneline", "-1"], Shape::Branched, out);
    cfg("rev-parse", &[("core.abbrev", "16")], &["rev-parse", "--short", "HEAD"], Shape::Linear, out);
    cfg("rev-parse", &[("core.abbrev", "no")], &["rev-parse", "--short", "HEAD"], Shape::Linear, out);
    cfg("ls-tree", &[("core.abbrev", "10")], &["ls-tree", "--abbrev", "HEAD"], Shape::Linear, out);

    // ---- diff.renames: whether a rename is a rename ----
    // On the one shape that contains renames, so the setting decides between
    // `R100 orig/alpha.txt moved/alpha.txt` and a delete plus an add.
    for value in ["true", "false", "copies"] {
        cfg(
            "diff",
            &[("diff.renames", value)],
            &["diff", "--name-status", "HEAD~2", "HEAD"],
            Shape::Renamed,
            out,
        );
    }
    // A limit of one pair, which git reports by refusing to detect at all.
    cfg(
        "diff",
        &[("diff.renames", "true"), ("diff.renameLimit", "1")],
        &["diff", "--name-status", "HEAD~2", "HEAD"],
        Shape::Renamed,
        out,
    );

    // ---- diff hunk shape ----
    cfg("diff", &[("diff.context", "1")], &["diff", "HEAD~1", "HEAD"], Shape::Renamed, out);
    cfg("diff", &[("diff.noprefix", "true")], &["diff", "HEAD~1", "HEAD"], Shape::Renamed, out);
    cfg("diff", &[("diff.mnemonicPrefix", "true")], &["diff", "HEAD"], Shape::Dirty, out);
    for algo in ["patience", "histogram", "minimal"] {
        cfg("diff", &[("diff.algorithm", algo)], &["diff", "HEAD~1", "HEAD"], Shape::Whitespace, out);
    }

    // ---- status: what the most-read porcelain decides to print ----
    for value in ["no", "normal", "all"] {
        cfg("status", &[("status.showUntrackedFiles", value)], &["status", "--porcelain"], Shape::Dirty, out);
    }
    cfg("status", &[("status.short", "true")], &["status"], Shape::Dirty, out);
    cfg("status", &[("status.short", "true"), ("status.branch", "true")], &["status"], Shape::Dirty, out);
    cfg("status", &[("status.relativePaths", "false")], &["status"], Shape::Dirty, out);
    // Colour is otherwise unreachable: `env::harden` sets `NO_COLOR` and pins
    // `TERM=dumb`, so `color.ui=always` is the only way a case asks for escape
    // sequences — and it is the one value that overrides both.
    cfg("status", &[("color.ui", "always")], &["status", "--short"], Shape::Dirty, out);

    // ---- log: the defaults every walk inherits ----
    cfg("log", &[("log.abbrevCommit", "true")], &["log", "-1"], Shape::Branched, out);
    cfg("log", &[("log.decorate", "full")], &["log", "--oneline", "-2"], Shape::Branched, out);
    cfg("log", &[("log.decorate", "no")], &["log", "--oneline", "-2"], Shape::Branched, out);
    for value in ["iso", "short", "raw", "unix"] {
        cfg("log", &[("log.date", value)], &["log", "-1", "--pretty=%ad"], Shape::Branched, out);
    }
    // `pretty.<name>` defines a format that `--pretty=<name>` then resolves, so
    // one key reaches the whole placeholder language from the config side.
    cfg("log", &[("pretty.custom", "%H %s")], &["log", "-1", "--pretty=custom"], Shape::Branched, out);

    // ---- path quoting, on the shape whose paths need quoting ----
    for value in ["true", "false"] {
        cfg("ls-files", &[("core.quotePath", value)], &["ls-files"], Shape::AwkwardPaths, out);
        cfg("status", &[("core.quotePath", value)], &["status", "--porcelain"], Shape::AwkwardPaths, out);
    }

    // ---- ref ordering ----
    cfg("tag", &[("tag.sort", "-refname")], &["tag", "--list"], Shape::Branched, out);
    cfg("branch", &[("branch.sort", "-refname")], &["branch", "--list"], Shape::Branched, out);

    // ---- merge conflict rendering ----
    for style in ["merge", "diff3", "zdiff3"] {
        cfg("diff", &[("merge.conflictStyle", style)], &["diff"], Shape::Conflicted, out);
    }

    // ---- global options: the half of argv that precedes the subcommand ----
    glob("log", &[&["--no-pager"]], &["log", "--oneline", "-1"], Shape::Branched, out);
    glob("log", &[&["-P"]], &["log", "--oneline", "-1"], Shape::Branched, out);
    glob("status", &[&["--no-optional-locks"]], &["status", "--porcelain"], Shape::Dirty, out);
    glob("log", &[&["--no-replace-objects"]], &["log", "--oneline", "-1"], Shape::Linear, out);
    glob("status", &[&["--no-advice"]], &["status"], Shape::Conflicted, out);

    // `-C` moves before anything else happens, so `--show-prefix` reports where
    // it landed — the one query that separates "changed directory" from
    // "accepted the option and ignored it".
    glob("rev-parse", &[&["-C", "src"]], &["rev-parse", "--show-prefix"], Shape::Linear, out);
    glob("rev-parse", &[&["-C", "src"]], &["rev-parse", "--show-cdup"], Shape::Linear, out);
    glob("status", &[&["-C", "src"]], &["status", "--porcelain"], Shape::Dirty, out);
    // `--git-dir` and `--work-tree` after a `-C`, which is where their relative
    // resolution is decided.
    glob("rev-parse", &[&["-C", "src"], &["--git-dir=../.git"]], &["rev-parse", "--git-dir"], Shape::Linear, out);
    glob("rev-parse", &[&["--git-dir=.git"], &["--work-tree=."]], &["rev-parse", "--show-toplevel"], Shape::Linear, out);

    // Pathspec magic supplied globally rather than per pathspec. `*.md` is a
    // glob to `--glob-pathspecs`, a literal filename to `--literal-pathspecs`,
    // and neither to `--noglob-pathspecs`.
    for opt in ["--literal-pathspecs", "--glob-pathspecs", "--noglob-pathspecs"] {
        glob("ls-files", &[&[opt]], &["ls-files", "--", "*.md"], Shape::Linear, out);
    }
    glob("ls-files", &[&["--icase-pathspecs"]], &["ls-files", "--", "README.MD"], Shape::Linear, out);

    // A ref namespace, which relocates every ref read and written.
    glob("for-each-ref", &[&["--namespace=ns"]], &["for-each-ref"], Shape::Branched, out);
    glob("rev-parse", &[&["--namespace=ns"]], &["rev-parse", "--git-dir"], Shape::Branched, out);
    glob("show-ref", &[&["--namespace=ns"]], &["show-ref"], Shape::Branched, out);

    // Attributes read from a tree instead of from the worktree, on the shape
    // that has a `.gitattributes` with rules in it.
    glob("check-attr", &[&["--attr-source=HEAD"]], &["check-attr", "-a", "README.md"], Shape::Attributes, out);
    glob("check-attr", &[&["--attr-source=does-not-exist"]], &["check-attr", "-a", "README.md"], Shape::Attributes, out);
}
