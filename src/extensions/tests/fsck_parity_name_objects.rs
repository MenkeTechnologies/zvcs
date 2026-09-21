//! `git fsck --name-objects`: the path each reported object id is decorated
//! with, and the rules that decide which of several candidate paths it keeps.
//!
//! Every expectation is a byte captured from stock git 2.55.0 on the same
//! fixture. Four claims:
//!
//!   1. **Names spread outwards from the head that carries one.** A reference
//!      names its own tip (`fsck_handle_ref`, builtin/fsck.c:590-591); a commit
//!      names its tree `<name>:` and its first parent `<name>^`
//!      (`fsck_walk_commit`, fsck.c:415-459); a tree names an entry
//!      `<name><path>`, with a trailing `/` for a subtree (`fsck_walk_tree`,
//!      :375-388); a tag names its target with its own name (:471-479).
//!   2. **A chain of first parents collapses to `~<n>`.** Once a name ends in
//!      `^`, the next hop is `<prefix>~2`, then `~3`, … — `fsck_walk_commit`
//!      re-reads the suffix it wrote (fsck.c:424-459) instead of stacking carets.
//!   3. **First write wins** (`fsck_put_object_name`'s `if (!hashret) return;`,
//!      fsck.c:325-327), and reflogs are put before the walk runs, so in a
//!      repository with reflogs a commit is named `<ref>@{<timestamp>}` rather
//!      than by the path the traversal would have reached it by.
//!   4. **The index and the cache tree name objects too**: `:<path>` for an
//!      entry (builtin/fsck.c:900-903) and a bare `:` for a cache-tree node
//!      (:830) — which is how a `missing blob` line for a staged file gets a
//!      readable path even with every reference gone.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run binary under test")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fsck(dir: &Path, home: &Path, extra: &[&str]) -> (String, String, i32) {
    let mut args = vec!["fsck", "--no-progress", "--name-objects"];
    args.extend_from_slice(extra);
    let out = run(dir, home, &args);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn loose_path(repo: &Path, oid: &str) -> PathBuf {
    repo.join(".git/objects").join(&oid[..2]).join(&oid[2..])
}

/// A linear history of `depth` commits touching `a/b/f.txt` and `top.txt`, plus
/// an annotated tag on the tip. Reflogs are removed unless `keep_reflogs`, so
/// the naming under test is the walk's rather than `<ref>@{<t>}`.
fn fixture(tag: &str, depth: usize, keep_reflogs: bool) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-name-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(repo.join("a/b")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    for i in 0..depth {
        std::fs::write(repo.join("a/b/f.txt"), format!("v{i}\n")).unwrap();
        std::fs::write(repo.join("top.txt"), format!("t{i}\n")).unwrap();
        ok(&repo, &home, &["add", "-A"]);
        ok(&repo, &home, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    ok(&repo, &home, &["tag", "-a", "v1", "-m", "tagged"]);
    if !keep_reflogs {
        let _ = std::fs::remove_dir_all(repo.join(".git/logs"));
    }
    (repo, home)
}

/// Claims 1 and 2: the tree path, the `^` hop and the `~<n>` collapse.
#[test]
fn names_follow_the_path_the_object_was_reached_by() {
    let (repo, home) = fixture("walk", 5, false);
    // The index is a head source too and runs before the walk, so it wins the
    // race for the blobs and the cache-tree nodes; take it away so that what is
    // left to name `a/b/f.txt` is the commit -> tree -> subtree chain.
    std::fs::remove_file(repo.join(".git/index")).unwrap();

    let blob = ok(&repo, &home, &["rev-parse", "HEAD:a/b/f.txt"]).trim().to_owned();
    let sub = ok(&repo, &home, &["rev-parse", "HEAD:a/b"]).trim().to_owned();
    std::fs::remove_file(loose_path(&repo, &blob)).unwrap();
    let (out, _, code) = fsck(&repo, &home, &[]);
    assert!(
        out.contains(&format!("missing blob {blob} (refs/heads/main:a/b/f.txt)\n")),
        "a commit names its tree `<name>:` and a tree names an entry \
         `<name><path>`; got:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "broken link from    tree {sub} (refs/heads/main:a/b/)\n"
        )),
        "and a subtree entry carries the implicit trailing slash; got:\n{out}"
    );
    assert_eq!(code & 2, 2);

    // Claim 2: `^`, then `~2`, `~3`, `~4` rather than a stack of carets.
    for (rev, expected) in [
        ("HEAD^", "refs/heads/main^"),
        ("HEAD~2", "refs/heads/main~2"),
        ("HEAD~3", "refs/heads/main~3"),
        ("HEAD~4", "refs/heads/main~4"),
    ] {
        let oid = ok(&repo, &home, &["rev-parse", rev]).trim().to_owned();
        let saved = repo.join(".keep");
        std::fs::copy(loose_path(&repo, &oid), &saved).unwrap();
        std::fs::remove_file(loose_path(&repo, &oid)).unwrap();
        let (out, _, code) = fsck(&repo, &home, &[]);
        std::fs::rename(&saved, loose_path(&repo, &oid)).unwrap();
        assert!(
            out.contains(&format!("missing commit {oid} ({expected})\n")),
            "{rev} should be named {expected}; got:\n{out}"
        );
        assert_eq!(code & 2, 2);
    }
}

