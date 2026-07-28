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
use crate::runner::Case;

mod diff_family;
mod history_rewrite;
mod info_attrs;
mod mail_patch;
mod maintenance;
mod merge_family;
mod misc_commands;
mod plumbing_objects;
mod plumbing_refs;
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

/// The full curated corpus.
pub fn cases() -> Vec<Case> {
    let mut c = Vec::new();

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
    history_rewrite::cases(&mut c);
    info_attrs::cases(&mut c);
    mail_patch::cases(&mut c);
    maintenance::cases(&mut c);
    merge_family::cases(&mut c);
    misc_commands::cases(&mut c);
    plumbing_objects::cases(&mut c);
    plumbing_refs::cases(&mut c);
    shape_reach::cases(&mut c);
    transport_local::cases(&mut c);
    worktree_index::cases(&mut c);

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
const MBOX: &[u8] = b"From 1234567890abcdef1234567890abcdef12345678 Mon Sep 17 00:00:00 2001\n\
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
