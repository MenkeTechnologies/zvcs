//! The lines `git fsck` prints about links it could not follow, and about the
//! places outside the object database that name an object — every one of them a
//! byte captured from stock git 2.55.0 on the same fixture.
//!
//! Five claims, each with its own fixture:
//!
//!   1. **A severed link is two stdout lines, not one.** `mark_object()` prints
//!      `broken link from <type> <oid>` / `              to <type> <oid>` with
//!      both type names right-justified in seven columns before
//!      `check_reachable_object()` adds the `missing` line
//!      (builtin/fsck.c:163-175, :265-275). The pair is printed once per missing
//!      object however many parents name it, because `mark_object()`'s
//!      `REACHABLE` guard (:151-153) runs first.
//!   2. **A branch that does not name a commit is `ERROR_REFS`.** `snapshot_ref()`
//!      reports `<ref>: not a commit` and sets bit 010, not bit 02
//!      (builtin/fsck.c:569-572), so `git fsck` exits 8 while still reporting
//!      everything else.
//!   3. **A cache-tree entry pointing at an absent tree is `ERROR_REFS` too**, and
//!      it takes that node's subtrees out of the walk: `fsck_cache_tree()`
//!      returns before its `it->down[i]` loop (builtin/fsck.c:821-836).
//!   4. **An index entry whose blob is gone is a `missing blob` line.**
//!      `fsck_index()` reaches it through `lookup_blob()` (:894-898), so the
//!      object exists in `obj_hash` with `OBJ_BLOB` and no `HAS_OBJ` — and
//!      `mark_object_reachable()` passes no parent, so there is no `broken link`
//!      pair to go with it.
//!   5. **A stray file in `.git/objects/??` is `bad sha1 file:` on stderr**, at
//!      the position the scan met it and with no effect on the exit code —
//!      `fsck_cruft()` is a bare `fprintf_ln()` (builtin/fsck.c:776-782), and a
//!      `tmp_obj_*` name is exempt.
//!
//! Every fixture is built with the binary under test, so no system `git` is
//! needed at run time and nothing binary is checked in.

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

/// stdout, stderr and the exit code of one `git fsck` run.
fn fsck(dir: &Path, home: &Path, extra: &[&str]) -> (String, String, i32) {
    let mut args = vec!["fsck", "--no-progress"];
    args.extend_from_slice(extra);
    let out = run(dir, home, &args);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Two commits, a subdirectory and an annotated tag, under a pinned identity and
/// clock — so every object id below is reproducible on any machine.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-links-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("f.txt"), "hello\n").unwrap();
    std::fs::create_dir_all(repo.join("d")).unwrap();
    std::fs::write(repo.join("d/g.txt"), "sub\n").unwrap();
    ok(&repo, &home, &["add", "-A"]);
    ok(&repo, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("f.txt"), "hello\ntwo\n").unwrap();
    ok(&repo, &home, &["commit", "-q", "-am", "two"]);
    (repo, home)
}

fn loose_path(repo: &Path, oid: &str) -> PathBuf {
    repo.join(".git/objects").join(&oid[..2]).join(&oid[2..])
}

/// Claim 1: the `broken link` pair, its seven-column type padding, and its one
/// line per missing object.
#[test]
fn a_severed_parent_link_prints_the_broken_link_pair_before_the_missing_line() {
    let (repo, home) = fixture("parent");
    let head = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_owned();
    let parent = ok(&repo, &home, &["rev-parse", "HEAD^"]).trim().to_owned();
    let dropped_tree = ok(&repo, &home, &["rev-parse", "HEAD^^{tree}"]).trim().to_owned();
    std::fs::remove_file(loose_path(&repo, &parent)).unwrap();

    let (stdout, _, code) = fsck(&repo, &home, &[]);
    // `commit` is six characters, so `%7s` pads it by one; the continuation is
    // fourteen spaces plus `to` and the same seven-column field.
    assert_eq!(
        stdout,
        format!(
            "broken link from  commit {head}\n\
             \x20             to  commit {parent}\n\
             dangling tree {dropped_tree}\n\
             missing commit {parent}\n"
        )
    );
    // ERROR_REACHABLE only — the object database itself is intact.
    assert_eq!(code, 2);
}

/// Claim 1, the padding of a shorter type name, and the guard that keeps the
/// pair to one copy when two commits name the same missing tree.
#[test]
fn a_missing_tree_pads_to_seven_and_is_reported_once_per_object() {
    let (repo, home) = fixture("tree");
    let tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    let head = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_owned();
    // A second commit on the same tree, so two parents name the one missing
    // object. `git commit --allow-empty` reuses HEAD's tree exactly.
    ok(&repo, &home, &["branch", "side"]);
    ok(&repo, &home, &["commit", "-q", "--allow-empty", "-m", "same-tree"]);
    let second = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim(), tree);
    std::fs::remove_file(loose_path(&repo, &tree)).unwrap();

    let (stdout, _, code) = fsck(&repo, &home, &[]);
    let pairs = stdout.matches("broken link from").count();
    assert_eq!(
        pairs, 1,
        "mark_object()'s REACHABLE guard runs before the HAS_OBJ test, so the \
         second parent to name {tree} says nothing; got:\n{stdout}"
    );
    // Whichever of the two commits the walk reached first owns the line; both
    // are `commit`, so only the id varies.
    let from_second = format!("broken link from  commit {second}\n              to    tree {tree}\n");
    let from_head = format!("broken link from  commit {head}\n              to    tree {tree}\n");
    assert!(
        stdout.starts_with(&from_second) || stdout.starts_with(&from_head),
        "expected a `tree` right-justified in seven columns; got:\n{stdout}"
    );
    assert!(stdout.contains(&format!("missing tree {tree}\n")));
    assert_eq!(code, 10, "ERROR_REACHABLE | ERROR_REFS: the cache-tree names it too");
}

