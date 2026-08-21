//! The `TREE` (cache-tree) extension: it must be *present*, *correct*, and
//! *exactly as invalidated* as stock git leaves it.
//!
//! The extension caches, per directory, the id of the tree that directory's index
//! entries hash to plus the number of entries it covers, so `git write-tree` and
//! `git commit` can skip whole unchanged subdirectories (`update_one()` returns
//! early on a node whose `entry_count` is non-negative and whose object is present,
//! `cache-tree.c:336-339`). A missing extension only costs time. A *stale* one is a
//! correctness bug: `write-tree` would hand back a cached id for a directory whose
//! entries have since changed, and the resulting commit would record content nobody
//! staged.
//!
//! So the assertions come in two flavours, and the interop ones are the ones that
//! matter:
//!
//! * shape — the invalidation pattern a verb leaves behind, read straight out of
//!   the index file, because "the root went invalid but `other/` kept its id" is
//!   the whole point and is invisible from any command's output.
//! * agreement — **stock** git reads an index this binary wrote and must produce
//!   the same tree from it, pass `git fsck`, and survive
//!   `GIT_TEST_CHECK_CACHE_TREE=1`, which makes stock verify every cached node
//!   against the entries it claims to summarise (`cache_tree_verify()`,
//!   read-cache.c:3329-3331). That last one is the strongest check available: it is
//!   git's own auditor, run against our output.
//!
//! Every stock-git test degrades to a skip when no stock git is on the machine, so
//! the file is safe in a headless CI that only has the binary under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

// ---------------------------------------------------------------------------
// process plumbing
// ---------------------------------------------------------------------------

