//! Differential corpus cases for the merge_family subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # Where the merged bytes are actually asserted
//!
//! The runner compares stdout, exit code, and a state digest built from stock
//! git probes. Neither `status --porcelain` nor `ls-files --stage` reveals the
//! *content* of a file a merge just wrote, so a case that only writes to the
//! worktree proves nothing about the merge result. Three routes do assert bytes,
//! and every content case below takes one of them:
//!
//! * `merge-file -p` prints the merged text — the result is stdout.
//! * `merge-tree --write-tree` prints the resulting tree id, and the blobs it
//!   writes show up in the state digest's `cat-file --batch-check
//!   --batch-all-objects` probe. A different conflict marker, label, or hunk
//!   grouping changes the blob id, so it changes both surfaces.
//! * The strategy backends (`merge-recursive`, `merge-resolve`, `mergetool`)
//!   stage their result, so `ls-files --stage` carries the blob id.
//!
//! Cases that merely rewrite a worktree file in place are kept for their exit
//! code, and are marked as such.
//!
//! # Both text drivers are exercised
//!
//! `builtin/merge-file.c` asks xdiff for `XDL_MERGE_ZEALOUS_ALNUM` while
//! `merge-ll.c` asks for `XDL_MERGE_ZEALOUS`, so `git merge-file` and every
//! command that merges through the ll-merge driver can legitimately disagree
//! with each other on the same three inputs. Both are covered here:
//! `merge-file` directly, and the ll driver via `merge-tree --write-tree`,
//! `merge-recursive`, `merge-index`/`merge-one-file` and `mergetool`.
//!
//! # Fixture constraints these cases work around
//!
//! [`Shape`](crate::fixture::Shape) templates are fixed and the runner offers no
//! per-case file staging, no stdin, and no per-case environment: `Case` is
//! `(cmd, argv, shape)` and stdin is `Stdio::null()`. Two consequences shape the
//! choices below:
//!
//! * Every tracked fixture file is one or two lines long, which cannot express
//!   adjacent changed regions, a single unchanged line between two changes, or
//!   changes at both ends of a file. The only multi-line, byte-deterministic
//!   files present in a fixture are the hook samples `git init` writes into
//!   `.git/hooks/`. They are near-identical shell scripts that differ in several
//!   short, closely spaced regions, so a three-way merge across them produces
//!   exactly the multi-hunk layouts the `xdl_merge` port has to group correctly
//!   — several conflicts separated by a single `#` line, changes at line 1, and
//!   changes running to the last line. They are copied verbatim into both
//!   sides' repos, so they are identical across a comparison; their content does
//!   track the installed git version, which is the same property the rest of the
//!   harness already has.
//! * Nothing in any fixture lacks a trailing newline, so that edge is covered
//!   only in its degenerate form, with `.git/MERGE_MODE` (zero bytes) as an
//!   input.

use crate::fixture::Shape;
use crate::runner::Case;

/// Hook samples `git init` writes, used as multi-line three-way merge inputs.
/// Near-identical scripts: merging across them yields multiple conflict regions
/// separated by one unchanged line, which is the grouping `xdl_merge` decides.
const APPLYPATCH_MSG: &str = ".git/hooks/applypatch-msg.sample";
const PRE_APPLYPATCH: &str = ".git/hooks/pre-applypatch.sample";
const PRE_MERGE_COMMIT: &str = ".git/hooks/pre-merge-commit.sample";
const PRE_COMMIT: &str = ".git/hooks/pre-commit.sample";
const PRE_PUSH: &str = ".git/hooks/pre-push.sample";
const PREPARE_COMMIT_MSG: &str = ".git/hooks/prepare-commit-msg.sample";
const COMMIT_MSG: &str = ".git/hooks/commit-msg.sample";
const UPDATE: &str = ".git/hooks/update.sample";
const PRE_REBASE: &str = ".git/hooks/pre-rebase.sample";
/// The one sample that does not start with `#!/bin/sh`, so a triple containing
/// it conflicts on line 1 — the file-start edge.
const FSMONITOR: &str = ".git/hooks/fsmonitor-watchman.sample";
/// Zero-byte file present in the mid-merge fixture; stands in for the empty and
/// no-final-newline input edges.
const EMPTY: &str = ".git/MERGE_MODE";

/// A user-defined mergetool: `mergetool.<tool>.cmd` plus `trustExitCode` makes
/// `git mergetool` non-interactive, which is the only way it can be driven at
/// all under a null stdin. `cat "$LOCAL"`/`cat "$REMOTE"` pick a side, so the
/// staged blob differs between the two and the state digest can tell them apart.
const TOOL_TAKE_LOCAL: &str = "mergetool.parity.cmd=cat \"$LOCAL\" > \"$MERGED\"";
const TOOL_TAKE_REMOTE: &str = "mergetool.parity.cmd=cat \"$REMOTE\" > \"$MERGED\"";
const TOOL_TRUST: &str = "mergetool.parity.trustExitCode=true";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    merge_file(out);
    merge_index_and_one_file(out);
    merge_tree(out);
    strategy_backends(out);
    mergetool(out);
    rerere(out);
}