/// Claim 2: `refs/heads/<name>` that does not resolve to a commit.
#[test]
fn a_branch_pointing_at_a_tree_is_error_refs() {
    let (repo, home) = fixture("branch");
    let tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    std::fs::write(repo.join(".git/refs/heads/invalid"), format!("{tree}\n")).unwrap();

    let (stdout, stderr, code) = fsck(&repo, &home, &[]);
    assert_eq!(stderr, "error: refs/heads/invalid: not a commit\n");
    assert_eq!(stdout, "", "the tree is reachable, so nothing is reported about it");
    assert_eq!(code, 8, "ERROR_REFS, not ERROR_REACHABLE");
}

/// Claim 2 again, for the other half of `is_branch()` (refs.c:1072-1075): a
/// detached `HEAD` is judged the same way, and a tag reference is not.
#[test]
fn only_head_and_refs_heads_are_judged_as_branches() {
    let (repo, home) = fixture("isbranch");
    let tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    std::fs::write(repo.join(".git/refs/tags/treeish"), format!("{tree}\n")).unwrap();
    assert_eq!(
        fsck(&repo, &home, &[]).1,
        "",
        "refs/tags/ is not a branch, so a non-commit there is fine"
    );

    std::fs::write(repo.join(".git/HEAD"), format!("{tree}\n")).unwrap();
    let (_, stderr, code) = fsck(&repo, &home, &[]);
    assert!(
        stderr.contains("error: HEAD: not a commit\n"),
        "is_branch() is `HEAD` or refs/heads/*; got:\n{stderr}"
    );
    assert_eq!(code & 8, 8);
}

/// Claim 3: a cache-tree id the object database cannot produce.
#[test]
fn a_cache_tree_id_with_no_object_is_reported_and_stops_that_subtree() {
    let (repo, home) = fixture("cachetree");
    // `git status` leaves a fully valid cache-tree behind, with one node per
    // directory: the root and `d/`.
    ok(&repo, &home, &["status", "--porcelain"]);
    let root_tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    let sub_tree = ok(&repo, &home, &["rev-parse", "HEAD:d"]).trim().to_owned();
    // Repack so the blobs and the subtree survive the removal of the loose root
    // tree, then drop the root tree from the pack's reach by removing it from
    // both halves of the odb.
    std::fs::remove_file(loose_path(&repo, &root_tree)).unwrap();

    let (_, stderr, code) = fsck(&repo, &home, &[]);
    assert!(
        stderr.contains(&format!(
            "error: {root_tree}: invalid sha1 pointer in cache-tree of .git/index\n"
        )),
        "expected fsck_cache_tree()'s line; got:\n{stderr}"
    );
    assert!(
        !stderr.contains(&sub_tree),
        "the `return 1` at builtin/fsck.c:827 precedes the subtree loop, so `d/`'s \
         node is never visited; got:\n{stderr}"
    );
    assert_eq!(code & 8, 8, "ERROR_REFS");
}

/// Claim 4: an index entry whose blob is gone.
#[test]
fn an_index_entry_with_no_blob_is_a_missing_blob_line_with_no_broken_link() {
    let (repo, home) = fixture("indexblob");
    let blob = ok(&repo, &home, &["rev-parse", ":f.txt"]).trim().to_owned();
    // Detach every reference so the index is the only thing naming the blob;
    // otherwise the tree that names it would print a `broken link` pair too and
    // the claim about `mark_object_reachable()`'s null parent would not be
    // isolated.
    std::fs::remove_dir_all(repo.join(".git/refs/heads")).unwrap();
    std::fs::create_dir_all(repo.join(".git/refs/heads")).unwrap();
    std::fs::remove_dir_all(repo.join(".git/logs")).unwrap();
    std::fs::remove_file(loose_path(&repo, &blob)).unwrap();

    let (stdout, stderr, code) = fsck(&repo, &home, &[]);
    assert!(
        stdout.contains(&format!("missing blob {blob}\n")),
        "expected the type lookup_blob() gave it; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("broken link"),
        "mark_object_reachable() passes no parent, so builtin/fsck.c:164 is false; got:\n{stdout}"
    );
    assert!(stderr.contains("notice: No default references\n"));
    assert_eq!(code & 2, 2, "ERROR_REACHABLE");
}

/// Claim 5: `fsck_cruft()`.
#[test]
fn a_stray_file_in_an_object_subdirectory_is_bad_sha1_file() {
    let (repo, home) = fixture("cruft");
    // `17` sorts before every fanout this fixture uses, and the name is not
    // `hexsz - 2` hex digits, so `for_each_file_in_obj_subdir()` hands it to the
    // cruft callback rather than to `fsck_loose()`.
    let dir = repo.join(".git/objects/17");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notahash"), "junk\n").unwrap();
    // A `tmp_obj_*` name in the same directory is skipped outright.
    std::fs::write(dir.join("tmp_obj_abcdef"), "junk\n").unwrap();

    let (stdout, stderr, code) = fsck(&repo, &home, &[]);
    assert_eq!(stderr, "bad sha1 file: .git/objects/17/notahash\n");
    assert_eq!(stdout, "");
    assert_eq!(
        code, 0,
        "fsck_cruft() is fprintf_ln(), not error(): errors_found is untouched"
    );
}
