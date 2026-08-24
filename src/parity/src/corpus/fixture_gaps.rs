//! Cases for the six shapes added to close the gaps six corpus modules
//! independently recorded as unmeasurable.
//!
//! Grouped by shape, for the reason [`super::shape_reach`] is: the shape *is*
//! what is under test. Each block below asks a question that no case could ask
//! before, because every fixture in the corpus descended from one root, had one
//! merge base for any pair, carried each patch once, stored no symlink and no
//! empty blob, wrote no commit-graph, and was undamaged.
//!
//! What each shape supplies, so a reader does not have to rebuild it from
//! `fixture.rs`:
//!
//! * `Unrelated` — three roots. `main` (two commits), the orphan `alien` (two
//!   commits, tagged `alien-tip`, no path in common with `main`), and the orphan
//!   `alien-clash` (one commit, its own `README.md`).
//! * `CrissCross` — `cc-a` and `cc-b` fork from the base and disagree on
//!   `clash.txt`; `cc-left` and `cc-right` each merged the other and resolved
//!   that disagreement their own way, then moved once more on `cc.txt`. HEAD is
//!   `cc-left`, so `merge cc-right` is one argv. `merge-base --all cc-left
//!   cc-right` prints two ids.
//! * `Cherry` — `main` holds `cherry: shared patch` and `cherry: upstream only`;
//!   `topic` holds `cherry: topic base`, a cherry-picked copy of `cherry: shared
//!   patch` (same patch id, different commit id) and `cherry: topic only`. HEAD
//!   is `topic`.
//! * `Symlinks` — tracked `link-to-file`, `link-to-dir`, `link-broken`,
//!   `link-escape`, `link-to-link`, `dir/link-up`, `link-wt` (retargeted in the
//!   worktree), the empty blobs `empty.txt` and `dir/empty-nested.txt`, the
//!   branch `sym-pending`, `patches/symlink.patch` describing the difference to
//!   it, and the untracked `stray-link` and `stray-empty.txt`.
//! * `CommitGraph` — `.git/objects/info/commit-graph` written with
//!   `--changed-paths` over a history holding a merge (`cg-side`) and an
//!   unmerged fork (`cg-loose`), plus `cg-late`, committed after the write and
//!   therefore absent from the file.
//! * `Damaged` — `refs/heads/dangling` (a valid id, no object),
//!   `refs/heads/broken-symref` (a symref to nothing), a loose object file that
//!   is not a zlib stream, and an empty `.git/objects/info/alternates` entry.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    unrelated_queries(out);
    unrelated_merges(out);
    criss_cross_bases(out);
    criss_cross_merges(out);
    cherry_equivalence(out);
    symlink_reads(out);
    symlink_writes(out);
    commit_graph(out);
    damaged(out);
}

/// Push one case per argv against `shape`.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, shape));
    }
}

// ---------------------------------------------------------------------------
// Unrelated histories
// ---------------------------------------------------------------------------

