//! Cases that only exist because of the repository shapes added alongside them:
//! `Attributes`, `Renamed`, `Whitespace`, `Packed`, `Patches` and `Sparse`
//! first, `NoIndexTrees` and `DecomposedPaths` since.
//!
//! Grouped by shape rather than by subsystem, which is the exception to how the
//! rest of the corpus is arranged. The reason is that the shape *is* the thing
//! under test here: each block below exercises a behaviour that no case could
//! reach before, because a case is one argv against a pristine copy and could
//! not create a `.gitattributes`, a rename, a pack or a patch file first. Split
//! across the eleven subsystem modules, that premise would be invisible.
//!
//! Properties of the new shapes that the cases below depend on, recorded so a
//! reader does not have to rebuild them from the fixture source:
//!
//! * `Attributes` — root and nested `.gitattributes`, root and nested
//!   `.gitignore`, `.git/info/attributes`, `.git/info/exclude`, a `.mailmap`,
//!   three commits authored by the identities it rewrites, one tracked file
//!   that its own ignore rule matches, and untracked files for every rule.
//! * `Renamed` — one commit per detection class: a 100% rename, a rename with
//!   an edit that stock scores `R072`, a copy whose source is modified in the
//!   same commit, and an in-place rewrite.
//! * `Whitespace` — tabs→spaces, trailing blanks, CRLF→LF, one real edit amid
//!   whitespace churn, and an unstaged whitespace-only worktree edit.
//! * `Packed` — two packs with delta chains, loose duplicates of packed
//!   objects, a dangling commit, and pack files tracked at `packs/sample.idx`,
//!   `packs/sample.pack` and `packs/unindexed.pack`.
//! * `Patches` — `patches/{valid,corrupt,context-only,whitespace,offset,
//!   binary}.patch`, `mail/series.mbox`, `mail/one.eml`, and a quilt series
//!   under `quilt/`, all applying to `main`'s tree.
//! * `Sparse` — cone mode with `inside/` in and `outside/` out, plus an
//!   untracked file inside the excluded cone.
//! * `NoIndexTrees` — under `ni/`: `da`/`db` (a modification, a left-only file
//!   and a right-only file), the add-only pair `addonly_a`/`addonly_b`, the
//!   delete-only pair `delonly_a`/`delonly_b`, two plain files `a.txt`/`b.txt`,
//!   and `core.abbrev = 10` in the repository config.
//! * `DecomposedPaths` — `e`+U+0301`.txt` tracked and edited in the worktree,
//!   and `e`+U+0301`-new.txt` untracked.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    attributes(out);
    ignores(out);
    mailmap(out);
    renames(out);
    whitespace(out);
    packs(out);
    patches(out);
    sparse(out);
    no_index_trees(out);
    decomposed_paths(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

/// `check-attr` against rules that match. Every answer below is a lookup
/// result, not the "nothing configured" default the shape-less corpus was
/// limited to: `text=auto` from the root file, `eol=crlf` from `sub/`, the
/// `binary` macro expanding to three unset attributes, and `!text` for a path
/// no rule sets.
fn attributes(out: &mut Vec<Case>) {
    each(
        Shape::Attributes,
        "check-attr",
        &[
            &["check-attr", "text", "src/tabs.rs"],
            &["check-attr", "eol", "src/tabs.rs"],
            &["check-attr", "whitespace", "src/tabs.rs"],
            &["check-attr", "-a", "src/tabs.rs"],
            &["check-attr", "--all", "docs/manual.md"],
            &["check-attr", "diff", "docs/manual.md"],
            &["check-attr", "export-ignore", "docs/manual.md"],
            // The `binary` macro expands to `-diff -merge -text`.
            &["check-attr", "-a", "assets/logo.bin"],
            &["check-attr", "text", "assets/logo.bin"],
            &["check-attr", "diff", "assets/logo.bin"],
            // `sub/.gitattributes` overrides the root for its own subtree.
            &["check-attr", "-a", "sub/nested.txt"],
            &["check-attr", "eol", "sub/nested.txt"],
            &["check-attr", "merge", "sub/nested.txt"],
            &["check-attr", "diff", "sub/nested.txt"],
            &["check-attr", "-a", "vendor/generated.js"],
            &["check-attr", "linguist-generated", "vendor/generated.js"],
            &["check-attr", "-a", "logs/keep.log"],
            // Explicitly unset, which is a third answer distinct from set/unspecified.
            &["check-attr", "text", "missing-attr.txt"],
            &["check-attr", "-a", "missing-attr.txt"],
            &["check-attr", "text", "eol", "src/tabs.rs"],
            &["check-attr", "text", "diff", "merge", "sub/nested.txt"],
            &["check-attr", "-a", "src/tabs.rs", "docs/manual.md", "assets/logo.bin"],
            &["check-attr", "-z", "-a", "src/tabs.rs"],
            &["check-attr", "-z", "text", "src/tabs.rs", "sub/nested.txt"],
            &["check-attr", "--cached", "-a", "src/tabs.rs"],
            &["check-attr", "--cached", "text", "sub/nested.txt"],
            &["check-attr", "-a", "--", "src/tabs.rs"],
            &["check-attr", "text", "--", "no/such/path.rs"],
            // Directories and paths under an ignored directory still resolve.
            &["check-attr", "-a", "build/output.o"],
            &["check-attr", "-a", "src"],
        ],
        out,
    );

    // Attributes with an effect beyond `check-attr`: `-diff` suppresses a body,
    // `export-ignore` drops a path from an archive, `text` drives eol handling.
    each(
        Shape::Attributes,
        "diff",
        &[
            &["diff", "HEAD~1", "HEAD"],
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--stat", "HEAD~3", "HEAD"],
            &["diff", "--numstat", "HEAD~3", "HEAD"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "archive",
        &[
            &["archive", "--format=tar", "HEAD"],
            &["archive", "--format=tar", "--prefix=root/", "HEAD"],
            &["archive", "--list"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "ls-files",
        &[
            &["ls-files", "--eol"],
            &["ls-files", "--others", "--exclude-standard"],
            &["ls-files", "--others", "--ignored", "--exclude-standard"],
            &["ls-files", "--directory", "--others", "--exclude-standard"],
            &["ls-files", "-o", "-i", "--exclude-standard", "-z"],
        ],
        out,
    );
}

/// `check-ignore` and the porcelain that reads the same rules. Six rule forms
/// are in play — bare glob, negation, directory, root-anchored, `**`, and a
/// nested file — plus `.git/info/exclude`, plus a tracked path whose own rule
/// matches (git reports it as not ignored; the index wins).
fn ignores(out: &mut Vec<Case>) {
    each(
        Shape::Attributes,
        "check-ignore",
        &[
            &["check-ignore", "build/output.o"],
            &["check-ignore", "-v", "build/output.o"],
            &["check-ignore", "-v", "logs/debug.log"],
            &["check-ignore", "-v", "notes.tmp"],
            &["check-ignore", "-v", "sub/deep-ignored/thing.txt"],
            &["check-ignore", "-v", "sub/local-scratch.txt"],
            &["check-ignore", "-v", "excluded-by-info.txt"],
            // Negated by `!important.log`, so not ignored: exit 1, no output.
            &["check-ignore", "important.log"],
            &["check-ignore", "-v", "important.log"],
            &["check-ignore", "-v", "--non-matching", "important.log"],
            &["check-ignore", "-v", "-n", "tracked-looking.txt"],
            &["check-ignore", "tracked-looking.txt"],
            // Tracked, and matched by `*.log`: the index outranks the rule.
            &["check-ignore", "-v", "logs/keep.log"],
            &["check-ignore", "--no-index", "-v", "logs/keep.log"],
            &["check-ignore", "-q", "build/output.o"],
            &["check-ignore", "-q", "important.log"],
            &["check-ignore", "-z", "-v", "build/output.o"],
            &["check-ignore", "-v", "build/output.o", "logs/debug.log", "notes.tmp"],
            &["check-ignore", "-v", "-n", "build/output.o", "important.log"],
            &["check-ignore", "-v", "build"],
            &["check-ignore", "-v", "sub/deep-ignored"],
            // Root-anchored: matches at the top level only.
            &["check-ignore", "-v", "sub/notes.tmp"],
        ],
        out,
    );

    each(
        Shape::Attributes,
        "status",
        &[
            &["status", "--porcelain"],
            &["status", "--porcelain", "--ignored"],
            &["status", "--porcelain", "--ignored=matching"],
            &["status", "--porcelain", "--ignored=traditional"],
            &["status", "--porcelain", "-uall"],
            &["status", "--porcelain", "-uall", "--ignored"],
            &["status", "--porcelain=v2", "--ignored"],
            &["status", "--short", "--ignored"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "clean",
        &[
            &["clean", "-n"],
            &["clean", "-n", "-d"],
            &["clean", "-n", "-d", "-x"],
            &["clean", "-n", "-d", "-X"],
            &["clean", "-n", "-d", "-x", "-e", "*.tmp"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "add",
        &[
            &["add", "-n", "."],
            &["add", "-n", "-A"],
            &["add", "-n", "-f", "logs/debug.log"],
            &["add", "-n", "logs/debug.log"],
            &["add", "-n", "important.log"],
        ],
        out,
    );
}

/// `.mailmap` rewriting, in `check-mailmap` and in every reader that honours it.
/// Three mapping forms are present: email→email, name+email→name+email, and a
/// bare replacement email.
fn mailmap(out: &mut Vec<Case>) {
    each(
        Shape::Attributes,
        "check-mailmap",
        &[
            &["check-mailmap", "Old Name <old@example.invalid>"],
            &["check-mailmap", "<old@example.invalid>"],
            &["check-mailmap", "Alias Name <alias@example.invalid>"],
            &["check-mailmap", "<alias@example.invalid>"],
            &["check-mailmap", "Typo Name <typo@example.invalid>"],
            &["check-mailmap", "Solo Name <solo@example.invalid>"],
            &["check-mailmap", "Unknown Person <nobody@example.invalid>"],
            &["check-mailmap", "zvcs parity <parity@example.invalid>"],
            &[
                "check-mailmap",
                "Old Name <old@example.invalid>",
                "Typo Name <typo@example.invalid>",
            ],
            // Not a valid contact spec: the error path, with rules present.
            &["check-mailmap", "no-angle-brackets"],
        ],
        out,
    );

    each(
        Shape::Attributes,
        "log",
        &[
            &["log", "--format=%aN <%aE>"],
            &["log", "--format=%an <%ae>"],
            &["log", "--format=%cN <%cE>"],
            &["log", "--use-mailmap", "--format=%an <%ae>"],
            &["log", "--no-use-mailmap", "--format=%aN <%aE>"],
            &["log", "--pretty=fuller"],
            &["log", "--author=Proper", "--oneline"],
            &["log", "--author=old@example.invalid", "--oneline"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "shortlog",
        &[
            &["shortlog", "-s", "HEAD"],
            &["shortlog", "-sn", "HEAD"],
            &["shortlog", "-sne", "HEAD"],
            &["shortlog", "-se", "HEAD"],
            &["shortlog", "--group=author", "-s", "HEAD"],
            &["shortlog", "-s", "-e", "--no-use-mailmap", "HEAD"],
        ],
        out,
    );
    each(
        Shape::Attributes,
        "blame",
        &[
            &["blame", "-s", "sub/nested.txt"],
            &["blame", "--porcelain", "sub/nested.txt"],
            &["blame", "--line-porcelain", "docs/manual.md"],
            &["blame", "-s", "--no-use-mailmap", "sub/nested.txt"],
        ],
        out,
    );
}

/// Rename, copy and rewrite detection. The commits are, from oldest: seed,
/// pure rename, rename-with-edit (`R072`), copy-with-modified-source, rewrite.
fn renames(out: &mut Vec<Case>) {
    each(
        Shape::Renamed,
        "diff",
        &[
            // Pure rename: HEAD~4 → HEAD~3.
            &["diff", "--name-status", "HEAD~4", "HEAD~3"],
            &["diff", "-M", "--name-status", "HEAD~4", "HEAD~3"],
            &["diff", "-M", "--summary", "HEAD~4", "HEAD~3"],
            &["diff", "-M", "--stat", "HEAD~4", "HEAD~3"],
            &["diff", "-M", "--raw", "HEAD~4", "HEAD~3"],
            &["diff", "--no-renames", "--name-status", "HEAD~4", "HEAD~3"],
            &["diff", "-M", "HEAD~4", "HEAD~3"],
            // Rename with edit: stock scores 72%, so the threshold decides.
            &["diff", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "-M", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "-M50%", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "-M72%", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "-M73%", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "-M90%", "--name-status", "HEAD~3", "HEAD~2"],
            &["diff", "--find-renames=50%", "--summary", "HEAD~3", "HEAD~2"],
            &["diff", "--find-renames=90%", "--summary", "HEAD~3", "HEAD~2"],
            &["diff", "-M", "--stat", "HEAD~3", "HEAD~2"],
            &["diff", "-M", "--numstat", "HEAD~3", "HEAD~2"],
            &["diff", "-M", "--raw", "HEAD~3", "HEAD~2"],
            &["diff", "-M", "HEAD~3", "HEAD~2"],
            // Copy, with the source modified in the same commit.
            &["diff", "--name-status", "HEAD~2", "HEAD~1"],
            &["diff", "-C", "--name-status", "HEAD~2", "HEAD~1"],
            &["diff", "-C", "--summary", "HEAD~2", "HEAD~1"],
            &["diff", "-C", "--find-copies-harder", "--name-status", "HEAD~2", "HEAD~1"],
            &["diff", "-C50%", "--name-status", "HEAD~2", "HEAD~1"],
            &["diff", "--find-copies=90%", "--name-status", "HEAD~2", "HEAD~1"],
            &["diff", "-C", "--raw", "HEAD~2", "HEAD~1"],
            // Rewrite in place.
            &["diff", "--name-status", "HEAD~1", "HEAD"],
            &["diff", "-B", "--name-status", "HEAD~1", "HEAD"],
            &["diff", "-B", "--summary", "HEAD~1", "HEAD"],
            &["diff", "-B50%", "--name-status", "HEAD~1", "HEAD"],
            &["diff", "-B", "--stat", "HEAD~1", "HEAD"],
            &["diff", "-B", "-M", "--name-status", "HEAD~1", "HEAD"],
            // Across the whole history, where every class is present at once.
            &["diff", "-M", "-C", "-B", "--name-status", "HEAD~5", "HEAD"],
            &["diff", "-M", "--summary", "HEAD~5", "HEAD"],
            &["diff", "--stat", "HEAD~5", "HEAD"],
            &["diff", "-M", "--name-only", "HEAD~5", "HEAD"],
            &["diff", "-M", "--diff-filter=R", "--name-status", "HEAD~5", "HEAD"],
            &["diff", "-C", "--diff-filter=C", "--name-status", "HEAD~2", "HEAD~1"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "show",
        &[
            &["show", "-M", "--stat", "HEAD~3"],
            &["show", "-M", "--name-status", "HEAD~3"],
            &["show", "-C", "--name-status", "HEAD~1"],
            &["show", "-B", "--summary", "HEAD"],
            &["show", "--stat", "HEAD~2"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "log",
        &[
            &["log", "--oneline", "--name-status"],
            &["log", "-M", "--oneline", "--name-status"],
            &["log", "--follow", "--oneline", "moved/alpha.txt"],
            &["log", "--follow", "--oneline", "moved/beta.txt"],
            &["log", "--oneline", "--", "moved/alpha.txt"],
            &["log", "-C", "--oneline", "--name-status"],
            &["log", "--summary", "--oneline"],
            &["log", "--stat", "--oneline"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "diff-tree",
        &[
            &["diff-tree", "-M", "-r", "--name-status", "HEAD~4", "HEAD~3"],
            &["diff-tree", "-M", "-r", "HEAD~3", "HEAD~2"],
            &["diff-tree", "-C", "-r", "--raw", "HEAD~2", "HEAD~1"],
            &["diff-tree", "-B", "-r", "--name-status", "HEAD~1", "HEAD"],
            &["diff-tree", "-r", "--name-status", "HEAD~4", "HEAD~3"],
        ],
        out,
    );
    each(
        Shape::Renamed,
        "blame",
        &[
            &["blame", "-s", "moved/alpha.txt"],
            &["blame", "-s", "-C", "copies/gamma.txt"],
            &["blame", "-s", "-C", "-C", "copies/gamma.txt"],
            &["blame", "-s", "-M", "moved/beta.txt"],
        ],
        out,
    );
    // `mv` on a shape that already contains renames, so the resulting index has
    // both a recorded rename and a pending one.
    each(
        Shape::Renamed,
        "mv",
        &[
            &["mv", "moved/alpha.txt", "orig/alpha.txt"],
            &["mv", "-n", "moved/alpha.txt", "copies/alpha.txt"],
            &["mv", "orig", "renamed-dir"],
        ],
        out,
    );
}

/// Whitespace-only differences. The commit order is: seed (tabs), tabs→spaces,
/// trailing blanks, CRLF→LF, one real edit amid churn. The worktree carries an
/// unstaged whitespace-only edit on top.
fn whitespace(out: &mut Vec<Case>) {
    each(
        Shape::Whitespace,
        "diff",
        &[
            // Unstaged, whitespace-only: `-w` must produce nothing at all.
            &["diff"],
            &["diff", "-w"],
            &["diff", "-b"],
            &["diff", "--ignore-all-space"],
            &["diff", "--ignore-space-change"],
            &["diff", "--ignore-space-at-eol"],
            &["diff", "--ignore-blank-lines"],
            &["diff", "--stat"],
            &["diff", "--stat", "-w"],
            &["diff", "--numstat", "-w"],
            &["diff", "--shortstat", "-w"],
            &["diff", "--name-only", "-w"],
            &["diff", "--exit-code", "-w"],
            &["diff", "--quiet", "-w"],
            &["diff", "--check"],
            // tabs → spaces.
            &["diff", "HEAD~4", "HEAD~3"],
            &["diff", "-w", "HEAD~4", "HEAD~3"],
            &["diff", "-b", "HEAD~4", "HEAD~3"],
            &["diff", "--stat", "-w", "HEAD~4", "HEAD~3"],
            &["diff", "--ignore-all-space", "--exit-code", "HEAD~4", "HEAD~3"],
            // trailing blanks.
            &["diff", "HEAD~3", "HEAD~2"],
            &["diff", "-b", "HEAD~3", "HEAD~2"],
            &["diff", "--ignore-space-at-eol", "HEAD~3", "HEAD~2"],
            &["diff", "--check", "HEAD~3", "HEAD~2"],
            &["diff", "-w", "HEAD~3", "HEAD~2"],
            // CRLF → LF.
            &["diff", "HEAD~2", "HEAD~1"],
            &["diff", "--ignore-cr-at-eol", "HEAD~2", "HEAD~1"],
            &["diff", "-w", "HEAD~2", "HEAD~1"],
            &["diff", "--stat", "HEAD~2", "HEAD~1"],
            // One real edit amid churn: `-w` must keep the edit and, critically,
            // take its context lines from the post-image.
            &["diff", "HEAD~1", "HEAD"],
            &["diff", "-w", "HEAD~1", "HEAD"],
            &["diff", "-b", "HEAD~1", "HEAD"],
            &["diff", "-w", "-U1", "HEAD~1", "HEAD"],
            &["diff", "-w", "-U5", "HEAD~1", "HEAD"],
            &["diff", "-w", "--stat", "HEAD~1", "HEAD"],
            &["diff", "-w", "--numstat", "HEAD~1", "HEAD"],
            &["diff", "-w", "--word-diff", "HEAD~1", "HEAD"],
            &["diff", "--ignore-all-space", "HEAD~1", "HEAD"],
            &["diff", "-w", "HEAD~5", "HEAD"],
            &["diff", "-b", "HEAD~5", "HEAD"],
            &["diff", "-w", "--patience", "HEAD~1", "HEAD"],
            &["diff", "-w", "--histogram", "HEAD~1", "HEAD"],
            &["diff", "-w", "--minimal", "HEAD~1", "HEAD"],
        ],
        out,
    );
    each(
        Shape::Whitespace,
        "show",
        &[
            &["show", "-w", "HEAD"],
            &["show", "-w", "HEAD~3"],
            &["show", "--stat", "-w", "HEAD~3"],
            &["show", "-b", "HEAD~2"],
        ],
        out,
    );
    each(
        Shape::Whitespace,
        "log",
        &[
            &["log", "-p", "-w", "-1"],
            &["log", "-p", "-w", "--oneline", "-2"],
            &["log", "--stat", "-w", "--oneline"],
        ],
        out,
    );
    each(
        Shape::Whitespace,
        "diff-files",
        &[&["diff-files"], &["diff-files", "-w"], &["diff-files", "-p", "-w"]],
        out,
    );
    each(
        Shape::Whitespace,
        "diff-index",
        &[
            &["diff-index", "HEAD"],
            &["diff-index", "-p", "-w", "HEAD"],
            &["diff-index", "--cached", "HEAD"],
        ],
        out,
    );
    each(
        Shape::Whitespace,
        "diff-tree",
        &[
            &["diff-tree", "-p", "-w", "-r", "HEAD~1", "HEAD"],
            &["diff-tree", "-p", "-w", "-r", "HEAD~4", "HEAD~3"],
        ],
        out,
    );
    each(
        Shape::Whitespace,
        "blame",
        &[
            &["blame", "-s", "ws/indent.c"],
            &["blame", "-s", "-w", "ws/indent.c"],
            &["blame", "-s", "ws/eol.txt"],
        ],
        out,
    );
}

/// Packs with deltas, loose duplicates, and pack files at stable worktree paths.
fn packs(out: &mut Vec<Case>) {
    each(
        Shape::Packed,
        "verify-pack",
        &[
            &["verify-pack", "packs/sample.idx"],
            &["verify-pack", "-v", "packs/sample.idx"],
            &["verify-pack", "--verbose", "packs/sample.idx"],
            &["verify-pack", "-s", "packs/sample.idx"],
            &["verify-pack", "--stat-only", "packs/sample.idx"],
            &["verify-pack", "--", "packs/sample.idx"],
            &["verify-pack", "packs/sample.pack"],
            &["verify-pack", "packs/missing.idx"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "index-pack",
        &[
            &["index-pack", "packs/unindexed.pack"],
            &["index-pack", "-v", "packs/unindexed.pack"],
            &["index-pack", "--strict", "packs/unindexed.pack"],
            &["index-pack", "-o", "packs/named.idx", "packs/unindexed.pack"],
            &["index-pack", "packs/sample.pack"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "prune-packed",
        &[
            &["prune-packed", "-n"],
            &["prune-packed", "--dry-run"],
            &["prune-packed"],
            &["prune-packed", "-q"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "count-objects",
        &[
            &["count-objects"],
            &["count-objects", "-v"],
            &["count-objects", "-H"],
            &["count-objects", "-v", "-H"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "cat-file",
        &[
            &["cat-file", "--batch-all-objects", "--batch-check"],
            &["cat-file", "--batch-all-objects", "--batch-check=%(objecttype) %(objectsize)"],
            &["cat-file", "--batch-all-objects", "--batch-check=%(objectsize:disk)"],
            &["cat-file", "-s", "HEAD:big.txt"],
            &["cat-file", "-t", "HEAD:big.txt"],
            &["cat-file", "--unordered", "--batch-all-objects", "--batch-check"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "fsck",
        &[
            &["fsck"],
            &["fsck", "--unreachable"],
            &["fsck", "--dangling"],
            &["fsck", "--no-dangling"],
            &["fsck", "--connectivity-only"],
            &["fsck", "--strict"],
            &["fsck", "--no-progress"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "repack",
        &[
            &["repack", "-a", "-d", "-q"],
            &["repack", "-A", "-d", "-q"],
            &["repack", "-d", "-q"],
            &["repack", "-q"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "gc",
        &[&["gc", "-q"], &["gc", "--auto", "-q"], &["gc", "--prune=now", "-q"]],
        out,
    );
    each(Shape::Packed, "prune", &[&["prune", "-n"], &["prune", "--dry-run", "-v"]], out);
    each(
        Shape::Packed,
        "multi-pack-index",
        &[
            &["multi-pack-index", "write"],
            &["multi-pack-index", "verify"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "pack-refs",
        &[&["pack-refs", "--all"], &["pack-refs", "--all", "--prune"]],
        out,
    );
    each(
        Shape::Packed,
        "bundle",
        &[
            &["bundle", "create", "-q", "out.bundle", "HEAD"],
            &["bundle", "create", "-q", "all.bundle", "--all"],
        ],
        out,
    );
    // The self-hosting clone path, over a repository whose objects are deltas.
    each(
        Shape::Packed,
        "clone",
        &[
            &["clone", "--no-local", "-q", ".", "cloned"],
            &["clone", "-q", ".", "cloned-local"],
            &["clone", "--bare", "--no-local", "-q", ".", "cloned.git"],
            &["clone", "--depth=1", "--no-local", "-q", ".", "shallow"],
        ],
        out,
    );
    each(
        Shape::Packed,
        "rev-list",
        &[
            &["rev-list", "--objects", "--all"],
            &["rev-list", "--count", "--all"],
            &["rev-list", "--all", "--objects", "--no-object-names"],
        ],
        out,
    );
}

/// `apply`, `am`, `mailsplit` and `quiltimport` given real input.
fn patches(out: &mut Vec<Case>) {
    each(
        Shape::Patches,
        "apply",
        &[
            // Applies cleanly.
            &["apply", "--check", "patches/valid.patch"],
            &["apply", "--check", "--cached", "patches/valid.patch"],
            &["apply", "--check", "--index", "patches/valid.patch"],
            &["apply", "patches/valid.patch"],
            &["apply", "--index", "patches/valid.patch"],
            &["apply", "--cached", "patches/valid.patch"],
            &["apply", "--stat", "patches/valid.patch"],
            &["apply", "--numstat", "patches/valid.patch"],
            &["apply", "--summary", "patches/valid.patch"],
            &["apply", "--stat", "--summary", "patches/valid.patch"],
            &["apply", "-v", "patches/valid.patch"],
            &["apply", "--3way", "patches/valid.patch"],
            &["apply", "--check", "-p1", "patches/valid.patch"],
            &["apply", "--check", "--unidiff-zero", "patches/valid.patch"],
            &["apply", "--check", "--include=app/main.c", "patches/valid.patch"],
            &["apply", "--check", "--exclude=app/main.c", "patches/valid.patch"],
            // Reversing a not-yet-applied patch must fail.
            &["apply", "--check", "-R", "patches/valid.patch"],
            &["apply", "-R", "patches/valid.patch"],
            &["apply", "--check", "--reverse", "patches/valid.patch"],
            // Corrupt: rejected, and rejected the same way with `--cached`,
            // which is the exact combination that once returned 0.
            &["apply", "--check", "patches/corrupt.patch"],
            &["apply", "--check", "--cached", "patches/corrupt.patch"],
            &["apply", "--check", "--index", "patches/corrupt.patch"],
            &["apply", "patches/corrupt.patch"],
            &["apply", "--cached", "patches/corrupt.patch"],
            &["apply", "--stat", "patches/corrupt.patch"],
            &["apply", "--recount", "--check", "patches/corrupt.patch"],
            &["apply", "--recount", "patches/corrupt.patch"],
            // A hunk that changes nothing.
            &["apply", "--check", "patches/context-only.patch"],
            &["apply", "patches/context-only.patch"],
            &["apply", "--recount", "--check", "patches/context-only.patch"],
            // Whitespace damage, under each policy.
            &["apply", "--check", "patches/whitespace.patch"],
            &["apply", "--check", "--whitespace=error", "patches/whitespace.patch"],
            &["apply", "--check", "--whitespace=warn", "patches/whitespace.patch"],
            &["apply", "--check", "--whitespace=nowarn", "patches/whitespace.patch"],
            &["apply", "--whitespace=fix", "patches/whitespace.patch"],
            &["apply", "--whitespace=error", "patches/whitespace.patch"],
            &["apply", "patches/whitespace.patch"],
            // Hunk header three lines off: the offset search.
            &["apply", "--check", "patches/offset.patch"],
            &["apply", "patches/offset.patch"],
            &["apply", "-v", "patches/offset.patch"],
            &["apply", "--check", "-C1", "patches/offset.patch"],
            &["apply", "--check", "--unidiff-zero", "patches/offset.patch"],
            // Binary hunks.
            &["apply", "--check", "patches/binary.patch"],
            &["apply", "patches/binary.patch"],
            &["apply", "--stat", "patches/binary.patch"],
            &["apply", "--numstat", "patches/binary.patch"],
            // A mailbox is not a patch.
            &["apply", "--check", "mail/series.mbox"],
            &["apply", "--stat", "mail/one.eml"],
            // Several patches in one invocation.
            &["apply", "--check", "patches/valid.patch", "patches/whitespace.patch"],
            &["apply", "--check", "patches/valid.patch", "patches/corrupt.patch"],
        ],
        out,
    );
    each(
        Shape::Patches,
        "am",
        &[
            &["am", "mail/series.mbox"],
            &["am", "mail/one.eml"],
            &["am", "-3", "mail/series.mbox"],
            &["am", "-k", "mail/one.eml"],
            &["am", "--signoff", "mail/one.eml"],
            &["am", "--keep-non-patch", "mail/one.eml"],
            &["am", "--committer-date-is-author-date", "mail/one.eml"],
            &["am", "--whitespace=fix", "mail/one.eml"],
            &["am", "--exclude=app/main.c", "mail/one.eml"],
            &["am", "--quiet", "mail/series.mbox"],
            &["am", "--abort"],
            &["am", "--skip"],
            &["am", "mail/missing.mbox"],
        ],
        out,
    );
    each(
        Shape::Patches,
        "mailsplit",
        &[
            &["mailsplit", "-osplit", "mail/series.mbox"],
            &["mailsplit", "-osplit", "-b", "mail/series.mbox"],
            &["mailsplit", "-osplit", "mail/one.eml"],
        ],
        out,
    );
    each(
        Shape::Patches,
        "quiltimport",
        &[
            &["quiltimport", "--patches", "quilt"],
            &["quiltimport", "--dry-run", "--patches", "quilt"],
            &["quiltimport", "--patches", "quilt", "--series", "quilt/series"],
        ],
        out,
    );
    each(
        Shape::Patches,
        "diff",
        &[
            &["diff", "main", "pending"],
            &["diff", "--binary", "main", "pending"],
            &["diff", "--stat", "main", "pending"],
            &["diff", "--numstat", "main", "pending"],
        ],
        out,
    );
}

/// A cone-mode sparse checkout: half the tracked paths are absent from the
/// worktree and carry the skip-worktree bit in the index.
fn sparse(out: &mut Vec<Case>) {
    each(
        Shape::Sparse,
        "sparse-checkout",
        &[
            &["sparse-checkout", "list"],
            &["sparse-checkout", "add", "outside"],
            &["sparse-checkout", "set", "outside"],
            &["sparse-checkout", "set", "inside", "outside"],
            &["sparse-checkout", "reapply"],
            &["sparse-checkout", "disable"],
            &["sparse-checkout", "init", "--cone"],
            &["sparse-checkout", "check-rules", "outside/drop.txt"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "ls-files",
        &[
            &["ls-files"],
            &["ls-files", "-t"],
            &["ls-files", "-v"],
            &["ls-files", "--stage"],
            &["ls-files", "--sparse"],
            &["ls-files", "--others", "--exclude-standard"],
            &["ls-files", "--deleted"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "status",
        &[
            &["status", "--porcelain"],
            &["status", "--porcelain", "-uall"],
            &["status", "--porcelain=v2"],
            &["status", "--short"],
            &["status"],
        ],
        out,
    );
    // `rm` on a sparse-excluded path: refusing it without `--sparse` is the
    // documented behaviour and was a real defect once.
    each(
        Shape::Sparse,
        "rm",
        &[
            &["rm", "outside/drop.txt"],
            &["rm", "--sparse", "outside/drop.txt"],
            &["rm", "-r", "outside"],
            &["rm", "-r", "--sparse", "outside"],
            &["rm", "--cached", "outside/drop.txt"],
            &["rm", "--cached", "--sparse", "outside/drop.txt"],
            &["rm", "-n", "outside/drop.txt"],
            &["rm", "inside/keep.txt"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "add",
        &[
            &["add", "outside/stray.txt"],
            &["add", "--sparse", "outside/stray.txt"],
            &["add", "-A"],
            &["add", "."],
            &["add", "-n", "outside"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "checkout",
        &[
            &["checkout", "HEAD", "--", "outside/drop.txt"],
            &["checkout", "--", "inside/keep.txt"],
            &["checkout", "-b", "topic"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "mv",
        &[
            &["mv", "root.txt", "inside/root.txt"],
            &["mv", "root.txt", "outside/root.txt"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "clean",
        &[&["clean", "-n"], &["clean", "-n", "-d"], &["clean", "-n", "-d", "-x"]],
        out,
    );
    each(
        Shape::Sparse,
        "read-tree",
        &[
            &["read-tree", "-m", "-u", "HEAD"],
            &["read-tree", "HEAD"],
            &["read-tree", "-m", "-u", "--no-sparse-checkout", "HEAD"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "update-index",
        &[
            &["update-index", "--no-skip-worktree", "outside/drop.txt"],
            &["update-index", "--skip-worktree", "inside/keep.txt"],
            &["update-index", "--refresh"],
        ],
        out,
    );
    each(
        Shape::Sparse,
        "diff",
        &[&["diff"], &["diff", "HEAD"], &["diff", "--cached"], &["diff", "--stat", "HEAD"]],
        out,
    );
    each(
        Shape::Sparse,
        "grep",
        &[
            &["grep", "-n", "kept"],
            &["grep", "-n", "excluded"],
            &["grep", "-n", "--cached", "excluded"],
            &["grep", "-n", "excluded", "HEAD"],
        ],
        out,
    );
    each(Shape::Sparse, "stash", &[&["stash", "list"], &["stash", "push", "-u"]], out);
}

/// Outside every repository, from `ni/`.
///
/// A ceiling has to be a *strict* ancestor of the working directory to have any
/// effect: `longest_ancestor_length()` rejects a prefix unless `path[len]` is a
/// `/` with something after it (path.c:1263-1264), so a ceiling equal to the
/// working directory matches nothing, the offset comes back `-1`, and discovery
/// walks up as though nothing were set. Naming the fixture root and running one
/// level below it is what actually stops the walk —
/// verified against stock 2.55.0, which answers `fatal: not a git repository`
/// for `rev-parse --show-toplevel` under this pair and prints the toplevel when
/// the ceiling names `ni/` instead.
const OUTSIDE: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}")];

/// The same, with `core.abbrev = 10` supplied by configuration rather than by
/// the repository that is deliberately out of reach.
///
/// `GIT_CONFIG_KEY_0`/`VALUE_0` rather than `GIT_CONFIG_GLOBAL`, which
/// [`crate::env::harden`] pins to `/dev/null` and
/// [`crate::env::is_pinned`] therefore forbids a case from re-pointing — a case
/// that could aim `GIT_CONFIG_GLOBAL` anywhere could aim it at the machine's own
/// file. The two reach the same place: both are ordinary config sources read by
/// `git_config_from_parameters()` before any repository is looked for, which is
/// all these cases need, because what is under test is whether the width comes
/// from `core.abbrev` at all.
const OUTSIDE_ABBREV_10: &[(&str, &str)] = &[
    ("GIT_CEILING_DIRECTORIES", "{repo}"),
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "core.abbrev"),
    ("GIT_CONFIG_VALUE_0", "10"),
];

/// `diff --no-index`: two directory trees compared to each other, with no
/// repository in reach.
///
/// Three fixes are pinned here, and the first two are only visible in some of
/// the queue shapes:
///
///  * the `index` line and the `--raw` columns are abbreviated to
///    `core.abbrev`, not to a hard-coded 7 — invisible in every other shape,
///    where `auto` and 7 agree;
///  * `--raw` prints the real blob ids when the queue holds both a source and a
///    destination, and zeros when it cannot — `diffcore_rename()` skips its
///    hashing pass on an add-only or delete-only queue
///    (diffcore-rename.c:1461-1462), so a port that always hashes and a port
///    that never does each pass one half of this block;
///  * `--summary` names the side that exists on a delete line, which is why
///    `a.txt /dev/null` is here: the pre-fix output was
///    ` delete mode 000000 /dev/null`.
fn no_index_trees(out: &mut Vec<Case>) {
    for args in [
        // Both halves present: rename detection runs, so the delete and the add
        // carry real ids and the modified pair keeps zeros.
        &["diff", "--no-index", "--raw", "da", "db"][..],
        &["diff", "--no-index", "--summary", "da", "db"],
        // Reversed, which swaps which side the create and delete lines name.
        &["diff", "--no-index", "-R", "--raw", "da", "db"],
        &["diff", "--no-index", "-R", "--summary", "da", "db"],
        // A delete written as an explicit `/dev/null` destination.
        &["diff", "--no-index", "--summary", "a.txt", "/dev/null"],
        // The two degenerate queues, where zeros are the correct answer.
        &["diff", "--no-index", "--raw", "addonly_a", "addonly_b"],
        &["diff", "--no-index", "--raw", "delonly_a", "delonly_b"],
        // The width, spelled every way the option takes it. `--abbrev=2` and
        // `--abbrev=0` run against `a.txt`/`b.txt`, whose single modified pair
        // never gets hashed, so they measure the width applied to the zeros.
        &["diff", "--no-index", "--no-abbrev", "--raw", "da", "db"],
        &["diff", "--no-index", "--abbrev", "--raw", "da", "db"],
        &["diff", "--no-index", "--abbrev=12", "--raw", "da", "db"],
        &["diff", "--no-index", "--abbrev=2", "--raw", "a.txt", "b.txt"],
        &["diff", "--no-index", "--abbrev=-5", "a.txt", "b.txt"],
        &["diff", "--no-index", "--abbrev=0", "--raw", "a.txt", "b.txt"],
    ] {
        out.push(Case::new("diff", args, Shape::NoIndexTrees).in_dir("ni").with_env(OUTSIDE));
    }

    // A value that is not a number. `PARSE_OPT_ERROR` prints the one-line
    // `error: option 'abbrev' expects a numerical value` and exits 129 with **no
    // usage block** after it, which is the half an implementation that answers
    // every parse failure with the usage text gets wrong while still exiting
    // 129 — so the message is the behaviour and this one is compared on stderr.
    out.push(
        Case::strict(
            "diff",
            &["diff", "--no-index", "--abbrev=abc", "a.txt", "b.txt"],
            Shape::NoIndexTrees,
        )
        .in_dir("ni")
        .with_env(OUTSIDE),
    );

    // `core.abbrev = 10`, in play both ways it can be: from a config source
    // with no repository anywhere, and from the repository's own config with
    // the working directory inside it. Without one of these nothing in the
    // corpus distinguishes "abbreviates to `core.abbrev`" from "abbreviates to
    // 7", because every other fixture is small enough that git's `auto` width
    // is 7 as well.
    for args in [
        &["diff", "--no-index", "--raw", "da", "db"][..],
        &["diff", "--no-index", "da", "db"],
    ] {
        out.push(
            Case::new("diff", args, Shape::NoIndexTrees).in_dir("ni").with_env(OUTSIDE_ABBREV_10),
        );
    }
    for args in [
        &["diff", "--no-index", "--raw", "ni/da", "ni/db"][..],
        &["diff", "--no-index", "ni/da", "ni/db"],
    ] {
        out.push(Case::new("diff", args, Shape::NoIndexTrees));
    }
}

/// A decomposed path, through the three places the composition happens.
///
/// The `log` pair is the direct observation of the argv conversion and needs no
/// filesystem at all: git composes *every* argument, not only the ones that name
/// files, so the same format string prints `%H` followed by `é` under
/// `core.precomposeunicode=true` and by `e`+U+0301 under `false`. On macOS the
/// two cases produce different bytes, which is what makes them a test; on Linux
/// the conversion does not exist on either side and both print the decomposed
/// form, so the pair still agrees. Measured both ways against stock 2.55.0 on
/// macOS before being added.
fn decomposed_paths(out: &mut Vec<Case>) {
    // Pathspec against index: the argument arrives decomposed and the index
    // entry is composed (on macOS), so a missing conversion makes `add` report
    // that the path matched no files. The post-state probe reads the index back,
    // which is the `ls-files` half of the check.
    each(
        Shape::DecomposedPaths,
        "add",
        &[&["add", crate::fixture::NFD_TRACKED], &["add", "--", crate::fixture::NFD_TRACKED]],
        out,
    );

    // Every argument is converted, files or not — and the config gate decides
    // whether any of it happens.
    each(
        Shape::DecomposedPaths,
        "log",
        &[
            &["-c", "core.precomposeunicode=true", "log", "-1", NFD_FORMAT],
            &["-c", "core.precomposeunicode=false", "log", "-1", NFD_FORMAT],
        ],
        out,
    );

    // The readdir side, which `gix` already handled: one decomposed path dirty
    // through the index and one untracked, so the walk has to name both.
    each(
        Shape::DecomposedPaths,
        "status",
        &[&["status", "--porcelain"], &["status", "--porcelain", "-uall"]],
        out,
    );
}

/// `--format=%H` followed by the same decomposed `é` the shape's paths carry.
const NFD_FORMAT: &str = "--format=%He\u{301}";
