//! `git fsck --connectivity-only`'s report comes out in `obj_hash` slot order, and the slot each
//! object lands in depends on the whole `lookup_object()` / `create_object()` sequence the
//! command runs (object.c): insertion order, linear probing, `lookup_object()`'s move of a hit
//! back to the slot its probe started at, and `grow_object_hash()`'s re-insertion in slot order.
//!
//! Under `--connectivity-only` that sequence is: the snapshot's `parse_object()` of every head;
//! `mark_object_for_connectivity()`'s `lookup_unknown_object()` over `odb_for_each_object()`'s
//! listing, which walks each source's loose objects and then its packs in list order, each in
//! index order, once per copy (builtin/fsck.c:795-803, odb/source-files.c:121-140); the reflog,
//! index and cache-tree lookups; `traverse_reachable()`, which parses every non-head commit and
//! tag it meets; and `mark_unreachable_referents()`, which looks every listed id up again before
//! walking the unreachable ones (builtin/fsck.c:210-240). Dangling, unreachable and `--verbose`
//! `Checking` lines all follow from it.
//!
//! The fixtures are large enough for the table to grow several times, and hold dangling blobs,
//! trees, commits and tags, loose-and-packed copies, and two packs. Stock git builds them and
//! is the oracle (`support/stock_git.rs`); the tests skip when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-conn-{label}-{}", std::process::id()));
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

/// Every file under `.git/lost-found`, with its content, in path order.
fn lost_found(repo: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for sub in ["commit", "other"] {
        let dir = repo.join(".git/lost-found").join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            out.push((format!("{sub}/{}", entry.file_name().to_string_lossy()), std::fs::read(entry.path()).unwrap()));
        }
    }
    out.sort();
    out
}

/// `commits` commits over five directories with a repack partway, then `dangling` each of
/// unreferenced blobs, trees with a fresh blob, commits of those trees and annotated tags of
/// reachable commits, so every kind is reported and the table holds a few thousand slots.
fn history(stock: &str, root: &Path, commits: usize, dangling: usize) -> PathBuf {
    let repo = root.join("repo");
    setup(stock, root, &["init", "-q", "-b", "main", "repo"]);
    setup(stock, &repo, &["config", "core.commitGraph", "false"]);
    // No automatic maintenance: it would write a multi-pack-index behind the fixture's back.
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for i in 0..commits {
        let dir = repo.join(format!("d{}", i % 5));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{i}")), format!("f {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        if i == commits / 2 {
            setup(stock, &repo, &["repack", "-q", "-d"]);
        }
    }
    for j in 0..dangling {
        setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("dangle {j}\n"));
        let blob = setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("tb {j}\n"));
        let tree = setup_in(stock, &repo, &["mktree"], &format!("100644 blob {blob}\tx{j}\n"));
        setup_in(stock, &repo, &["commit-tree", &tree], &format!("dc {j}\n"));
        let target = setup(stock, &repo, &["rev-parse", &format!("HEAD~{}", j % commits)]);
        let tag = format!("object {target}\ntype commit\ntag d{j}\ntagger C O Mitter <committer@example.com> 1112911993 -0700\n\nd{j}\n");
        setup_in(stock, &repo, &["hash-object", "-t", "tag", "-w", "--stdin"], &tag);
    }
    repo
}

const FORMS: [&[&str]; 7] = [
    &["fsck", "--connectivity-only"],
    &["fsck", "--connectivity-only", "--unreachable"],
    &["fsck", "--connectivity-only", "--no-reflogs"],
    &["fsck", "--connectivity-only", "--name-objects"],
    &["fsck", "--connectivity-only", "--verbose"],
    &["fsck", "--connectivity-only", "HEAD"],
    &["fsck", "--connectivity-only", "HEAD^{tree}"],
];