/// `merge-file`: the `ZEALOUS_ALNUM` text driver, printed to stdout.
fn merge_file(out: &mut Vec<Case>) {
    /// The hook samples live in every shape, so these run against the floor case.
    fn p(out: &mut Vec<Case>, args: &[&str]) {
        out.push(Case::new("merge-file", args, Shape::Linear));
    }
    macro_rules! p {
        ($($a:expr),* $(,)?) => { p(out, &[$($a),*]) };
    }

    // Three closely-related scripts. The merge splits into several conflict
    // regions separated by a single unchanged `#`, which is the hunk grouping
    // hunk-intersection got wrong and `xdl_merge` gets right.
    p!("merge-file", "-p", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--diff3", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--zdiff3", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--ours", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--theirs", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--union", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "-q", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!("merge-file", "-p", "--marker-size=10", APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT);
    p!(
        "merge-file", "-p", "-L", "ours", "-L", "base", "-L", "theirs",
        APPLYPATCH_MSG, PRE_APPLYPATCH, PRE_MERGE_COMMIT,
    );

    // Reordered so the two sides share lines *inside* the conflict region: this
    // is the triple on which `--zdiff3` hoists common lines out of the markers
    // and therefore differs from `--diff3`. Verified against stock: 42 lines
    // under `--diff3`, 41 under `--zdiff3`.
    p!("merge-file", "-p", APPLYPATCH_MSG, PRE_MERGE_COMMIT, PRE_APPLYPATCH);
    p!("merge-file", "-p", "--diff3", APPLYPATCH_MSG, PRE_MERGE_COMMIT, PRE_APPLYPATCH);
    p!("merge-file", "-p", "--zdiff3", APPLYPATCH_MSG, PRE_MERGE_COMMIT, PRE_APPLYPATCH);

    // `merge.conflictStyle` reaches merge-file through `git_xmerge_config`, so
    // the same three styles must be selectable without the command-line flags.
    for style in ["merge", "diff3", "zdiff3"] {
        p!(
            "-c", &format!("merge.conflictStyle={style}"), "merge-file", "-p",
            APPLYPATCH_MSG, PRE_MERGE_COMMIT, PRE_APPLYPATCH,
        );
    }

    // Larger inputs: many hunks, conflicts abutting each other, a conflict on
    // line 1 (only `fsmonitor-watchman.sample` starts with `#!/usr/bin/perl`),
    // and changes running to the final line.
    p!("merge-file", "-p", "--diff3", UPDATE, PRE_REBASE, FSMONITOR);
    p!("merge-file", "-p", "--diff3", PRE_REBASE, FSMONITOR, UPDATE);
    p!("merge-file", "-p", FSMONITOR, UPDATE, PRE_REBASE);
    p!("merge-file", "-p", COMMIT_MSG, PREPARE_COMMIT_MSG, PRE_COMMIT);
    p!("merge-file", "-p", PRE_COMMIT, PRE_PUSH, PREPARE_COMMIT_MSG);

    // Clean paths: identical inputs, and ours-unchanged so theirs is taken whole.
    p!("merge-file", "-p", PRE_COMMIT, PRE_COMMIT, PRE_COMMIT);
    p!("merge-file", "-p", PRE_COMMIT, PRE_COMMIT, PRE_PUSH);

    // Empty input on each of the three positions.
    out.push(Case::new("merge-file", &["merge-file", "-p", EMPTY, "README.md", "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("merge-file", &["merge-file", "-p", "README.md", EMPTY, "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("merge-file", &["merge-file", "-p", "--diff3", EMPTY, EMPTY, "conflict.txt"], Shape::Conflicted));

    // Conflict labels are the path names, so awkward bytes have to survive them.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "with space.txt", "README.md", "üñïçødé.txt"],
        Shape::AwkwardPaths,
    ));

    // Without `-p` the result is written back into file1. Only the exit code and
    // "the file is now modified" are observable; the bytes are not.
    out.push(Case::new("merge-file", &["merge-file", "README.md", "src/lib.rs", "main.txt"], Shape::Merged));
    out.push(Case::new("merge-file", &["merge-file", "-q", "main.txt", "README.md", "side.txt"], Shape::Merged));

    // `--object-id` merges blobs straight out of the object store.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--object-id", "main:main.txt", "main:README.md", "main:side.txt"],
        Shape::Merged,
    ));

    // Error paths.
    p!("merge-file", "-p", "no-such-file.txt", "README.md", "src/lib.rs");
    p!("merge-file", "-p", "--object-id", "deadbeef", "HEAD:README.md", "HEAD:src/lib.rs");
    p!("merge-file", "-p", "README.md");
}

