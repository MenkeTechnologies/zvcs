//! `git fsck <object>…` under two packs whose `.pack` files share one mtime: `sort_pack()`
//! ties them (odb/source-packed.c:805-830), so the odb reads `repo_get_oid()` makes while
//! resolving each argument decide the pack order the listing walks, and its lookups decide
//! where colliding ids land in `obj_hash`. These are the argument shapes whose reads go
//! beyond a ref and its parents:
//!
//! * a commit-graph answering `repo_parse_commit()` (commit.c:625-640), which reads nothing
//!   and creates the parents but not the tree (commit-graph.c:925-995, :1091-1120);
//! * `:/<text>`, which parses every ref's object and HEAD's before the date-ordered search
//!   (object-name.c:1164-1236, :1763-1775);
//! * reflog selectors, `@{-<n>}` and `@{u}`, whose `get_oid_basic()` reads nothing and dies
//!   on a log too short or an upstream that is not there (object-name.c:667-825);
//! * abbreviated ids and describe names, whose `get_short_oid()` lists candidates without
//!   reading them (object-name.c:501-610, :1056-1081);
//! * `./` and `../` paths from a subdirectory (object-name.c:1700-1712).
//!
//! Stock git builds the fixtures and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-argreads-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped());
    c
}

/// Run stock git for fixture setup, feeding `input`; it must succeed. Returns stdout.
fn setup_in(stock: &str, dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = cmd(stock, dir, args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    setup_in(stock, dir, args, "")
}

/// `(code, stdout, stderr)` of one run.
fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Both binaries agree on exit code, stdout and stderr; stock's triple is returned.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} (left zvcs, right stock)");
    theirs
}

/// 80 commits over five directories `d0`..`d4`, a repack halfway, 40 each of dangling blobs,
/// trees, commits and tags, then a second pack and a tag `t` of `HEAD~7`, the two `.pack`
/// files given one mtime. A branch `side` at `HEAD~5` was checked out and left, so `@{-1}`
/// names it.
fn packs_of_one_mtime(stock: &str, root: &Path) -> PathBuf {
    let repo = root.join("repo");
    setup(stock, root, &["init", "-q", "-b", "main", "repo"]);
    setup(stock, &repo, &["config", "core.commitGraph", "false"]);
    // No automatic maintenance: it would write a multi-pack-index behind the fixture's back.
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for i in 0..80 {
        let dir = repo.join(format!("d{}", i % 5));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{i}")), format!("f {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        if i == 40 {
            setup(stock, &repo, &["repack", "-q", "-d"]);
        }
    }
    for j in 0..40 {
        setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("dangle {j}\n"));
        let blob = setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("tb {j}\n"));
        let tree = setup_in(stock, &repo, &["mktree"], &format!("100644 blob {blob}\tx{j}\n"));
        setup_in(stock, &repo, &["commit-tree", &tree], &format!("dc {j}\n"));
        let target = setup(stock, &repo, &["rev-parse", &format!("HEAD~{j}")]);
        let tag = format!("object {target}\ntype commit\ntag d{j}\ntagger C O Mitter <committer@example.com> 1112911993 -0700\n\nd{j}\n");
        setup_in(stock, &repo, &["hash-object", "-t", "tag", "-w", "--stdin"], &tag);
    }
    setup(stock, &repo, &["repack", "-q", "-a"]);
    let target = setup(stock, &repo, &["rev-parse", "HEAD~7"]);
    let tag = setup_in(
        stock,
        &repo,
        &["hash-object", "-t", "tag", "-w", "--stdin"],
        &format!("object {target}\ntype commit\ntag t\ntagger C O Mitter <committer@example.com> 1112911993 -0700\n\nt\n"),
    );
    setup(stock, &repo, &["update-ref", "refs/tags/t", &tag]);
    setup(stock, &repo, &["branch", "side", "HEAD~5"]);
    setup(stock, &repo, &["checkout", "-q", "side"]);
    setup(stock, &repo, &["checkout", "-q", "main"]);
    // The owner may set a file's times through a read-only descriptor (utimensat(2)),
    // and a pack is read-only.
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let mut packs = 0;
    for entry in std::fs::read_dir(repo.join(".git/objects/pack")).unwrap().flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "pack") {
            std::fs::File::open(entry.path()).unwrap().set_modified(when).unwrap();
            packs += 1;
        }
    }
    assert_eq!(packs, 2);
    repo
}

const FORMS: [&[&str]; 4] = [
    &["fsck", "--connectivity-only"],
    &["fsck", "--connectivity-only", "--unreachable"],
    &["fsck", "--connectivity-only", "--verbose"],
    &["fsck"],
];

