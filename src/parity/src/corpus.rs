//! The hand-written case corpus: known-interesting invocations per subcommand.
//!
//! This is the targeted half of the harness. It encodes the flags and repo
//! states a human knows are load-bearing, so a regression in a common path
//! fails loudly instead of waiting for the fuzzer to stumble into it.
//!
//! Cases here are curated, not exhaustive — `fuzz` covers combinatorial breadth.

use crate::fixture::Shape;
use crate::runner::Case;

/// Shapes every read-only command should agree on regardless of history layout.
const READ_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Detached,
];

/// Expand one read-only invocation across the standard shapes.
fn read_only(cmd: &'static str, args: &[&str], out: &mut Vec<Case>) {
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

    c
}