/// Run `bin` in `repo` with an isolated, deterministic environment so no ambient
/// config or identity can reach the run.
fn run_with(bin: &str, repo: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

/// [`run_with`] for the binary under test.
fn run(repo: &Path, args: &[&str]) -> Output {
    run_with(BIN, repo, args)
}

/// [`run_with`] asserting success and returning trimmed stdout.
fn ok_with(bin: &str, repo: &Path, args: &[&str]) -> String {
    let out = run_with(bin, repo, args);
    assert!(
        out.status.success(),
        "`{bin} {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// [`ok_with`] for the binary under test.
fn ok(repo: &Path, args: &[&str]) -> String {
    ok_with(BIN, repo, args)
}

/// A stock git that is definitely *not* this binary, or `None` to skip.
///
/// `zjobs` is a zvcs-only verb: stock git fails on it, this binary succeeds. The
/// `--version` probe alone is not enough — a shim can report an upstream version
/// while dispatching somewhere else entirely — so both must hold.
fn stock_git() -> Option<String> {
    for cand in [
        "/opt/homebrew/bin/git",
        "/usr/bin/git",
        "/usr/local/bin/git",
        "git",
    ] {
        let version = Command::new(cand).arg("--version").output();
        let Ok(version) = version else { continue };
        if !version.status.success() || !version.stdout.starts_with(b"git version") {
            continue;
        }
        match Command::new(cand).arg("zjobs").output() {
            Ok(out) if !out.status.success() => return Some(cand.to_string()),
            _ => continue,
        }
    }
    None
}

/// A fresh, empty directory named after `tag`.
fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-cache-tree-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

// ---------------------------------------------------------------------------
// reading the extension back out of the index file
// ---------------------------------------------------------------------------

/// One `TREE` node as it appears on disk, in file order.
///
/// `write_one()` emits `path NUL entry_count SP subtree_nr LF [hash]`, with the
/// hash present only when `entry_count >= 0` (cache-tree.c:555-577), then recurses
/// into the subtrees. `entries: None` is git's `entry_count = -1`, i.e. invalid.
#[derive(Debug, PartialEq, Eq)]
struct Node {
    /// The directory's own name; empty for the root.
    name: String,
    /// `entry_count`, or `None` when the node is invalid.
    entries: Option<u32>,
    /// How many subtrees follow this node.
    subtrees: usize,
    /// The cached tree id, present exactly when `entries` is.
    oid: Option<String>,
}

/// Parse the extensions out of an index file, returning the `TREE` nodes in the
/// order they were written, or `None` when the index carries no `TREE` at all.
///
/// Only index versions 2 and 3 are handled: version 4 path-compresses entry names,
/// and nothing in this binary writes it (`detect_required_version()` picks V2 or V3
/// from the entry flags alone).
fn tree_nodes(index: &Path) -> Option<Vec<Node>> {
    let data = std::fs::read(index).unwrap_or_else(|e| panic!("read {}: {e}", index.display()));
    assert_eq!(&data[..4], b"DIRC", "not an index file: {}", index.display());
    let version = u32::from_be_bytes(data[4..8].try_into().unwrap());
    assert!(
        version <= 3,
        "index version {version} is path-compressed; this parser only reads v2/v3"
    );
    let count = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;

    // Fixed 62-byte head — plus the two extra flag bytes a v3 entry carries when
    // its `CE_EXTENDED` bit is set, which is how intent-to-add and skip-worktree
    // are recorded — then a NUL-terminated name, then NUL padding to the next
    // 8-byte boundary measured from the start of the entry.
    let mut off = 12;
    for _ in 0..count {
        let start = off;
        let flags = u16::from_be_bytes(data[off + 60..off + 62].try_into().unwrap());
        off += 62;
        if version >= 3 && flags & 0x4000 != 0 {
            off += 2;
        }
        off += data[off..].iter().position(|&b| b == 0).expect("entry name is NUL terminated");
        off += 8 - ((off - start) % 8);
    }

    // Extensions run until the trailing checksum.
    while off + 8 <= data.len() - 20 {
        let sig = &data[off..off + 4];
        let size = u32::from_be_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
        let body = &data[off + 8..off + 8 + size];
        if sig == b"TREE" {
            return Some(parse_tree_body(body));
        }
        off += 8 + size;
    }
    None
}

/// Decode a `TREE` extension body into its nodes, in file order.
fn parse_tree_body(body: &[u8]) -> Vec<Node> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let nul = i + body[i..].iter().position(|&b| b == 0).expect("node name is NUL terminated");
        let name = String::from_utf8_lossy(&body[i..nul]).into_owned();
        i = nul + 1;
        let lf = i + body[i..].iter().position(|&b| b == b'\n').expect("node header ends in LF");
        let header = std::str::from_utf8(&body[i..lf]).expect("node header is ASCII");
        i = lf + 1;
        let (count, subs) = header.split_once(' ').expect("`<entry_count> <subtree_nr>`");
        let count: i64 = count.parse().expect("entry_count is an integer");
        let subtrees: usize = subs.parse().expect("subtree_nr is an integer");
        let (entries, oid) = if count >= 0 {
            let hex = body[i..i + 20].iter().map(|b| format!("{b:02x}")).collect::<String>();
            i += 20;
            (Some(count as u32), Some(hex))
        } else {
            (None, None)
        };
        out.push(Node { name, entries, subtrees, oid });
    }
    out
}

/// The nodes of the repository's own index, panicking when there is no `TREE`.
fn nodes(repo: &Path) -> Vec<Node> {
    tree_nodes(&repo.join(".git/index"))
        .unwrap_or_else(|| panic!("{} has no TREE extension", repo.display()))
}

/// Look one node up by name (names are unique within these fixtures).
fn node<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no TREE node named {name:?} in {nodes:#?}"))
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// A repository with one commit over `root.txt`, `sub/a.txt`, `sub/deeper/d.txt`
/// and `other/o.txt` — two independent subtrees and one nested one, so "only the
/// touched branch was invalidated" is actually observable.
///
/// Built with `bin` so the caller decides whose index it starts from.
fn fixture(bin: &str, tag: &str) -> PathBuf {
    let repo = tmp(tag);
    ok_with(bin, &repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("sub/deeper")).unwrap();
    std::fs::create_dir_all(repo.join("other")).unwrap();
    std::fs::write(repo.join("root.txt"), b"root\n").unwrap();
    std::fs::write(repo.join("sub/a.txt"), b"a\n").unwrap();
    std::fs::write(repo.join("sub/deeper/d.txt"), b"d\n").unwrap();
    std::fs::write(repo.join("other/o.txt"), b"o\n").unwrap();
    ok_with(bin, &repo, &["add", "-A"]);
    ok_with(bin, &repo, &["commit", "-q", "-m", "base"]);
    repo
}

// ---------------------------------------------------------------------------
// shape: what each verb leaves in the extension
// ---------------------------------------------------------------------------

/// `git add` must invalidate the path it staged and every directory above it —
/// and nothing else.
///
/// This is `cache_tree_invalidate_path()` walking down from the root, marking each
/// node it passes and deleting the node named by the last component
/// (cache-tree.c:113-157). Dropping the whole extension instead would be *safe*
/// but would throw away `other/`'s tree id for no reason, which is exactly the
/// regression this pins.
#[test]
fn add_invalidates_the_staged_path_and_its_ancestors_only() {
    let repo = fixture(BIN, "add-shape");
    // The base commit left a fully valid cache-tree; nothing below is meaningful
    // otherwise.
    for n in nodes(&repo) {
        assert!(n.entries.is_some(), "commit must leave every node valid, got {n:?}");
    }

    std::fs::write(repo.join("sub/deeper/d.txt"), b"changed\n").unwrap();
    ok(&repo, &["add", "sub/deeper/d.txt"]);

    let after = nodes(&repo);
    assert_eq!(node(&after, "").entries, None, "the root must go invalid");
    assert_eq!(node(&after, "sub").entries, None, "`sub` is on the path, so it goes invalid");
    assert_eq!(
        node(&after, "deeper").entries,
        None,
        "the node for the staged file's own directory must go invalid"
    );
    let other = node(&after, "other");
    assert!(
        other.entries.is_some() && other.oid.is_some(),
        "a directory the add never touched must keep its cached tree id, got {other:?}"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// `git commit` must leave a *fully valid* cache-tree whose root is the tree the
/// commit records.
///
/// That equality is the load-bearing part: git takes the commit's tree straight
/// from the cache-tree root (`commit_tree_extended(..., &index->cache_tree->oid,
/// ...)`, builtin/commit.c:1938), so a root that disagreed with `HEAD^{tree}`
/// would mean the extension and the history had diverged.
#[test]
fn commit_leaves_a_valid_cache_tree_naming_the_commit_tree() {
    let repo = fixture(BIN, "commit-shape");
    std::fs::write(repo.join("sub/a.txt"), b"a2\n").unwrap();
    ok(&repo, &["add", "sub/a.txt"]);
    ok(&repo, &["commit", "-q", "-m", "second"]);

    let after = nodes(&repo);
    for n in &after {
        assert!(
            n.entries.is_some() && n.oid.is_some(),
            "every node must be valid after a commit, got {n:?}"
        );
    }
    let head_tree = ok(&repo, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        node(&after, "").oid.as_deref(),
        Some(head_tree.as_str()),
        "the cache-tree root must be the tree the commit recorded"
    );
    assert_eq!(
        node(&after, "").entries,
        Some(4),
        "the root's entry count is the number of index entries it covers"
    );
    assert_eq!(node(&after, "sub").entries, Some(2), "`sub` covers a.txt and deeper/d.txt");

    let _ = std::fs::remove_dir_all(&repo);
}

/// A `write-tree` over an already-valid cache-tree must print the cached root and
/// leave the index file byte-for-byte alone.
///
/// `write_index_as_tree()` only writes the index when the cache-tree was *not*
/// already valid (`if (!ret && !was_valid) write_locked_index(...)`,
/// cache-tree.c:818-819). Rewriting it anyway would churn the file — and its stat
/// data — on a command that is supposed to be a pure read in that case.
#[test]
fn write_tree_reuses_a_valid_cache_tree_without_rewriting_the_index() {
    let repo = fixture(BIN, "write-tree-reuse");
    let index = repo.join(".git/index");

    // Freshly committed: valid cache-tree, so this is the reuse path.
    let before = std::fs::read(&index).unwrap();
    let printed = ok(&repo, &["write-tree"]);
    assert_eq!(
        std::fs::read(&index).unwrap(),
        before,
        "write-tree must not rewrite an index whose cache-tree is already valid"
    );
    assert_eq!(
        Some(printed.as_str()),
        node(&nodes(&repo), "").oid.as_deref(),
        "the id printed must be the one the extension already cached"
    );

    // Invalidate one path: now write-tree has work to do, and must persist the
    // refreshed extension so the next reader gets it for free.
    std::fs::write(repo.join("other/o.txt"), b"o2\n").unwrap();
    ok(&repo, &["add", "other/o.txt"]);
    assert_eq!(node(&nodes(&repo), "").entries, None, "add must have invalidated the root");
    let recomputed = ok(&repo, &["write-tree"]);
    let after = nodes(&repo);
    assert_eq!(
        node(&after, "").oid.as_deref(),
        Some(recomputed.as_str()),
        "write-tree must write the recomputed root back into the index"
    );
    for n in &after {
        assert!(n.entries.is_some(), "write-tree must leave every node valid, got {n:?}");
    }

    let _ = std::fs::remove_dir_all(&repo);
}

/// `git update-index` invalidates per path, exactly like the porcelain does.
///
/// git's invalidation lives in `add_index_entry_with_check()`
/// (read-cache.c:1273-1274), i.e. below every verb rather than inside any of them,
/// so plumbing that adds one entry must leave the same shape `git add` does.
#[test]
fn update_index_invalidates_per_path() {
    let repo = fixture(BIN, "update-index-shape");
    std::fs::write(repo.join("sub/a.txt"), b"a2\n").unwrap();
    ok(&repo, &["update-index", "sub/a.txt"]);

    let after = nodes(&repo);
    assert_eq!(node(&after, "").entries, None, "the root must go invalid");
    assert_eq!(node(&after, "sub").entries, None, "`sub` holds the changed entry");
    assert!(
        node(&after, "deeper").entries.is_some(),
        "`sub/deeper` was not touched and must keep its id"
    );
    assert!(
        node(&after, "other").entries.is_some(),
        "`other` was not touched and must keep its id"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// An as-is `git commit` rewrites the index when — and only when — git does.
///
/// git's condition is not "always": `prepare_index()` updates the cache-tree if
/// `cache_changed || !cache_tree_fully_valid(...)` (builtin/commit.c:486-488), and
/// only `cache_tree_update()` setting `CACHE_TREE_CHANGED` gets the following
/// `write_locked_index(..., SKIP_IF_UNCHANGED)` past its guard
/// (read-cache.c:3333). So a commit after a `git add` writes — the add invalidated
/// the root — and a second commit over an untouched index does not.
///
/// Forcing an unconditional write would reproduce the first line of this test and
/// fail the second, which is exactly why the second line is here.
#[test]
fn commit_writes_the_index_exactly_when_git_does() {
    let repo = fixture(BIN, "commit-write-gate");
    let index = repo.join(".git/index");

    std::fs::write(repo.join("sub/a.txt"), b"a2\n").unwrap();
    ok(&repo, &["add", "sub/a.txt"]);
    let staged = std::fs::read(&index).unwrap();
    ok(&repo, &["commit", "-q", "-m", "second"]);
    assert_ne!(
        std::fs::read(&index).unwrap(),
        staged,
        "a commit after `add` must rewrite the index: the add left the cache-tree invalid"
    );

    // Nothing has touched the index since, so its cache-tree is fully valid and
    // there is nothing for git to write.
    let committed = std::fs::read(&index).unwrap();
    ok(&repo, &["commit", "-q", "--allow-empty", "-m", "third"]);
    assert_eq!(
        std::fs::read(&index).unwrap(),
        committed,
        "a commit over an already-valid cache-tree must leave the index alone"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// `git read-tree <tree>` primes the cache-tree straight from the tree object, and
/// every other shape falls back to the repair pass that invents nothing.
///
/// The one-tree read is the only case where "the index must match exactly what came
/// from the tree" holds, which is why it is the only case git primes
/// (builtin/read-tree.c:281-290). A `--prefix` read mixes the old index with the
/// bound tree, so it goes through `unpack_trees()`'s
/// `WRITE_TREE_REPAIR` pass instead, which validates a node only when the tree it
/// derived is already in the odb (cache-tree.c:490-497) — so nodes below the bind
/// point can still come out valid while the root, a tree nobody has ever stored,
/// does not.
#[test]
fn read_tree_primes_from_one_tree_and_repairs_otherwise() {
    let repo = fixture(BIN, "read-tree-prime");
    let head_tree = ok(&repo, &["rev-parse", "HEAD^{tree}"]);

    // Dirty the index first, so a primed cache-tree cannot be mistaken for one that
    // simply survived from the commit.
    std::fs::write(repo.join("sub/a.txt"), b"dirty\n").unwrap();
    ok(&repo, &["add", "sub/a.txt"]);
    assert_eq!(node(&nodes(&repo), "").entries, None, "add must invalidate the root");

    ok(&repo, &["read-tree", &head_tree]);
    let primed = nodes(&repo);
    for n in &primed {
        assert!(
            n.entries.is_some(),
            "a one-tree read-tree must leave every node valid, got {n:?}"
        );
    }
    assert_eq!(
        node(&primed, "").oid.as_deref(),
        Some(head_tree.as_str()),
        "the primed root must be the tree that was read"
    );
    assert_eq!(node(&primed, "").entries, Some(4), "four blobs live under that tree");
    assert_eq!(node(&primed, "sub").entries, Some(2), "`sub` holds a.txt and deeper/d.txt");
    assert_eq!(ok(&repo, &["write-tree"]), head_tree, "and it must build back to it");

    // A `--prefix` read is not any single tree, so nothing may be primed: the root
    // must be invalid, while the bound subtree is a tree the repository does have
    // and the repair pass may keep it.
    let bound = fixture(BIN, "read-tree-prefix");
    ok(&bound, &["read-tree", "--prefix=vendor/", &ok(&bound, &["rev-parse", "HEAD^{tree}"])]);
    let after = tree_nodes(&bound.join(".git/index"));
    if let Some(after) = after {
        assert_eq!(
            node(&after, "").entries,
            None,
            "a --prefix read must not leave a valid root: no such tree exists"
        );
    }
    assert_eq!(
        ok(&bound, &["write-tree"]),
        ok(&bound, &["write-tree"]),
        "and the tree it builds must be stable across runs"
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&bound);
}

// ---------------------------------------------------------------------------
// agreement: stock git reading what this binary wrote
// ---------------------------------------------------------------------------

/// Ask stock git to audit an index this binary wrote, three ways.
///
/// `GIT_TEST_CHECK_CACHE_TREE=1` is the important one: it makes stock run
/// `cache_tree_verify()` before every index write (read-cache.c:3329-3331), which
/// walks each cached node and re-derives it from the entries it claims to cover.
/// A cache-tree that is merely *present* passes nothing; this is what proves it is
/// also *right*.
fn stock_audits(git: &str, repo: &Path, what: &str) {
    let debug = run_with(git, repo, &["ls-files", "--debug"]);
    assert!(
        debug.status.success(),
        "stock `git ls-files --debug` must read the index {what} left: {}",
        String::from_utf8_lossy(&debug.stderr)
    );

    let verify = Command::new(git)
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .env("GIT_TEST_CHECK_CACHE_TREE", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "stock git's own cache-tree verifier rejected the index {what} left: {}",
        String::from_utf8_lossy(&verify.stderr)
    );

    let fsck = run_with(git, repo, &["fsck", "--full"]);
    assert!(
        fsck.status.success(),
        "`git fsck --full` must pass after {what}: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// For every index-writing verb: run it with this binary and with stock git over
/// identical fixtures, and require stock's `write-tree` to produce the same tree
/// from both indexes — plus stock's cache-tree verifier to accept ours.
///
/// This is the assertion that catches a stale cache-tree, because a stale node is
/// invisible to everything else: the index parses, `status` looks normal, and only
/// a tree build that *trusts* the node produces the wrong id.
#[test]
fn stock_git_builds_the_same_tree_from_a_zvcs_written_index() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping cache-tree interop test");
        return;
    };

    // (tag, argv) — one case per verb that writes the index and can leave a
    // cache-tree behind.
    let cases: &[(&str, &[&str])] = &[
        ("add", &["add", "-A"]),
        ("commit", &["commit", "-aqm", "two"]),
        ("update-index", &["update-index", "sub/a.txt"]),
        ("write-tree", &["write-tree"]),
        ("rm-cached", &["rm", "-q", "--cached", "sub/deeper/d.txt"]),
        ("read-tree", &["read-tree", "HEAD"]),
        ("read-tree-reset", &["read-tree", "--reset", "HEAD"]),
        ("read-tree-empty", &["read-tree", "--empty"]),
    ];

    for (tag, argv) in cases {
        let ours = fixture(&git, &format!("interop-{tag}-zvcs"));
        let theirs = fixture(&git, &format!("interop-{tag}-stock"));
        for repo in [&ours, &theirs] {
            std::fs::write(repo.join("sub/a.txt"), b"changed\n").unwrap();
            std::fs::write(repo.join("newfile.txt"), b"new\n").unwrap();
        }
        assert!(run(&ours, argv).status.success(), "zvcs `{argv:?}` failed");
        assert!(
            run_with(&git, &theirs, argv).status.success(),
            "stock `{argv:?}` failed"
        );

        stock_audits(&git, &ours, tag);

        // `write-tree` mutates the index it reads, so measure on copies.
        let ours_tree = ok_with(&git, &ours, &["write-tree"]);
        let theirs_tree = ok_with(&git, &theirs, &["write-tree"]);
        assert_eq!(
            ours_tree, theirs_tree,
            "after `{argv:?}`, stock git must build the same tree from the zvcs index \
             as from its own"
        );

        let _ = std::fs::remove_dir_all(&ours);
        let _ = std::fs::remove_dir_all(&theirs);
    }
}

/// A cache-tree stock git wrote must survive an index write by this binary, node
/// for node — the round trip that matters on a machine where both binaries touch
/// the same repositories.
///
/// The two halves are separate failures. Losing the extension is a slowdown for
/// whoever reads the index next. *Keeping* it while the entries moved underneath
/// is corruption, which is why the second half re-checks the invalidation shape
/// rather than merely that something is still there.
#[test]
fn a_stock_written_cache_tree_survives_a_zvcs_index_write() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping cache-tree interop test");
        return;
    };

    let repo = fixture(&git, "roundtrip");
    let before = nodes(&repo);
    assert!(
        before.iter().all(|n| n.entries.is_some()),
        "stock git's own commit must leave a fully valid cache-tree: {before:#?}"
    );

    // A zvcs write that changes nothing about a directory must leave that
    // directory's node exactly as stock wrote it.
    std::fs::write(repo.join("other/o.txt"), b"o2\n").unwrap();
    ok(&repo, &["add", "other/o.txt"]);
    let after = nodes(&repo);
    assert_eq!(
        node(&after, "sub"),
        node(&before, "sub"),
        "a directory the zvcs write never touched must come through byte-identical"
    );
    assert_eq!(
        node(&after, "deeper"),
        node(&before, "deeper"),
        "and so must the nested one below it"
    );
    assert_eq!(node(&after, "").entries, None, "the root is on the changed path");
    assert_eq!(node(&after, "other").entries, None, "`other` holds the changed entry");

    stock_audits(&git, &repo, "a zvcs add over a stock cache-tree");

    // And the tree stock derives from it is still the right one.
    let expected = ok_with(&git, &repo, &["rev-parse", "HEAD^{tree}"]);
    let rebuilt = ok_with(&git, &repo, &["write-tree"]);
    assert_ne!(rebuilt, expected, "the staged change must produce a different tree");
    assert_eq!(
        ok_with(&git, &repo, &["cat-file", "-p", &format!("{rebuilt}:other/o.txt")]),
        "o2",
        "the rebuilt tree must contain the content zvcs staged"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// Intent-to-add entries live in the index but not in any tree, and their presence
/// keeps every node above them invalid.
///
/// `update_one()` skips them and sets `to_invalidate`, "to force cache-tree users
/// to read elsewhere" (cache-tree.c:464-472). Getting this wrong in the other
/// direction — caching a root that covers an `-N` entry — would let a later
/// `write-tree` emit a tree containing an empty blob nobody staged.
#[test]
fn intent_to_add_entries_stay_out_of_the_tree_and_poison_the_root() {
    let repo = fixture(BIN, "ita");
    std::fs::write(repo.join("sub/pending.txt"), b"pending\n").unwrap();
    ok(&repo, &["add", "-N", "sub/pending.txt"]);

    let built = ok(&repo, &["write-tree"]);
    assert_eq!(
        built,
        ok(&repo, &["rev-parse", "HEAD^{tree}"]),
        "an intent-to-add entry must not change the tree the index builds"
    );

    let after = nodes(&repo);
    assert_eq!(
        node(&after, "").entries,
        None,
        "the root must stay invalid while an intent-to-add entry is in the index"
    );
    assert_eq!(node(&after, "sub").entries, None, "and so must the directory holding it");
    assert!(
        node(&after, "other").entries.is_some(),
        "a directory with no intent-to-add entry below it is unaffected"
    );

    if let Some(git) = stock_git() {
        assert_eq!(
            ok_with(&git, &repo, &["write-tree"]),
            built,
            "stock git must build the same tree from the same index"
        );
        stock_audits(&git, &repo, "an intent-to-add add");
    }

    let _ = std::fs::remove_dir_all(&repo);
}