/// Every form over each space-separated argument list, run from `dir`. A list in `failing`
/// exits 1 with the dangling report, one in `dying` exits 128 before it.
#[track_caller]
fn check(stock: &str, dir: &Path, resolving: &[&str], failing: &[&str], dying: &[&str]) {
    for revs in resolving.iter().chain(failing).chain(dying) {
        let revs: Vec<&str> = revs.split(' ').collect();
        for form in FORMS {
            let args: Vec<&str> = form.iter().copied().chain(revs.iter().copied()).collect();
            let (code, out, _) = same(stock, dir, &args);
            let joined = revs.join(" ");
            let want = match () {
                _ if dying.contains(&joined.as_str()) => 128,
                _ if failing.contains(&joined.as_str()) => 1,
                _ => 0,
            };
            assert_eq!(code, want, "{args:?}");
            if want != 128 {
                assert!(out.contains("dangling") || out.contains("unreachable"), "{args:?}: {out}");
            }
        }
    }
}

/// Under a commit-graph, `repo_parse_commit()` fills each ancestor in from the graph: no read,
/// the parents created, the tree only when `repo_get_commit_tree()` first asks — a `^{tree}`
/// peel here, else the walk that reaches the commit.
#[test]
fn argument_reads_under_the_commit_graph() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("graph");
    let repo = packs_of_one_mtime(stock, &root);
    setup(stock, &repo, &["config", "core.commitGraph", "true"]);
    setup(stock, &repo, &["commit-graph", "write", "--reachable"]);
    check(
        stock,
        &repo,
        &["HEAD~3", "HEAD~1:d0", "HEAD^", "t~2", "HEAD^{/c7}", "HEAD~2^{tree}", "HEAD~3 HEAD~1", ":/c50"],
        &["HEAD^2", "HEAD~100", "HEAD^{tree}~1"],
        &[],
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `:/<text>` parses every ref's object, a tag down to what it tags, and HEAD's, then
/// searches by date; a ref whose object is missing is passed over without a word under the
/// default `GIT_REF_PARANOIA`.
#[test]
fn argument_reads_for_a_oneline_search() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("oneline");
    let repo = packs_of_one_mtime(stock, &root);
    setup(stock, &repo, &["tag", "-a", "-m", "ann", "at1", "HEAD~13"]);
    std::fs::write(repo.join(".git/refs/heads/zz"), "1234567890123456789012345678901234567890\n").unwrap();
    check(stock, &repo, &[":/c7", ":/c1[0-9]", ":/!-c79", "HEAD~3 :/c5"], &[":/nomatch", ":/!x"], &[]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Reflog selectors, `@{-<n>}` and `@{u}` read no object; a log too short and a missing
/// upstream die inside `repo_get_oid()`, ahead of everything, and a ref name that is also
/// a tag warns.
#[test]
fn argument_reads_for_reflog_selectors() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("reflog");
    let repo = packs_of_one_mtime(stock, &root);
    check(
        stock,
        &repo,
        &["@{1}", "main@{2}", "@{-1}", "@{-1}~2", "HEAD@{3}~2", "@{0}", "@{1}^{/c2}"],
        &[],
        &["main@{500}", "@{u}", "HEAD^{blob} main@{500}"],
    );
    setup(stock, &repo, &["tag", "main", "HEAD~11"]);
    let (_, _, err) = same(stock, &repo, &["fsck", "--connectivity-only", "main"]);
    assert!(err.starts_with("warning: refname 'main' is ambiguous.\n"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// An abbreviated id or describe name one object answers reads nothing to resolve; one that
/// names no object is listed again after `odb_reprepare()` and fails.
#[test]
fn argument_reads_for_abbreviated_ids() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("abbrev");
    let repo = packs_of_one_mtime(stock, &root);
    let hex = |rev: &str, len: usize| setup(stock, &repo, &["rev-parse", rev])[..len].to_owned();
    let ids = setup(stock, &repo, &["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]);
    let absent = (0..0x10000u32)
        .map(|n| format!("{n:04x}f"))
        .find(|prefix| !ids.lines().any(|id| id.starts_with(prefix.as_str())))
        .unwrap();
    let (a, b, tree) = (hex("HEAD~4", 7), hex("HEAD~6", 6), hex("HEAD^{tree}", 8));
    let resolving = [
        a.clone(),
        format!("{b}~2"),
        tree,
        format!("x-3-g{a}"),
        format!("v1-0-g{b}^{{tree}}"),
        format!("{a}:d0"),
    ];
    let failing = [absent.clone(), format!("x-1-g{absent}")];
    let resolving: Vec<&str> = resolving.iter().map(String::as_str).collect();
    let failing: Vec<&str> = failing.iter().map(String::as_str).collect();
    check(stock, &repo, &resolving, &failing, &[]);
    let _ = std::fs::remove_dir_all(&root);
}

/// `./` and `../` paths are rewritten against the subdirectory, and one that climbs out of
/// the work tree dies.
#[test]
fn argument_reads_for_relative_paths() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("relative");
    let repo = packs_of_one_mtime(stock, &root);
    check(
        stock,
        &repo.join("d0"),
        &["HEAD:./f0", ":./f5", ":../d1/f1", "HEAD~2:./", ":0:./f10", "HEAD~1:../d2"],
        &["HEAD:./nosuch"],
        &[":../../x", "HEAD:../../x"],
    );
    let _ = std::fs::remove_dir_all(&root);
}
