//! Differential corpus cases for the diff_family subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Scope: the *plumbing* diff commands (`diff-files`, `diff-index`,
//! `diff-tree`, `diff-pairs`), the two history-diff porcelains built on them
//! (`whatchanged`, `range-diff`), the pickaxe search (`-S`/`-G`), and the two
//! summarizers that read the same traversal (`shortlog`, `show-branch`).
//!
//! `git diff` itself is covered in `corpus.rs`; the commands here share almost
//! none of that code path in zvcs, which is why they get their own corpus. The
//! blocks below are organised by *output format* rather than by flag category,
//! because that is how the divergences cluster: raw vs patch vs stat vs `-z`
//! are separate emitters, and a fix in one has never implied a fix in another.
//!
//! Two fixture properties are load-bearing and worth stating so a reader does
//! not mistake them for accidents:
//!
//! * Templates are *copied*, so every tracked file is stat-dirty in a fresh
//!   case repo. `diff-files`/`diff-index` (plumbing, which never refreshes the
//!   index) therefore report every path as modified even in the clean shapes.
//!   That is deterministic — the raw format carries only mode and object id —
//!   and it is what gives the clean shapes any `diff-files` output at all.
//! * `-U0` against `Branched` is the only way these fixtures reach the
//!   `@@ … @@ <funcname>` context suffix: `src/lib.rs` gains a second line
//!   below `pub fn one() -> u32 { 1 }`, so a zero-context hunk starts at line 2
//!   and git searches backwards for the function line. Cases that exist to pin
//!   the suffix are marked below.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    diff_files(out);
    diff_index(out);
    diff_tree(out);
    diff_pairs(out);
    range_diff(out);
    whatchanged(out);
    pickaxe(out);
    shortlog(out);
    show_branch(out);
}