/// Claim 1, the tag half: `fsck_walk_tag()` hands the tagged object the tag's
/// own name unchanged, so both ends of the broken link read the same.
#[test]
fn a_tag_lends_its_own_name_to_what_it_points_at() {
    let (repo, home) = fixture("tag", 3, false);
    let tag = ok(&repo, &home, &["rev-parse", "refs/tags/v1"]).trim().to_owned();
    let tip = ok(&repo, &home, &["rev-parse", "refs/tags/v1^{}"]).trim().to_owned();
    // Leave the tag as the only reference, so nothing else can name the tip.
    std::fs::remove_file(repo.join(".git/refs/heads/main")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/gone\n").unwrap();
    std::fs::remove_file(repo.join(".git/index")).unwrap();
    std::fs::remove_file(loose_path(&repo, &tip)).unwrap();

    let (out, _, code) = fsck(&repo, &home, &[]);
    assert!(
        out.contains(&format!(
            "broken link from     tag {tag} (refs/tags/v1)\n\
             \x20             to  commit {tip} (refs/tags/v1)\n"
        )),
        "the tagged object inherits the tag's name verbatim; got:\n{out}"
    );
    assert!(out.contains(&format!("missing commit {tip} (refs/tags/v1)\n")));
    assert_eq!(code & 2, 2);
}

/// Claim 3: whichever head source runs first owns the name.
#[test]
fn a_reflog_entry_names_an_object_before_the_walk_can() {
    let (repo, home) = fixture("reflog", 3, true);
    // No tag on this fixture's path to the parent, so the only two candidates
    // are the reflog entry and the walk.
    std::fs::remove_file(repo.join(".git/refs/tags/v1")).unwrap();
    let parent = ok(&repo, &home, &["rev-parse", "HEAD^"]).trim().to_owned();

    let (_, trace, _) = fsck(&repo, &home, &["-v"]);
    let line = trace
        .lines()
        .find(|l| l.starts_with(&format!("Checking {parent} ")))
        .unwrap_or_else(|| panic!("no connectivity trace for {parent} in:\n{trace}"));
    assert!(
        line.contains("@{"),
        "process_refs() walks the reflogs before traverse_reachable() runs, and \
         fsck_put_object_name() keeps the first name; got: {line}"
    );

    let (_, trace, _) = fsck(&repo, &home, &["-v", "--no-reflogs"]);
    assert!(
        trace.contains(&format!("Checking {parent} (refs/heads/main^)\n")),
        "--no-reflogs takes that head source away and the walk's name is what \
         is left; got:\n{trace}"
    );
}

/// Claim 4.
#[test]
fn the_index_and_cache_tree_name_what_they_hold() {
    let (repo, home) = fixture("index", 3, false);
    // `git status` fills the cache tree; then take every reference away so the
    // index is the only head source left.
    ok(&repo, &home, &["status", "--porcelain"]);
    let blob = ok(&repo, &home, &["rev-parse", ":top.txt"]).trim().to_owned();
    let nested = ok(&repo, &home, &["rev-parse", ":a/b/f.txt"]).trim().to_owned();
    let a_tree = ok(&repo, &home, &["rev-parse", "HEAD:a"]).trim().to_owned();
    let b_tree = ok(&repo, &home, &["rev-parse", "HEAD:a/b"]).trim().to_owned();
    std::fs::remove_file(repo.join(".git/refs/heads/main")).unwrap();
    std::fs::remove_file(repo.join(".git/refs/tags/v1")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/gone\n").unwrap();

    std::fs::remove_file(loose_path(&repo, &blob)).unwrap();
    std::fs::remove_file(loose_path(&repo, &nested)).unwrap();
    let (out, stderr, code) = fsck(&repo, &home, &[]);
    assert!(
        out.contains(&format!("missing blob {blob} (:top.txt)\n")),
        "fsck_index() names an entry of the current worktree `:<path>`; got:\n{out}"
    );
    assert!(
        out.contains(&format!("missing blob {nested} (:a/b/f.txt)\n")),
        "including one under a directory; got:\n{out}"
    );
    assert!(stderr.contains("notice: No default references\n"));
    assert_eq!(code & 2, 2);

    // Every cache-tree node is named with a bare colon — the root and `a` alike,
    // because each `fsck_put_object_name(..., ":")` is a fresh id — and the walk
    // extends that into `:b/` for the subtree `a` names.
    std::fs::remove_file(loose_path(&repo, &b_tree)).unwrap();
    let (out, _, _) = fsck(&repo, &home, &[]);
    assert!(
        out.contains(&format!(
            "broken link from    tree {a_tree} (:)\n\
             \x20             to    tree {b_tree} (:b/)\n"
        )),
        "got:\n{out}"
    );
    assert!(out.contains(&format!("missing tree {b_tree} (:b/)\n")));
}