/// Loose objects only, and then with a full pack beside them so every object is listed twice.
#[test]
fn dangling_order_over_loose_and_duplicated_objects() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("loose");
    let repo = history(stock, &root, 80, 40);
    for args in FORMS {
        let (code, out, _) = same(stock, &repo, args);
        assert_eq!(code, 0, "{args:?}");
        assert!(out.contains("dangling") || out.contains("unreachable"), "{args:?}: {out}");
    }
    // `repack -a` without `-d` keeps every loose copy.
    setup(stock, &repo, &["repack", "-q", "-a"]);
    for args in FORMS {
        same(stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Two packs and loose objects, a deleted branch whose history only the reflog keeps, and
/// `--lost-found`, whose files must be the ones stock writes.
#[test]
fn dangling_order_over_packs_reflogs_and_lost_found() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("packs");
    let repo = history(stock, &root, 120, 60);
    setup(stock, &repo, &["repack", "-q"]);
    setup(stock, &repo, &["checkout", "-q", "-b", "side", "HEAD~30"]);
    for i in 0..10 {
        std::fs::write(repo.join(format!("s{i}")), format!("s {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("s{i}")]);
    }
    setup(stock, &repo, &["checkout", "-q", "main"]);
    setup(stock, &repo, &["branch", "-D", "side"]);
    for args in FORMS {
        same(stock, &repo, args);
    }
    same(stock, &repo, &["fsck", "--connectivity-only", "--lost-found"]);
    let theirs = lost_found(&repo);
    std::fs::remove_dir_all(repo.join(".git/lost-found")).unwrap();
    run(ZVCS, &repo, &["fsck", "--connectivity-only", "--lost-found"]);
    assert_eq!(lost_found(&repo), theirs);
    assert!(!theirs.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// The history under a commit-graph: every commit no head parsed goes through the walk's graph
/// parse, which looks up the parents before the tree (commit-graph.c:858-874, 1091-1120), and a
/// tree argument leaves every commit to it.
#[test]
fn dangling_order_under_the_commit_graph() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("graph");
    let repo = history(stock, &root, 60, 20);
    setup(stock, &repo, &["config", "core.commitGraph", "true"]);
    setup(stock, &repo, &["commit-graph", "write", "--reachable"]);
    let tree = setup(stock, &repo, &["rev-parse", "HEAD^{tree}"]);
    for args in [
        &["fsck", "--connectivity-only"][..],
        &["fsck", "--connectivity-only", "--unreachable"],
        &["fsck", "--connectivity-only", &tree],
        &["-c", "core.commitGraph=false", "fsck", "--connectivity-only", &tree],
        &["fsck", "--connectivity-only", "--verbose", &tree],
    ] {
        let (code, out, _) = same(stock, &repo, args);
        assert_eq!(code, 0, "{args:?}");
        assert!(out.contains("dangling") || out.contains("unreachable"), "{args:?}: {out}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A `--filter=blob:none` clone: the first head's `mark_object()` runs `is_promisor_object()`,
/// whose `add_promisor_object()` parses every object of the promisor pack (packfile.c:2006-2059),
/// so the pack's commits and tags look their links up there and never again, and the history
/// behind the promisor tip is reported unreachable.
#[test]
fn dangling_order_in_a_partial_clone() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("promisor");
    let server = history(stock, &root, 60, 0);
    setup(stock, &server, &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", server.display());
    setup(stock, &root, &["clone", "-q", "--no-local", "--filter=blob:none", &url, "clone"]);
    let repo = root.join("clone");
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for j in 0..12 {
        setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("pd {j}\n"));
        let blob = setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("pq {j}\n"));
        let tree = setup_in(stock, &repo, &["mktree"], &format!("100644 blob {blob}\tq{j}\n"));
        setup_in(stock, &repo, &["commit-tree", &tree], &format!("pc {j}\n"));
    }
    for args in FORMS {
        same(stock, &repo, args);
    }
    let (_, out, _) = same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable"]);
    assert!(out.contains("unreachable commit"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}