/// `diff-files`: worktree against the index, with no index refresh.
fn diff_files(out: &mut Vec<Case>) {
    // The default raw format, on every history layout.
    read_only("diff-files", &["diff-files"], out);
    read_only("diff-files", &["diff-files", "--raw"], out);
    read_only("diff-files", &["diff-files", "-p"], out);

    // Every output format, against the shape that has real content differences
    // rather than only stat differences.
    for args in [
        &["diff-files", "-z"][..],
        &["diff-files", "--name-only"],
        &["diff-files", "--name-status"],
        &["diff-files", "--name-status", "-z"],
        &["diff-files", "--stat"],
        &["diff-files", "--numstat"],
        &["diff-files", "--shortstat"],
        &["diff-files", "--summary"],
        &["diff-files", "--compact-summary"],
        &["diff-files", "--dirstat"],
        &["diff-files", "-p", "--stat"],
        &["diff-files", "--check"],
    ] {
        out.push(Case::new("diff-files", args, Shape::Dirty));
    }

    // Context, whitespace, and hunk-shaping knobs on the patch emitter.
    for args in [
        &["diff-files", "-p", "-U0"][..],
        &["diff-files", "-p", "-U5"],
        &["diff-files", "-p", "-w"],
        &["diff-files", "-p", "-b"],
        &["diff-files", "-p", "--ignore-all-space"],
        &["diff-files", "-p", "--no-prefix"],
        &["diff-files", "-p", "--src-prefix=x/", "--dst-prefix=y/"],
        &["diff-files", "-p", "--binary"],
        &["diff-files", "-p", "--word-diff"],
        &["diff-files", "-p", "--textconv"],
        &["diff-files", "-p", "--ext-diff"],
        &["diff-files", "-p", "--irreversible-delete"],
        // Known dropped: `diff-files` rejects `--inter-hunk-context` outright.
        &["diff-files", "-p", "--inter-hunk-context=3"],
    ] {
        out.push(Case::new("diff-files", args, Shape::Dirty));
    }

    // Object-id abbreviation: config form, flag form, and the patch `index`
    // line, which is a separate abbreviation decision from the raw columns.
    out.push(Case::new("diff-files", &["diff-files", "--abbrev=12"], Shape::Dirty));
    out.push(Case::new("diff-files", &["diff-files", "--no-abbrev"], Shape::Dirty));
    out.push(Case::new("diff-files", &["diff-files", "-p", "--abbrev=12"], Shape::Dirty));
    out.push(Case::new("diff-files", &["diff-files", "-p", "--no-abbrev"], Shape::Dirty));
    out.push(Case::new("diff-files", &["diff-files", "-p", "--full-index"], Shape::Dirty));
    out.push(Case::new("diff-files", &["-c", "core.abbrev=12", "diff-files", "-p"], Shape::Dirty));
    out.push(Case::new("diff-files", &["-c", "core.abbrev=12", "diff-files"], Shape::Dirty));
    out.push(Case::new("diff-files", &["-c", "core.abbrev=40", "diff-files"], Shape::Dirty));

    // Rename/copy detection and filtering.
    for args in [
        &["diff-files", "-M"][..],
        &["diff-files", "--find-renames"],
        &["diff-files", "-B"],
        &["diff-files", "-C"],
        &["diff-files", "--no-renames"],
        &["diff-files", "-l1"],
        &["diff-files", "--diff-filter=M"],
        &["diff-files", "--diff-filter=D"],
        &["diff-files", "-R"],
        &["diff-files", "--exit-code"],
        &["diff-files", "--quiet"],
        &["diff-files", "-s"],
        &["diff-files", "-q"],
        &["diff-files", "--line-prefix=X"],
        &["diff-files", "--relative"],
        &["diff-files", "--rotate-to=src/lib.rs"],
        &["diff-files", "--skip-to=src/lib.rs"],
        &["diff-files", "--ignore-submodules=all"],
        &["diff-files", "--", "README.md"],
        // `-I` with a *detached* argument, which git's parser accepts.
        &["diff-files", "-I", "fixture"],
    ] {
        out.push(Case::new("diff-files", args, Shape::Dirty));
    }

    // Pickaxe through the plumbing entry point rather than through `log`.
    out.push(Case::new("diff-files", &["diff-files", "-Sstaged"], Shape::Dirty));
    out.push(Case::new("diff-files", &["diff-files", "-Gfixture"], Shape::Dirty));

    // Unmerged index: the stage-selecting flags and the combined forms.
    for args in [
        &["diff-files"][..],
        &["diff-files", "-0"],
        &["diff-files", "-1"],
        &["diff-files", "-2"],
        &["diff-files", "-3"],
        &["diff-files", "-c"],
        &["diff-files", "--cc"],
    ] {
        out.push(Case::new("diff-files", args, Shape::Conflicted));
    }

    // Path quoting: the raw format quotes by default; `core.quotePath=false`
    // turns off *only* the high-byte escaping, never the `"` escaping.
    out.push(Case::new("diff-files", &["diff-files"], Shape::AwkwardPaths));
    out.push(Case::new("diff-files", &["diff-files", "-z"], Shape::AwkwardPaths));
    out.push(Case::new("diff-files", &["diff-files", "--name-only"], Shape::AwkwardPaths));
    out.push(Case::new(
        "diff-files",
        &["-c", "core.quotePath=false", "diff-files"],
        Shape::AwkwardPaths,
    ));

    out.push(Case::new("diff-files", &["diff-files"], Shape::Submodule));
    out.push(Case::new("diff-files", &["diff-files", "--no-such-flag"], Shape::Dirty));
}