/// `merge-index` drives a merge program over the unmerged index entries;
/// `merge-one-file` is the program git ships for that role.
fn merge_index_and_one_file(out: &mut Vec<Case>) {
    // `echo` as the merge program prints the stage triple git hands over, which
    // is the argument protocol itself.
    out.push(Case::new("merge-index", &["merge-index", "echo", "-a"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "echo", "--", "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "-o", "echo", "-a"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "-o", "-q", "echo", "-a"], Shape::Conflicted));
    // No unmerged entries: the program must never run.
    out.push(Case::new("merge-index", &["merge-index", "echo", "-a"], Shape::Linear));
    // The real pairing, which also exercises whether git's own helper is
    // reachable on the child's PATH.
    out.push(Case::new("merge-index", &["merge-index", "git-merge-one-file", "-a"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "-o", "git-merge-one-file", "-a"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "-o", "-q", "git-merge-one-file", "-a"], Shape::Conflicted));
    // Error paths.
    out.push(Case::new("merge-index", &["merge-index", "no-such-program", "-a"], Shape::Conflicted));
    out.push(Case::new("merge-index", &["merge-index", "-a"], Shape::Conflicted));

    // `merge-one-file` called directly, once with a real base (content conflict
    // through the ll driver) and once with the empty base an add/add produces.
    out.push(Case::new(
        "merge-one-file",
        &[
            "merge-one-file", "main^:README.md", "main:conflict.txt", "theirs:conflict.txt",
            "conflict.txt", "100644", "100644", "100644",
        ],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "merge-one-file",
        &[
            "merge-one-file", "", "main:conflict.txt", "theirs:conflict.txt",
            "conflict.txt", "", "100644", "100644",
        ],
        Shape::Conflicted,
    ));
    out.push(Case::new("merge-one-file", &["merge-one-file"], Shape::Conflicted));
}

/// `merge-tree` in both forms: the deprecated three-tree walk and `--write-tree`.
fn merge_tree(out: &mut Vec<Case>) {
    // Old form. In the mid-merge fixture `main^` is the common ancestor of
    // `main` and `theirs`, so no hash has to be spelled out.
    out.push(Case::new("merge-tree", &["merge-tree", "main^", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new("merge-tree", &["merge-tree", "main^", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-tree", &["merge-tree", "main^", "main", "main"], Shape::Branched));

    // New form. The printed tree id and the blobs written into the object store
    // are both compared, so the conflicted file's exact bytes are asserted.
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "--name-only", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "--no-messages", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "-z", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "--merge-base=main^", "main", "theirs"], Shape::Conflicted));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "--write-tree", "--allow-unrelated-histories", "main", "theirs"],
        Shape::Conflicted,
    ));

    // Conflict style changes the marker block and, under diff3/zdiff3, adds the
    // ancestor label carrying the merge base's abbreviated id — a different blob
    // and therefore a different tree id on stdout.
    for style in ["merge", "diff3", "zdiff3"] {
        out.push(Case::new(
            "merge-tree",
            &["-c", &format!("merge.conflictStyle={style}"), "merge-tree", "--write-tree", "main", "theirs"],
            Shape::Conflicted,
        ));
    }

    // Clean merges, including a merge whose result already exists.
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-tree", &["merge-tree", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "main", "side"], Shape::Merged));
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "HEAD", "HEAD"], Shape::Submodule));

    // Error paths: unknown ref, and an unreadable tree-ish in the old form.
    out.push(Case::new("merge-tree", &["merge-tree", "--write-tree", "no-such-ref", "main"], Shape::Linear));
    out.push(Case::new(
        "merge-tree",
        &["merge-tree", "main^", "main", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Conflicted,
    ));
}