/// Read-only questions about two revisions with no common ancestor.
///
/// The whole class was previously unaskable: every shape descends from the same
/// `initial` commit, so `merge-base` always had an answer and every traversal
/// always had a boundary. What is measured here is the *empty* answer and the
/// exit code that carries it — `merge-base` exits 1 printing nothing, which is
/// not an error path a port can reach by failing, and `--is-ancestor` answers 1
/// for a question it cannot resolve rather than failing outright. A port that
/// treats "no merge base" as an error prints a diagnostic and exits 128, which
/// agrees with stock on neither.
fn unrelated_queries(out: &mut Vec<Case>) {
    each(
        Shape::Unrelated,
        "merge-base",
        &[
            &["merge-base", "main", "alien"],
            &["merge-base", "--all", "main", "alien"],
            &["merge-base", "--all", "main", "alien", "alien-clash"],
            &["merge-base", "--octopus", "main", "alien"],
            &["merge-base", "--octopus", "main", "alien", "alien-clash"],
            &["merge-base", "--independent", "main", "alien"],
            &["merge-base", "--independent", "main", "alien", "alien-clash"],
            &["merge-base", "--is-ancestor", "main", "alien"],
            &["merge-base", "--is-ancestor", "alien~1", "alien"],
            &["merge-base", "main", "alien-tip"],
            &["merge-base", "alien", "alien-clash"],
            // One argument with no second side: the degenerate form, which
            // answers with the commit itself even here.
            &["merge-base", "--all", "alien"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "rev-list",
        &[
            // Disjoint graphs: `--not` removes nothing, because nothing on the
            // right is reachable from the left.
            &["rev-list", "--count", "main", "--not", "alien"],
            &["rev-list", "--count", "alien", "--not", "main"],
            &["rev-list", "--count", "--all"],
            &["rev-list", "--left-right", "--count", "main...alien"],
            &["rev-list", "--count", "main...alien"],
            // More than one root, which no other shape has.
            &["rev-list", "--max-parents=0", "--all"],
            &["rev-list", "--max-parents=0", "main", "alien"],
            &["rev-list", "--boundary", "main...alien"],
            &["rev-list", "--topo-order", "--all"],
            &["rev-list", "--date-order", "--all"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "log",
        &[
            &["log", "--oneline", "main", "alien"],
            &["log", "--oneline", "--all"],
            &["log", "--graph", "--oneline", "--all"],
            &["log", "--oneline", "main...alien"],
            &["log", "--oneline", "--left-right", "main...alien"],
            &["log", "--oneline", "--boundary", "main...alien"],
            &["log", "--oneline", "--ancestry-path", "main...alien"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "diff",
        &[
            &["diff", "--stat", "main", "alien"],
            &["diff", "--raw", "main", "alien"],
            &["diff", "--name-status", "main", "alien"],
            // Three-dot diff against a pair with no merge base: git falls back
            // to diffing against the empty tree rather than failing.
            &["diff", "--stat", "main...alien"],
            &["diff", "--name-status", "main...alien"],
            &["diff", "--stat", "main", "alien-clash"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "format-patch",
        &[
            &["format-patch", "--stdout", "--no-signature", "main..alien"],
            &["format-patch", "--stdout", "--no-signature", "main...alien"],
            &["format-patch", "--stdout", "--no-signature", "--root", "alien"],
            &["format-patch", "--stdout", "--no-signature", "-1", "alien"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "cherry",
        &[&["cherry", "main", "alien"], &["cherry", "-v", "main", "alien"]],
        out,
    );
    each(
        Shape::Unrelated,
        "branch",
        &[
            &["branch", "--contains", "alien"],
            &["branch", "--no-contains", "alien"],
            &["branch", "-a", "-v"],
            &["branch", "--merged", "main"],
            &["branch", "--no-merged", "main"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "describe",
        &[
            &["describe", "--all", "alien"],
            &["describe", "--all", "--always", "main"],
            &["describe", "--contains", "alien"],
        ],
        out,
    );
    each(
        Shape::Unrelated,
        "name-rev",
        &[&["name-rev", "--all"], &["name-rev", "--annotate-stdin", "--all"]],
        out,
    );
    each(
        Shape::Unrelated,
        "show-branch",
        &[&["show-branch", "main", "alien"], &["show-branch", "--all"]],
        out,
    );
}

/// The merges themselves: the refusal, the flag that lifts it, and the two
/// outcomes on the far side of it.
///
/// `refusing to merge unrelated histories` is a gate no fixture could reach, so
/// a port that never implemented it agreed with stock on every case in the
/// corpus. Past the gate there are two answers, which is why the shape carries
/// two orphans: `alien` shares no path with `main` and merges clean, while
/// `alien-clash` collides on `README.md` and produces an add/add conflict whose
/// stage 1 is *empty* — there is no common ancestor for the path.
fn unrelated_merges(out: &mut Vec<Case>) {
    each(
        Shape::Unrelated,
        "merge",
        &[
            &["merge", "alien"],
            &["merge", "--no-commit", "alien"],
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien"],
            &["merge", "--allow-unrelated-histories", "--no-commit", "alien"],
            &["merge", "--allow-unrelated-histories", "--squash", "alien"],
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "--no-ff", "-m", "join", "alien"],
            &["merge", "--allow-unrelated-histories", "-X", "ours", "-m", "join", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "-m", "join", "alien-tip"],
        ],
        out,
    );

    // `merge-tree` reaches the same strategy without a worktree or an index, so
    // its answer isolates the merge from everything a checkout does about it.
    each(
        Shape::Unrelated,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "main", "alien"],
            &["merge-tree", "--write-tree", "--name-only", "main", "alien"],
            &["merge-tree", "--write-tree", "main", "alien-clash"],
            &["merge-tree", "--write-tree", "--messages", "main", "alien-clash"],
            &["merge-tree", "--write-tree", "--allow-unrelated-histories", "main", "alien"],
        ],
        out,
    );

    // `pull` reaches the refusal through `merge`'s own child process, which is
    // the path that carries the flag across a process boundary. `--no-rebase` is
    // there because git stops earlier without it — an unset `pull.rebase` is a
    // fatal of its own and would measure that instead.
    each(
        Shape::Unrelated,
        "pull",
        &[
            &["pull", "--no-rebase", ".", "alien"],
            &["pull", "--no-rebase", "--allow-unrelated-histories", ".", "alien"],
            &["pull", "--no-rebase", "--allow-unrelated-histories", ".", "alien-clash"],
            &["pull", "--rebase", ".", "alien"],
            &["pull", "--ff-only", ".", "alien"],
        ],
        out,
    );

    each(
        Shape::Unrelated,
        "cherry-pick",
        &[&["cherry-pick", "alien"], &["cherry-pick", "alien~1"]],
        out,
    );
    each(Shape::Unrelated, "rebase", &[&["rebase", "alien"], &["rebase", "--onto", "alien", "main~1"]], out);
}

// ---------------------------------------------------------------------------
// Criss-cross
// ---------------------------------------------------------------------------

/// Two merge bases, and the queries that have to enumerate rather than pick.
///
/// `merge-base --all` on every other shape returns exactly one id whatever the
/// implementation does, so "returns the first base found" and "returns the set"
/// were the same program. Here they are not: the answer is two ids, in a
/// defined order, and `--independent` has a set to prune for the first time.
fn criss_cross_bases(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge-base",
        &[
            &["merge-base", "--all", "cc-left", "cc-right"],
            &["merge-base", "cc-left", "cc-right"],
            &["merge-base", "--all", "HEAD", "cc-right"],
            &["merge-base", "--all", "cc-right", "cc-left"],
            &["merge-base", "--octopus", "cc-left", "cc-right"],
            &["merge-base", "--all", "--octopus", "cc-left", "cc-right"],
            &["merge-base", "--independent", "cc-left", "cc-right", "cc-a", "cc-b"],
            &["merge-base", "--independent", "cc-a", "cc-b"],
            &["merge-base", "--independent", "cc-left", "cc-a"],
            &["merge-base", "--is-ancestor", "cc-a", "cc-right"],
            &["merge-base", "--is-ancestor", "cc-a", "cc-b"],
            &["merge-base", "--all", "cc-a", "cc-b"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "rev-list",
        &[
            &["rev-list", "--count", "cc-left...cc-right"],
            &["rev-list", "--left-right", "--count", "cc-left...cc-right"],
            &["rev-list", "--ancestry-path", "--count", "cc-a..cc-left"],
            &["rev-list", "--ancestry-path", "cc-a..cc-right"],
            &["rev-list", "--topo-order", "--all"],
            &["rev-list", "--merges", "--all"],
            &["rev-list", "--min-parents=2", "--all"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "log",
        &[
            &["log", "--graph", "--oneline", "--all"],
            &["log", "--oneline", "cc-left...cc-right"],
            &["log", "--oneline", "--left-right", "cc-left...cc-right"],
            // A three-dot diff has to pick *one* of the two bases, and which one
            // is an observable consequence of how they are enumerated.
            &["log", "--oneline", "-p", "cc-left...cc-right"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "diff",
        &[
            &["diff", "--stat", "cc-left...cc-right"],
            &["diff", "--name-status", "cc-left...cc-right"],
            &["diff", "--stat", "cc-left", "cc-right"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "show-branch",
        &[
            &["show-branch", "--merge-base", "cc-left", "cc-right"],
            &["show-branch", "--independent", "cc-left", "cc-right", "cc-a", "cc-b"],
            &["show-branch", "cc-left", "cc-right"],
        ],
        out,
    );
}

/// The merge itself, which is the point of the shape.
///
/// With two merge bases the recursive strategy merges the bases with each other
/// and merges against the result. The bases disagree on `clash.txt`, so that
/// virtual base is a conflicted merge and stage 1 of the outer conflict holds a
/// blob containing conflict markers — an object that exists in no commit and
/// that only this code path can produce. The state probe prints `ls-files
/// --stage`, so a port that instead picks one of the two bases is caught by the
/// id in stage 1 even when its stdout is byte-identical.
///
/// `-s resolve` is the control: it picks a single base by design, and its output
/// on this shape is nothing like `ort`'s.
fn criss_cross_merges(out: &mut Vec<Case>) {
    each(
        Shape::CrissCross,
        "merge",
        &[
            &["merge", "cc-right"],
            &["merge", "--no-commit", "cc-right"],
            &["merge", "-s", "ort", "cc-right"],
            &["merge", "-s", "resolve", "cc-right"],
            &["merge", "-X", "ours", "cc-right"],
            &["merge", "-X", "theirs", "cc-right"],
            &["merge", "-X", "diff-algorithm=histogram", "cc-right"],
            &["merge", "--no-ff", "-m", "criss", "cc-right"],
            &["merge", "--abort"],
            &["merge", "cc-a"],
            &["merge", "cc-right", "cc-a"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "--messages", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "--name-only", "cc-left", "cc-right"],
            &["merge-tree", "--write-tree", "-X", "ours", "cc-left", "cc-right"],
            // The pre-`--write-tree` form takes the base explicitly, so it is the
            // one merge here that never has to choose between two.
            &["merge-tree", "cc-a", "cc-left", "cc-right"],
        ],
        out,
    );

    each(
        Shape::CrissCross,
        "cherry-pick",
        &[&["cherry-pick", "cc-right"], &["cherry-pick", "-m", "1", "cc-right~1"]],
        out,
    );
    each(Shape::CrissCross, "rebase", &[&["rebase", "cc-right"], &["rebase", "cc-b"]], out);
    each(
        Shape::CrissCross,
        "revert",
        &[&["revert", "--no-edit", "-m", "1", "HEAD~1"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// An already-applied patch
// ---------------------------------------------------------------------------

/// The patch-id equivalence class: one commit's patch present on both sides of
/// a fork under two different commit ids.
///
/// No fixture carried a patch twice, so `cherry` could only ever print `+`,
/// `--cherry-mark` could only ever print `<` and `>`, and `--cherry-pick` had
/// nothing to drop. An implementation that computed no patch id at all — or
/// computed one that included the commit id, the parent or the timestamps —
/// produced exactly stock's output on every case in the corpus. Here it does
/// not: the marker is `-`, the class is `=`, and `rebase` says
/// `skipped previously applied commit`.
fn cherry_equivalence(out: &mut Vec<Case>) {
    each(
        Shape::Cherry,
        "cherry",
        &[
            &["cherry", "main", "topic"],
            &["cherry", "-v", "main", "topic"],
            &["cherry", "main"],
            &["cherry", "-v", "main"],
            &["cherry", "--abbrev=8", "-v", "main", "topic"],
            &["cherry", "topic", "main"],
            &["cherry", "-v", "main", "topic", "topic~2"],
        ],
        out,
    );

    each(
        Shape::Cherry,
        "rev-list",
        &[
            &["rev-list", "--cherry-mark", "--left-right", "main...topic"],
            &["rev-list", "--cherry-pick", "--right-only", "main...topic"],
            &["rev-list", "--cherry-pick", "--left-only", "main...topic"],
            &["rev-list", "--count", "--cherry-pick", "--left-right", "main...topic"],
            &["rev-list", "--cherry", "main...topic"],
            &["rev-list", "--cherry-mark", "--count", "main...topic"],
            &["rev-list", "--cherry-pick", "--count", "main...topic"],
            &["rev-list", "--left-right", "--count", "main...topic"],
        ],
        out,
    );

    each(
        Shape::Cherry,
        "log",
        &[
            &["log", "--oneline", "--cherry-mark", "--left-right", "main...topic"],
            &["log", "--oneline", "--cherry-pick", "--right-only", "main...topic"],
            &["log", "--oneline", "--cherry", "main...topic"],
            &["log", "--oneline", "--left-right", "main...topic"],
        ],
        out,
    );

    each(
        Shape::Cherry,
        "format-patch",
        &[
            &["format-patch", "--stdout", "--no-signature", "main..topic"],
            &["format-patch", "--stdout", "--no-signature", "--cherry-pick", "main...topic"],
            &["format-patch", "--stdout", "--no-signature", "--cherry-pick", "--right-only", "main...topic"],
        ],
        out,
    );

    // The mutating half. `rebase` has to *skip* the duplicate, and
    // `--reapply-cherry-picks` has to not; `cherry-pick` of a patch already in
    // HEAD stops with an empty commit rather than committing nothing.
    each(
        Shape::Cherry,
        "rebase",
        &[
            &["rebase", "main"],
            &["rebase", "--reapply-cherry-picks", "main"],
            &["rebase", "--keep-base", "main"],
            &["rebase", "--onto", "main", "topic~2"],
            &["rebase", "--no-ff", "main"],
        ],
        out,
    );
    each(
        Shape::Cherry,
        "cherry-pick",
        &[
            &["cherry-pick", "main~1"],
            &["cherry-pick", "--allow-empty", "--no-edit", "main~1"],
            &["cherry-pick", "--empty=drop", "main~1"],
            &["cherry-pick", "-x", "main"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// Symlinks and empty blobs
// ---------------------------------------------------------------------------

/// The four answers `cat-file --follow-symlinks` has, plus the reads that see a
/// `120000` entry or a zero-length blob for the first time.
///
/// Each payload below is one `<rev>:<path>` request, and each lands on a
/// different branch of the resolver: a blob, a blob reached *through* a
/// symlinked directory, `dangling` for a target no tree entry has, and `symlink`
/// for a target that leaves the tree — the last two being reports rather than
/// failures, which is the part an implementation that returns an error instead
/// gets wrong.
fn symlink_reads(out: &mut Vec<Case>) {
    const FOLLOW: &[&[u8]] = &[
        b"HEAD:link-to-file\n",
        b"HEAD:link-to-dir/target.txt\n",
        b"HEAD:link-broken\n",
        b"HEAD:link-escape\n",
        b"HEAD:link-to-link\n",
        b"HEAD:dir/link-up\n",
        b"HEAD:empty.txt\n",
        b"HEAD:link-to-file\nHEAD:link-broken\nHEAD:link-escape\nHEAD:empty.txt\n",
    ];
    for payload in FOLLOW {
        out.push(Case::with_stdin(
            "cat-file",
            &["cat-file", "--batch", "--follow-symlinks"],
            Shape::Symlinks,
            payload,
        ));
        out.push(Case::with_stdin(
            "cat-file",
            &["cat-file", "--batch-check", "--follow-symlinks"],
            Shape::Symlinks,
            payload,
        ));
        // Without the flag the same request answers about the *link* rather than
        // its target, which is the comparison that makes the flag measurable.
        out.push(Case::with_stdin("cat-file", &["cat-file", "--batch"], Shape::Symlinks, payload));
    }

    each(
        Shape::Symlinks,
        "cat-file",
        &[
            &["cat-file", "-t", "HEAD:link-to-file"],
            &["cat-file", "-s", "HEAD:link-to-file"],
            &["cat-file", "-p", "HEAD:link-to-file"],
            &["cat-file", "blob", "HEAD:link-escape"],
            &["cat-file", "-s", "HEAD:empty.txt"],
            &["cat-file", "-t", "HEAD:empty.txt"],
            &["cat-file", "-p", "HEAD:empty.txt"],
            &["cat-file", "-e", "HEAD:empty.txt"],
            // The empty blob by its id, which is a constant of the hash function
            // and not of this fixture.
            &["cat-file", "-t", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"],
            &["cat-file", "-s", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "ls-files",
        &[
            &["ls-files", "--stage"],
            &["ls-files", "-s", "--", "link-to-file", "link-broken", "empty.txt"],
            &["ls-files", "-t"],
            &["ls-files", "--eol"],
            &["ls-files", "--others", "--exclude-standard"],
            &["ls-files", "--modified"],
            &["ls-files", "--format=%(objectmode) %(path)"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "ls-tree",
        &[
            &["ls-tree", "HEAD"],
            &["ls-tree", "-r", "HEAD"],
            &["ls-tree", "-r", "-l", "HEAD"],
            &["ls-tree", "-r", "-t", "HEAD"],
            &["ls-tree", "--name-only", "-r", "HEAD"],
            &["ls-tree", "-r", "sym-pending"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "diff",
        &[
            // The worktree edit is a retargeted symlink: the diff is of a link's
            // *content*, which is its target path.
            &["diff"],
            &["diff", "--stat"],
            &["diff", "--raw"],
            &["diff", "--summary"],
            &["diff", "--numstat"],
            // Includes a regular file replaced by a symlink, which `--raw` scores
            // `T` and no other shape can produce.
            &["diff", "--raw", "main", "sym-pending"],
            &["diff", "--summary", "main", "sym-pending"],
            &["diff", "--stat", "main", "sym-pending"],
            &["diff", "main", "sym-pending"],
            &["diff", "--no-renames", "--raw", "main", "sym-pending"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "status",
        &[
            &["status", "--porcelain", "-uall"],
            &["status", "--porcelain=v2", "-uall"],
            &["status", "--short"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "archive",
        &[
            &["archive", "--format=tar", "HEAD"],
            &["archive", "--format=tar", "--prefix=root/", "HEAD"],
            &["archive", "--format=tar", "HEAD", "link-to-file", "empty.txt"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "hash-object",
        &[
            &["hash-object", "empty.txt"],
            &["hash-object", "-t", "blob", "empty.txt"],
            &["hash-object", "--", "link-to-file"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "apply",
        &[
            &["apply", "--check", "patches/symlink.patch"],
            &["apply", "--stat", "patches/symlink.patch"],
            &["apply", "--numstat", "patches/symlink.patch"],
            &["apply", "--summary", "patches/symlink.patch"],
            &["apply", "--check", "--reverse", "patches/symlink.patch"],
        ],
        out,
    );
}

/// The writing half: what each verb does when the entry it has to create,
/// remove or replace is a symlink or an empty file.
///
/// `apply` has to create a `120000` entry and a zero-byte file from a patch that
/// carries no hunk for the latter; `checkout` has to replace a regular file with
/// a symlink; `add` has to store a link's target rather than its target's
/// content. Every one of those is a distinct write path and none had a fixture.
fn symlink_writes(out: &mut Vec<Case>) {
    each(
        Shape::Symlinks,
        "apply",
        &[
            &["apply", "patches/symlink.patch"],
            &["apply", "--index", "patches/symlink.patch"],
            &["apply", "--cached", "patches/symlink.patch"],
            &["apply", "-3", "patches/symlink.patch"],
        ],
        out,
    );

    each(
        Shape::Symlinks,
        "checkout",
        &[
            &["checkout", "--", "link-wt"],
            &["checkout", "-q", "sym-pending"],
            &["checkout", "sym-pending", "--", "dir/target.txt"],
        ],
        out,
    );
    each(
        Shape::Symlinks,
        "restore",
        &[
            &["restore", "link-wt"],
            &["restore", "--source=sym-pending", "dir/target.txt"],
            &["restore", "--staged", "--source=sym-pending", "later-link"],
        ],
        out,
    );
    each(
        Shape::Symlinks,
        "add",
        &[
            &["add", "-A"],
            &["add", "stray-link"],
            &["add", "stray-empty.txt"],
            &["add", "link-wt"],
        ],
        out,
    );
    each(
        Shape::Symlinks,
        "rm",
        &[&["rm", "-f", "link-to-file"], &["rm", "--cached", "empty.txt"]],
        out,
    );
    each(Shape::Symlinks, "mv", &[&["mv", "link-to-file", "moved-link"], &["mv", "empty.txt", "dir/"]], out);
    each(
        Shape::Symlinks,
        "clean",
        &[&["clean", "-n"], &["clean", "-f"], &["clean", "-n", "-d"]],
        out,
    );
    each(
        Shape::Symlinks,
        "stash",
        &[&["stash", "push", "-u", "-m", "links"], &["stash", "push", "-m", "links"]],
        out,
    );
    each(
        Shape::Symlinks,
        "update-index",
        &[&["update-index", "--refresh"], &["update-index", "--again"]],
        out,
    );
}

// ---------------------------------------------------------------------------
// Commit-graph
// ---------------------------------------------------------------------------

/// The same traversals with the graph on and off.
///
/// Nothing here is interesting on a shape without a graph file: `core.commitGraph`
/// selects between two implementations of the same answer, so with no file to
/// read both settings run the same code and every case agrees whatever the port
/// does. With a file, the paired cases below either agree with each other — as
/// they must — or expose a graph reader that disagrees with the object store.
///
/// `cg-late` is not in the graph, so every traversal that reaches it has to mix
/// graph-supplied generation numbers with computed ones. `--changed-paths` wrote
/// the Bloom filters, so `log -- <path>` takes the filtered path.
fn commit_graph(out: &mut Vec<Case>) {
    const READS: &[&[&str]] = &[
        &["log", "--oneline"],
        &["log", "--oneline", "--all"],
        &["log", "--graph", "--oneline", "--all"],
        &["log", "--oneline", "--topo-order", "--all"],
        &["log", "--oneline", "--", "cg.txt"],
        &["log", "--oneline", "--", "cg-late.txt"],
        &["log", "--oneline", "--", "cg-side.txt"],
    ];
    for args in READS {
        for on in ["true", "false"] {
            out.push(
                Case::new("log", args, Shape::CommitGraph)
                    .with_config(&[("core.commitGraph", on)]),
            );
        }
    }

    const WALKS: &[&[&str]] = &[
        &["rev-list", "--count", "HEAD"],
        &["rev-list", "--count", "--all"],
        &["rev-list", "--topo-order", "--all"],
        &["rev-list", "--count", "cg-loose..main"],
        &["rev-list", "--count", "--merges", "--all"],
    ];
    for args in WALKS {
        for on in ["true", "false"] {
            out.push(
                Case::new("rev-list", args, Shape::CommitGraph)
                    .with_config(&[("core.commitGraph", on)]),
            );
        }
    }

    each(
        Shape::CommitGraph,
        "merge-base",
        &[
            &["merge-base", "main", "cg-loose"],
            &["merge-base", "--all", "main", "cg-loose"],
            &["merge-base", "--is-ancestor", "cg-loose", "main"],
            &["merge-base", "--octopus", "main", "cg-loose", "cg-side"],
        ],
        out,
    );

    each(
        Shape::CommitGraph,
        "commit-graph",
        &[
            &["commit-graph", "verify"],
            &["commit-graph", "verify", "--no-progress"],
            &["commit-graph", "verify", "--shallow"],
            &["commit-graph", "write", "--reachable"],
            &["commit-graph", "write", "--reachable", "--changed-paths"],
            &["commit-graph", "write", "--reachable", "--no-progress"],
            &["commit-graph", "write", "--reachable", "--split"],
            &["commit-graph", "write", "--stdin-commits"],
        ],
        out,
    );

    each(
        Shape::CommitGraph,
        "name-rev",
        &[&["name-rev", "--all"], &["name-rev", "--name-only", "HEAD"]],
        out,
    );
    each(
        Shape::CommitGraph,
        "describe",
        &[&["describe", "--always", "HEAD"], &["describe", "--all", "--always", "cg-loose"]],
        out,
    );
    // Both rewrite or repack around the graph file, and the storage probe sees
    // whether the file survived, was rewritten, or was dropped.
    each(Shape::CommitGraph, "gc", &[&["gc"], &["gc", "--prune=now"], &["gc", "--auto"]], out);
    each(Shape::CommitGraph, "fsck", &[&["fsck", "--no-progress"]], out);
    each(Shape::CommitGraph, "repack", &[&["repack", "-adq"]], out);
}

// ---------------------------------------------------------------------------
// A damaged repository
// ---------------------------------------------------------------------------

/// What each verb does about damage it did not cause.
///
/// The four defects are deliberately not equivalent, and neither are the
/// answers: `rev-parse --verify refs/heads/dangling` prints the id and exits 0
/// even though no object has it, while `show-ref` and `for-each-ref` fail with
/// exit 128 over the same ref; `rev-parse --verify refs/heads/broken-symref`
/// warns and fails; `branch --list` prints `dangling` and hides
/// `broken-symref`; `cat-file --batch-all-objects` lists the corrupt object as
/// `missing` and still exits 0. A port with one notion of "broken ref" cannot
/// match more than a few of those.
///
/// The mutating half is the harder question: `gc`, `prune`, `pack-refs` and
/// `repack` each have to decide whether to proceed, and what to leave behind
/// when they do. The state probe compares what survived.
fn damaged(out: &mut Vec<Case>) {
    each(
        Shape::Damaged,
        "fsck",
        &[
            &["fsck", "--no-progress"],
            &["fsck", "--no-progress", "--strict"],
            &["fsck", "--no-progress", "--connectivity-only"],
            &["fsck", "--no-progress", "--unreachable"],
            &["fsck", "--no-progress", "--dangling"],
            &["fsck", "--no-progress", "--no-dangling"],
            &["fsck", "--no-progress", "--name-objects"],
            &["fsck", "--no-progress", "--root"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "rev-parse",
        &[
            &["rev-parse", "--verify", "refs/heads/dangling"],
            &["rev-parse", "refs/heads/dangling"],
            &["rev-parse", "--verify", "refs/heads/broken-symref"],
            &["rev-parse", "--verify", "HEAD"],
            &["rev-parse", "--all"],
            &["rev-parse", "--branches"],
            &["rev-parse", "--verify", "refs/heads/dangling^{commit}"],
            &["rev-parse", "--symbolic-full-name", "refs/heads/broken-symref"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "show-ref",
        &[
            &["show-ref"],
            &["show-ref", "--heads"],
            &["show-ref", "--verify", "refs/heads/dangling"],
            &["show-ref", "--verify", "refs/heads/broken-symref"],
            &["show-ref", "dangling"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "for-each-ref",
        &[
            &["for-each-ref"],
            &["for-each-ref", "--format=%(refname)"],
            &["for-each-ref", "--format=%(refname) %(objectname)"],
            &["for-each-ref", "refs/heads/main"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "branch",
        &[
            &["branch", "--list"],
            &["branch", "-a"],
            &["branch", "-v"],
            &["branch", "--format=%(refname:short)"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "symbolic-ref",
        &[
            &["symbolic-ref", "refs/heads/broken-symref"],
            &["symbolic-ref", "-q", "refs/heads/broken-symref"],
            &["symbolic-ref", "HEAD"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "cat-file",
        &[
            &["cat-file", "-t", crate::fixture::CORRUPT_OBJECT],
            &["cat-file", "-s", crate::fixture::CORRUPT_OBJECT],
            &["cat-file", "-e", crate::fixture::CORRUPT_OBJECT],
            &["cat-file", "-p", crate::fixture::CORRUPT_OBJECT],
            &["cat-file", "-t", crate::fixture::MISSING_OBJECT],
            &["cat-file", "--batch-check", "--batch-all-objects"],
            &["cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)"],
        ],
        out,
    );

    each(
        Shape::Damaged,
        "count-objects",
        &[&["count-objects"], &["count-objects", "-v"], &["count-objects", "-H"]],
        out,
    );

    each(
        Shape::Damaged,
        "log",
        &[&["log", "--oneline"], &["log", "--oneline", "--all"], &["log", "--oneline", "--branches"]],
        out,
    );
    each(Shape::Damaged, "status", &[&["status", "--porcelain"], &["status"]], out);
    each(
        Shape::Damaged,
        "rev-list",
        &[&["rev-list", "--count", "HEAD"], &["rev-list", "--all", "--count"]],
        out,
    );

    // The mutating half.
    each(
        Shape::Damaged,
        "update-ref",
        &[
            &["update-ref", "-d", "refs/heads/dangling"],
            &["update-ref", "-d", "refs/heads/broken-symref"],
            &["update-ref", "--no-deref", "-d", "refs/heads/broken-symref"],
        ],
        out,
    );
    each(
        Shape::Damaged,
        "branch",
        &[&["branch", "-D", "dangling"], &["branch", "-D", "broken-symref"]],
        out,
    );
    each(Shape::Damaged, "pack-refs", &[&["pack-refs", "--all"], &["pack-refs"]], out);
    each(Shape::Damaged, "prune", &[&["prune", "-n"], &["prune"], &["prune", "--expire=now"]], out);
    each(Shape::Damaged, "gc", &[&["gc"], &["gc", "--prune=now"], &["gc", "--auto"]], out);
    each(Shape::Damaged, "repack", &[&["repack", "-adq"], &["repack", "-q"]], out);
    each(Shape::Damaged, "fetch", &[&["fetch", "--prune", "."]], out);
}