/// `diff-index`: a tree against the index, or against the worktree with the
/// index as the stat cache.
fn diff_index(out: &mut Vec<Case>) {
    read_only("diff-index", &["diff-index", "HEAD"], out);
    read_only("diff-index", &["diff-index", "--cached", "HEAD"], out);
    read_only("diff-index", &["diff-index", "-p", "HEAD"], out);

    for args in [
        &["diff-index", "-p", "--cached", "HEAD"][..],
        &["diff-index", "--raw", "HEAD"],
        &["diff-index", "-z", "HEAD"],
        &["diff-index", "--cached", "-z", "HEAD"],
        &["diff-index", "--name-only", "HEAD"],
        &["diff-index", "--name-status", "HEAD"],
        &["diff-index", "--stat", "HEAD"],
        &["diff-index", "--numstat", "HEAD"],
        &["diff-index", "--shortstat", "HEAD"],
        &["diff-index", "--summary", "HEAD"],
        &["diff-index", "--summary", "--cached", "HEAD"],
        &["diff-index", "--compact-summary", "HEAD"],
        &["diff-index", "--dirstat", "HEAD"],
        // Composite formats: raw block followed by the patch. The raw block
        // must use the same worktree convention as `--raw` alone.
        &["diff-index", "--patch-with-raw", "HEAD"],
        &["diff-index", "--patch-with-stat", "HEAD"],
        &["diff-index", "--check", "HEAD"],
    ] {
        out.push(Case::new("diff-index", args, Shape::Dirty));
    }

    for args in [
        &["diff-index", "-p", "-U0", "HEAD"][..],
        &["diff-index", "-p", "-U7", "HEAD"],
        &["diff-index", "-p", "-w", "HEAD"],
        &["diff-index", "-p", "-b", "HEAD"],
        &["diff-index", "-p", "--no-prefix", "HEAD"],
        &["diff-index", "-p", "--binary", "HEAD"],
        &["diff-index", "-p", "--word-diff", "HEAD"],
        // Known dropped: `--inter-hunk-context` is rejected here too.
        &["diff-index", "-p", "--inter-hunk-context=2", "HEAD"],
    ] {
        out.push(Case::new("diff-index", args, Shape::Dirty));
    }

    out.push(Case::new("diff-index", &["diff-index", "--abbrev=12", "HEAD"], Shape::Dirty));
    out.push(Case::new("diff-index", &["diff-index", "-p", "--abbrev=12", "HEAD"], Shape::Dirty));
    out.push(Case::new("diff-index", &["diff-index", "-p", "--no-abbrev", "HEAD"], Shape::Dirty));
    out.push(Case::new("diff-index", &["diff-index", "-p", "--full-index", "HEAD"], Shape::Dirty));
    out.push(Case::new("diff-index", &["diff-index", "--full-index", "HEAD"], Shape::Dirty));
    out.push(Case::new(
        "diff-index",
        &["-c", "core.abbrev=12", "diff-index", "HEAD"],
        Shape::Dirty,
    ));
    out.push(Case::new(
        "diff-index",
        &["-c", "core.abbrev=12", "diff-index", "-p", "HEAD"],
        Shape::Dirty,
    ));

    // Diff config that must reach the plumbing, not just `git diff`.
    for args in [
        &["-c", "diff.noprefix=true", "diff-index", "-p", "HEAD"][..],
        &["-c", "diff.mnemonicPrefix=true", "diff-index", "-p", "HEAD"],
        &["-c", "diff.context=5", "diff-index", "-p", "HEAD"],
        &["-c", "diff.interHunkContext=2", "diff-index", "-p", "HEAD"],
        &["-c", "diff.renames=false", "diff-index", "HEAD"],
        &["-c", "diff.renameLimit=1", "diff-index", "-M", "HEAD"],
    ] {
        out.push(Case::new("diff-index", args, Shape::Dirty));
    }

    for args in [
        &["diff-index", "-M", "HEAD"][..],
        &["diff-index", "-B", "HEAD"],
        &["diff-index", "-C", "HEAD"],
        &["diff-index", "--no-renames", "HEAD"],
        &["diff-index", "--find-renames=50%", "HEAD"],
        &["diff-index", "--diff-filter=D", "HEAD"],
        &["diff-index", "-R", "HEAD"],
        &["diff-index", "-s", "HEAD"],
        &["diff-index", "--exit-code", "HEAD"],
        &["diff-index", "--quiet", "HEAD"],
        &["diff-index", "--cached", "--exit-code", "HEAD"],
        &["diff-index", "--ita-invisible-in-index", "HEAD"],
        &["diff-index", "-Sstaged", "--cached", "HEAD"],
        &["diff-index", "HEAD", "--", "README.md"],
    ] {
        out.push(Case::new("diff-index", args, Shape::Dirty));
    }

    // Zero-context against the previous commit: the only fixture path that
    // produces a `@@ … @@ <funcname>` suffix. Both the `--cached` (index) and
    // worktree sides are pinned because they are separate code paths.
    out.push(Case::new("diff-index", &["diff-index", "-p", "-U0", "--cached", "HEAD~1"], Shape::Branched));
    out.push(Case::new("diff-index", &["diff-index", "-p", "-U0", "HEAD~1"], Shape::Branched));
    out.push(Case::new("diff-index", &["diff-index", "-p", "-U1", "--cached", "HEAD~1"], Shape::Branched));
    out.push(Case::new(
        "diff-index",
        &["diff-index", "-p", "-U0", "-w", "--cached", "HEAD~1"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "diff-index",
        &["diff-index", "-p", "-U0", "-b", "--cached", "HEAD~1"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "diff-index",
        &["-c", "diff.context=0", "diff-index", "-p", "--cached", "HEAD~1"],
        Shape::Branched,
    ));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "HEAD~1"], Shape::Branched));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "-p", "--stat", "HEAD~1"], Shape::Branched));
    out.push(Case::new("diff-index", &["diff-index", "--merge-base", "main"], Shape::Branched));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "--merge-base", "feature"], Shape::Branched));

    out.push(Case::new("diff-index", &["diff-index", "-m", "HEAD"], Shape::Merged));
    out.push(Case::new("diff-index", &["diff-index", "-m", "HEAD"], Shape::Conflicted));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "HEAD"], Shape::Conflicted));

    out.push(Case::new("diff-index", &["diff-index", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("diff-index", &["diff-index", "-z", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("diff-index", &["diff-index", "--name-status", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new(
        "diff-index",
        &["-c", "core.quotePath=false", "diff-index", "HEAD"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "HEAD"], Shape::Submodule));

    // Error paths: missing tree-ish, unresolvable tree-ish, absent object.
    out.push(Case::new("diff-index", &["diff-index"], Shape::Linear));
    out.push(Case::new("diff-index", &["diff-index", "--cached"], Shape::Linear));
    out.push(Case::new("diff-index", &["diff-index", "does-not-exist"], Shape::Linear));
    out.push(Case::new(
        "diff-index",
        &["diff-index", "--cached", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"],
        Shape::Linear,
    ));
}

/// `diff-tree`: tree against tree, including the multi-parent forms.
fn diff_tree(out: &mut Vec<Case>) {
    read_only("diff-tree", &["diff-tree", "-r", "HEAD"], out);
    read_only("diff-tree", &["diff-tree", "HEAD"], out);
    read_only("diff-tree", &["diff-tree", "--root", "-r", "HEAD"], out);

    for args in [
        &["diff-tree", "-r", "HEAD~1", "HEAD"][..],
        &["diff-tree", "-r", "--raw", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-z", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--name-only", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--name-status", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--numstat", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--shortstat", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--summary", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-t", "HEAD~1", "HEAD"],
        &["diff-tree", "-s", "HEAD"],
        &["diff-tree", "--no-commit-id", "-r", "HEAD"],
        // Formats and detectors `diff-files`/`diff-index` already emit.
        &["diff-tree", "-r", "--stat", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "--compact-summary", "HEAD"],
        &["diff-tree", "-r", "--dirstat", "HEAD"],
        &["diff-tree", "-p", "HEAD~1", "HEAD"],
        &["diff-tree", "-p", "-U0", "HEAD~1", "HEAD"],
        &["diff-tree", "-p", "-w", "HEAD~1", "HEAD"],
        &["diff-tree", "-p", "--inter-hunk-context=1", "HEAD~1", "HEAD"],
        &["diff-tree", "--pretty", "-r", "HEAD"],
        &["diff-tree", "--pretty=oneline", "-r", "HEAD"],
        &["diff-tree", "-v", "-r", "HEAD"],
        &["diff-tree", "-r", "-M", "HEAD"],
        &["diff-tree", "-r", "--find-renames", "HEAD"],
        &["diff-tree", "-r", "-B", "HEAD"],
        &["diff-tree", "-r", "--no-renames", "HEAD"],
        &["diff-tree", "-r", "--line-prefix=X", "HEAD"],
        &["diff-tree", "-r", "--diff-filter=A", "HEAD"],
        &["diff-tree", "-r", "--abbrev=12", "HEAD"],
        &["diff-tree", "-r", "--no-abbrev", "HEAD"],
        &["diff-tree", "-r", "--full-index", "HEAD"],
        &["diff-tree", "-r", "-R", "HEAD"],
        &["diff-tree", "-r", "--exit-code", "HEAD"],
        &["diff-tree", "-r", "main", "feature"],
        &["diff-tree", "-r", "--merge-base", "main", "feature"],
        &["diff-tree", "-r", "HEAD", "--", "src"],
        &["-c", "core.abbrev=12", "diff-tree", "-r", "HEAD"],
    ] {
        out.push(Case::new("diff-tree", args, Shape::Branched));
    }

    // `--stdin` with no input: git reads EOF and exits cleanly. The case exists
    // because the *absence* of input is the cheapest way to reach the flag.
    out.push(Case::new("diff-tree", &["diff-tree", "--stdin"], Shape::Branched));
    // No revision at all.
    out.push(Case::new("diff-tree", &["diff-tree"], Shape::Branched));

    // Multi-parent: a merge shows nothing by default, everything under `-m`,
    // and the condensed forms under `-c`/`--cc`. The commit-id line is part of
    // the output in every one of these.
    for args in [
        &["diff-tree", "-r", "-m", "HEAD"][..],
        &["diff-tree", "-r", "-m", "--name-status", "HEAD"],
        &["diff-tree", "-m", "-r", "--raw", "HEAD"],
        &["diff-tree", "-m", "--numstat", "HEAD"],
        &["diff-tree", "-m", "--summary", "HEAD"],
        &["diff-tree", "-r", "--stat", "-m", "HEAD"],
        &["diff-tree", "--root", "-m", "-r", "HEAD"],
        &["diff-tree", "-r", "-t", "-m", "HEAD"],
        &["diff-tree", "-c", "HEAD"],
        &["diff-tree", "--cc", "HEAD"],
        &["diff-tree", "-c", "-r", "HEAD"],
        &["diff-tree", "-r", "-c", "HEAD"],
        &["diff-tree", "--cc", "-r", "HEAD"],
        &["diff-tree", "--combined-all-paths", "-c", "-r", "HEAD"],
        &["diff-tree", "-r", "-p", "HEAD"],
    ] {
        out.push(Case::new("diff-tree", args, Shape::Merged));
    }

    // Path quoting in every emitter the raw formats share.
    for args in [
        &["diff-tree", "--root", "-r", "HEAD"][..],
        &["diff-tree", "--root", "-r", "-z", "HEAD"],
        &["diff-tree", "--root", "-r", "--name-only", "HEAD"],
        &["diff-tree", "--root", "-r", "--name-status", "HEAD"],
        &["diff-tree", "--root", "-r", "--numstat", "HEAD"],
        &["diff-tree", "--root", "-r", "--summary", "HEAD"],
        &["diff-tree", "--root", "-r", "-z", "--name-only", "HEAD"],
        &["-c", "core.quotePath=false", "diff-tree", "--root", "-r", "HEAD"],
        &["-c", "core.quotePath=false", "diff-tree", "--root", "-r", "--name-only", "HEAD"],
    ] {
        out.push(Case::new("diff-tree", args, Shape::AwkwardPaths));
    }

    out.push(Case::new("diff-tree", &["diff-tree", "-r", "HEAD"], Shape::Conflicted));
    out.push(Case::new("diff-tree", &["diff-tree", "-r", "does-not-exist"], Shape::Linear));
}

/// `diff-pairs`: reads `<oid> <path>` pairs on stdin. The harness gives every
/// command an empty stdin, so these pin the argument contract and the
/// no-input path rather than the pair-reading itself.
fn diff_pairs(out: &mut Vec<Case>) {
    out.push(Case::new("diff-pairs", &["diff-pairs"], Shape::Linear));
    out.push(Case::new("diff-pairs", &["diff-pairs", "-z"], Shape::Linear));
    out.push(Case::new("diff-pairs", &["diff-pairs", "-z"], Shape::Dirty));
    for args in [
        &["diff-pairs", "-z", "--raw"][..],
        &["diff-pairs", "-z", "-p"],
        &["diff-pairs", "-z", "--name-only"],
        &["diff-pairs", "-z", "--stat"],
        &["diff-pairs", "--no-such-flag"],
    ] {
        out.push(Case::new("diff-pairs", args, Shape::Branched));
    }
}

/// `range-diff`: a diff of two patch series.
fn range_diff(out: &mut Vec<Case>) {
    for args in [
        &["range-diff", "main...feature"][..],
        &["range-diff", "main..feature", "main..feature"],
        &["range-diff", "HEAD~1..HEAD", "HEAD~1..HEAD"],
        &["range-diff", "main", "feature", "feature"],
        &["range-diff", "--no-patch", "main...feature"],
        &["range-diff", "-s", "main...feature"],
        &["range-diff", "--stat", "main...feature"],
        &["range-diff", "--creation-factor=90", "main...feature"],
        &["range-diff", "--creation-factor=1", "main...feature"],
        &["range-diff", "--left-only", "main...feature"],
        &["range-diff", "--right-only", "main...feature"],
        &["range-diff", "--no-color", "main...feature"],
        &["range-diff", "--abbrev=12", "main...feature"],
        &["range-diff", "-U1", "main...feature"],
        &["range-diff", "--notes", "main...feature"],
        &["range-diff", "--diff-merges=on", "main...feature"],
        // `--dual-color` forces color on even when the environment says no.
        &["range-diff", "--dual-color", "main...feature"],
        // Error paths.
        &["range-diff", "main"],
    ] {
        out.push(Case::new("range-diff", args, Shape::Branched));
    }
    out.push(Case::new("range-diff", &["range-diff", "main~1...side"], Shape::Merged));
    out.push(Case::new("range-diff", &["range-diff", "HEAD...HEAD"], Shape::Linear));
    out.push(Case::new("range-diff", &["range-diff", "no-such...HEAD"], Shape::Linear));
    out.push(Case::new("range-diff", &["range-diff", "--bogus-flag", "HEAD...HEAD"], Shape::Linear));
    out.push(Case::new("range-diff", &["range-diff"], Shape::Linear));
}

/// `whatchanged`: `log --raw --no-merges`, still shipped behind a guard flag.
fn whatchanged(out: &mut Vec<Case>) {
    // Refusing without `--i-still-use-this` is itself part of the contract.
    read_only("whatchanged", &["whatchanged"], out);
    read_only("whatchanged", &["whatchanged", "--i-still-use-this"], out);

    for args in [
        &["whatchanged", "--i-still-use-this", "--raw"][..],
        &["whatchanged", "--i-still-use-this", "--oneline"],
        &["whatchanged", "--i-still-use-this", "-p"],
        &["whatchanged", "--i-still-use-this", "--name-status"],
        &["whatchanged", "--i-still-use-this", "--stat"],
        &["whatchanged", "--i-still-use-this", "-z"],
        &["whatchanged", "--i-still-use-this", "--format=%H"],
        &["whatchanged", "--i-still-use-this", "-1"],
        &["whatchanged", "--i-still-use-this", "-n", "1"],
        &["whatchanged", "--i-still-use-this", "--no-merges"],
        &["whatchanged", "--i-still-use-this", "--no-renames"],
        &["whatchanged", "--i-still-use-this", "--all"],
        &["whatchanged", "--i-still-use-this", "--root"],
        &["whatchanged", "--i-still-use-this", "--abbrev-commit"],
        &["whatchanged", "--i-still-use-this", "--no-abbrev-commit"],
        &["whatchanged", "--i-still-use-this", "--grep=two"],
    ] {
        out.push(Case::new("whatchanged", args, Shape::Branched));
    }

    out.push(Case::new("whatchanged", &["whatchanged", "--i-still-use-this", "-m"], Shape::Merged));
    out.push(Case::new(
        "whatchanged",
        &["whatchanged", "--i-still-use-this", "--no-merges", "--raw"],
        Shape::Merged,
    ));
    out.push(Case::new("whatchanged", &["whatchanged", "--i-still-use-this"], Shape::AwkwardPaths));
    out.push(Case::new(
        "whatchanged",
        &["whatchanged", "--i-still-use-this", "--raw"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "whatchanged",
        &["-c", "core.quotePath=false", "whatchanged", "--i-still-use-this", "--raw"],
        Shape::AwkwardPaths,
    ));
}

/// Pickaxe search (`-S`, `-G`) across the commands that expose it. Labelled as
/// its own command so a run can score the search independently of the
/// traversal it rides on.
fn pickaxe(out: &mut Vec<Case>) {
    for args in [
        &["log", "-Sone", "--oneline"][..],
        &["log", "-Stwo", "--oneline"],
        &["log", "-Gfn", "--oneline"],
        &["log", "-Gtwo", "--oneline"],
        &["log", "-Sfeature", "--all", "--oneline"],
        &["log", "-Snonexistentneedle", "--oneline"],
        // Without `--pickaxe-all`, the diff shown is narrowed to the paths that
        // actually changed occurrence count.
        &["log", "-Sone", "--name-status"],
        &["log", "-Sone", "--raw"],
        &["log", "-Sone", "--pickaxe-all", "--raw"],
        &["log", "--pickaxe-regex", "-Sfn.*two", "--oneline"],
        &["log", "--find-object=74b744054bc0580719c0765bd5efdf0ba1638668", "--oneline"],
        &["diff-tree", "-r", "-Stwo", "HEAD"],
    ] {
        out.push(Case::new("pickaxe", args, Shape::Branched));
    }
    out.push(Case::new("pickaxe", &["log", "-Sside", "--oneline"], Shape::Merged));
    out.push(Case::new("pickaxe", &["diff-index", "-Sstaged", "--cached", "HEAD"], Shape::Dirty));
    out.push(Case::new("pickaxe", &["diff-files", "-Gfixture"], Shape::Dirty));
}

/// `shortlog`: authorship summary over a traversal.
fn shortlog(out: &mut Vec<Case>) {
    // Bare `shortlog` reads stdin, which the harness leaves empty; the revision
    // form is what actually traverses.
    read_only("shortlog", &["shortlog", "HEAD"], out);
    read_only("shortlog", &["shortlog", "-s", "HEAD"], out);

    for args in [
        &["shortlog", "-n", "HEAD"][..],
        &["shortlog", "-sn", "HEAD"],
        &["shortlog", "-e", "HEAD"],
        &["shortlog", "-se", "HEAD"],
        &["shortlog", "-nse", "HEAD"],
        &["shortlog", "--summary", "--numbered", "HEAD"],
        &["shortlog", "--email", "--summary", "HEAD"],
        &["shortlog", "--no-merges", "HEAD"],
        &["shortlog", "-w60,2,4", "HEAD"],
        &["shortlog", "--format=%s", "HEAD"],
        &["shortlog", "--group=author", "HEAD"],
        &["shortlog", "--group=committer", "HEAD"],
        &["shortlog", "--group=trailer:Signed-off-by", "HEAD"],
        &["shortlog", "-c", "HEAD"],
        &["shortlog", "--committer", "HEAD"],
        &["shortlog", "--all"],
        &["shortlog", "-s", "--all"],
        &["shortlog", "HEAD", "--"],
        // Pathspec-limited traversal.
        &["shortlog", "-s", "HEAD", "--", "src"],
        &["shortlog", "HEAD", "--", "README.md"],
        &["shortlog", "-s", "HEAD", "--", "no-such-path"],
    ] {
        out.push(Case::new("shortlog", args, Shape::Branched));
    }

    out.push(Case::new("shortlog", &["shortlog", "--no-merges", "-s", "HEAD"], Shape::Merged));
    out.push(Case::new("shortlog", &["shortlog", "-s", "HEAD", "--", "side.txt"], Shape::Merged));
    out.push(Case::new(
        "shortlog",
        &["shortlog", "--group=author", "--group=committer", "HEAD"],
        Shape::Merged,
    ));
    out.push(Case::new("shortlog", &["shortlog", "-s", "HEAD"], Shape::Conflicted));
    out.push(Case::new("shortlog", &["shortlog", "-s", "HEAD"], Shape::Submodule));
    out.push(Case::new("shortlog", &["shortlog", "-s", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("shortlog", &["shortlog", "-s", "does-not-exist"], Shape::Linear));
    out.push(Case::new("shortlog", &["shortlog", "--bogus-flag", "HEAD"], Shape::Linear));
}

/// `show-branch`: the branch/commit matrix.
fn show_branch(out: &mut Vec<Case>) {
    read_only("show-branch", &["show-branch"], out);

    for args in [
        &["show-branch", "-a"][..],
        &["show-branch", "--list"],
        &["show-branch", "--current"],
        &["show-branch", "--sha1-name"],
        &["show-branch", "--sha1-name", "--all"],
        &["show-branch", "--no-name"],
        &["show-branch", "--topics", "main", "feature"],
        &["show-branch", "--topics", "--all"],
        &["show-branch", "--more=2"],
        &["show-branch", "--all", "--more=5"],
        &["show-branch", "--independent", "main", "feature"],
        &["show-branch", "--independent", "--all"],
        &["show-branch", "--merge-base", "main", "feature"],
        &["show-branch", "--merge-base", "--all"],
        &["show-branch", "--topo-order", "--all"],
        &["show-branch", "--date-order", "--all"],
        &["show-branch", "--sparse", "--all"],
        &["show-branch", "--reflog"],
        &["show-branch", "-g"],
        &["show-branch", "main", "feature"],
    ] {
        out.push(Case::new("show-branch", args, Shape::Branched));
    }

    out.push(Case::new("show-branch", &["show-branch", "--merge-base", "main", "side"], Shape::Merged));
    out.push(Case::new("show-branch", &["show-branch", "--independent", "main", "side"], Shape::Merged));
    out.push(Case::new("show-branch", &["show-branch", "--all"], Shape::Conflicted));
    out.push(Case::new("show-branch", &["show-branch", "--all"], Shape::Submodule));
    out.push(Case::new("show-branch", &["show-branch"], Shape::AwkwardPaths));
    out.push(Case::new("show-branch", &["show-branch", "no-such-branch"], Shape::Linear));
    out.push(Case::new("show-branch", &["show-branch", "--bogus-flag"], Shape::Linear));
}