/// The strategy backends `git merge` dispatches to. Each mutates the index and
/// worktree, so `ls-files --stage` and `status` carry the assertion.
fn strategy_backends(out: &mut Vec<Case>) {
    // Clean merge that really applies a commit: `feature` adds feature.txt.
    for cmd in [
        "merge-recursive",
        "merge-recursive-ours",
        "merge-recursive-theirs",
        "merge-subtree",
        "merge-resolve",
        "merge-ours",
    ] {
        out.push(Case::new(cmd, &[cmd, "main", "--", "main", "feature"], Shape::Branched));
    }
    // Same merge with the true common ancestor rather than a shortcut base.
    out.push(Case::new("merge-recursive", &["merge-recursive", "main^", "--", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-resolve", &["merge-resolve", "main^", "--", "main", "feature"], Shape::Branched));

    // Detached HEAD is the state `git submodule update` leaves; a backend has to
    // move the worktree without a branch to update.
    out.push(Case::new("merge-recursive", &["merge-recursive", "HEAD", "--", "HEAD", "main"], Shape::Detached));
    out.push(Case::new("merge-resolve", &["merge-resolve", "HEAD", "--", "HEAD", "main"], Shape::Detached));
    out.push(Case::new("merge-subtree", &["merge-subtree", "HEAD", "--", "HEAD", "main"], Shape::Detached));

    // Refusals. An unmerged index must stop every backend, and `merge-ours`
    // additionally refuses when the index has staged changes.
    for cmd in ["merge-recursive", "merge-recursive-ours", "merge-recursive-theirs", "merge-subtree"] {
        out.push(Case::new(cmd, &[cmd, "main^", "--", "main", "theirs"], Shape::Conflicted));
    }
    out.push(Case::new("merge-ours", &["merge-ours", "main", "--", "main", "main"], Shape::Dirty));

    // Octopus refuses anything that is not an octopus (fewer than two remotes),
    // so a real run needs three heads. `main` appears twice: the fixtures have
    // only two branches, and repeating a head still drives the second iteration
    // of the merge loop.
    out.push(Case::new("merge-octopus", &["merge-octopus", "main", "--", "main", "feature"], Shape::Branched));
    out.push(Case::new("merge-octopus", &["merge-octopus", "main", "--", "main", "feature", "main"], Shape::Branched));
    out.push(Case::new("merge-octopus", &["merge-octopus", "main^", "--", "main", "side", "main"], Shape::Merged));
    // A head named with a dot: git's octopus is a shell script that expands
    // `${GITHEAD_$name:-$name}`, which the shell rejects for a name that is not
    // an identifier. Kept because whatever stock does with it is the contract.
    out.push(Case::new("merge-octopus", &["merge-octopus", "main", "--", "main", "feature", "v0.1.0"], Shape::Branched));
}

/// `mergetool` is interactive by default; a user-defined tool plus `--no-prompt`
/// is the only way to drive it with no stdin, and it is driven here.
fn mergetool(out: &mut Vec<Case>) {
    out.push(Case::new("mergetool", &["mergetool", "--tool-help"], Shape::Conflicted));
    // Nothing to merge, no tool configured: the advice path.
    out.push(Case::new("mergetool", &["mergetool", "--no-prompt"], Shape::Linear));

    // Resolve for real. The tool picks a side, so the staged blob differs
    // between the two variants and `ls-files --stage` distinguishes them.
    out.push(Case::new(
        "mergetool",
        &["-c", TOOL_TAKE_LOCAL, "-c", TOOL_TRUST, "mergetool", "--no-prompt", "--tool=parity"],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "mergetool",
        &["-c", TOOL_TAKE_REMOTE, "-c", TOOL_TRUST, "mergetool", "--no-prompt", "--tool=parity"],
        Shape::Conflicted,
    ));
    // Pathspec form, and `keepBackup=false` which decides whether `*.orig`
    // survives — visible to the untracked-files probe.
    out.push(Case::new(
        "mergetool",
        &[
            "-c", TOOL_TAKE_REMOTE, "-c", TOOL_TRUST, "-c", "mergetool.keepBackup=false",
            "mergetool", "--no-prompt", "--tool=parity", "--", "conflict.txt",
        ],
        Shape::Conflicted,
    ));
    // A tool name with no `cmd` behind it.
    out.push(Case::new("mergetool", &["mergetool", "--no-prompt", "--tool=no-such-tool"], Shape::Conflicted));
}

/// `rerere`: the read-only reporting verbs, the record path, and forget.
fn rerere(out: &mut Vec<Case>) {
    // No `rr-cache`, rerere disabled: every verb is a silent success.
    for verb in ["status", "diff", "remaining", "gc", "clear"] {
        out.push(Case::new("rerere", &["rerere", verb], Shape::Linear));
    }
    // Mid-merge but still disabled: nothing may be recorded or reported.
    for verb in ["status", "diff", "remaining"] {
        out.push(Case::new("rerere", &["rerere", verb], Shape::Conflicted));
    }
    out.push(Case::new("rerere", &["rerere", "forget", "conflict.txt"], Shape::Conflicted));

    // Enabled, mid-merge: the bare form records a preimage for the conflict.
    out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere"], Shape::Conflicted));
    for verb in ["status", "diff", "remaining", "gc", "clear"] {
        out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere", verb], Shape::Conflicted));
    }
    // Forget with nothing remembered, on a conflicted and on a clean path.
    out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere", "forget", "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere", "forget", "README.md"], Shape::Conflicted));
    // Unknown verb.
    out.push(Case::new("rerere", &["rerere", "bogus"], Shape::Conflicted));
}
