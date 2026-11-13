//! `git fsck` on loose objects, refs and worktrees that a differential sweep
//! against stock git found reported differently. Each fixture is built twice
//! by stock git with pinned dates, so both copies hold the same ids, and the
//! command is then run by stock git in one copy and by zvcs in the other.
//!
//!   1. **`report()` decides whether a check goes on.** `fsck_commit()`,
//!      `fsck_ident()` and `fsck_tag_standalone()` stop at a finding only when
//!      `report()` returns non-zero, and it returns 0 for an ignored message and
//!      for a warning (fsck.c:241-242, builtin/fsck.c:93-98). So an ignored
//!      `badDateOverflow` author leaves the committer line to be checked, and
//!      `fsck_object()` returns the *last* `report()` of a tag: a bad tagger
//!      followed by an extra header returns the ignored `extraHeaderEntry`, and
//!      the error is printed without setting the exit code (fsck.c:1088-1126).
//!   2. **A loose ref that will not parse is a snapshot head with the null id**
//!      (`REFS_FOR_EACH_INCLUDE_BROKEN`, builtin/fsck.c:581-583), so fsck reports
//!      `invalid sha1 pointer 0000…` for it and goes on, where zvcs died.
//!   3. **A linked worktree's HEAD is snapshotted with the refs** as
//!      `worktrees/<id>/HEAD` (builtin/fsck.c:616-641), so one naming a missing
//!      object is `invalid sha1 pointer <oid>` and `ERROR_REACHABLE`.
//!   4. **An empty loose object's `error_errno()` line ends in `ENOENT`'s text**
//!      (object-file.c:163-170, :1207-1213).
//!   5. **`--connectivity-only` dies on a loose object of no known type**
//!      (odb/source-loose.c:166-172) when `mark_unreachable_referents()` asks
//!      for its type.
//!   6. **`--progress` draws around the scan's findings**: a subdirectory's lines
//!      come out before `fsck_subdir()` draws it (odb/source-loose.c:1101-1106).
//!   7. **A commit or tag that will not parse says why under
//!      `--connectivity-only` too**, from `mark_unreachable_referents()`'s walk
//!      (fsck.c:471-476, tag.c:168-179), and a linked worktree's cache tree is
//!      checked under that worktree's index path (builtin/fsck.c:790-791).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-sweep-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

/// `(exit code, stdout, stderr)` of `bin args` in `dir`, isolated from the
/// caller's configuration, with `dir` replaced by `<dir>` in both streams.
fn run(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("TERM", "dumb")
        .env("LC_ALL", "C");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let root = dir.to_string_lossy().into_owned();
    let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<dir>");
    (out.status.code().unwrap_or(-1), clean(&out.stdout), clean(&out.stderr))
}

/// Setup step run by stock git; it must succeed.
fn git(stock: &str, dir: &Path, args: &[&str]) -> String {
    let mut full = vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"];
    full.extend_from_slice(args);
    let (code, out, err) = run(stock, dir, &full, &[]);
    assert_eq!(code, 0, "{args:?}: {err}");
    out.trim_end().to_owned()
}

/// A zlib stream holding `data` in one stored deflate block.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let len = u16::try_from(data.len()).expect("small object");
    let mut out = vec![0x78, 0x01, 0x01];
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// Write `<kind> <len>\0<body>` as the loose object `id`, unchecked.
fn write_loose(repo: &Path, id: &str, kind: &str, body: &[u8]) {
    let mut raw = format!("{kind} {}\0", body.len()).into_bytes();
    raw.extend_from_slice(body);
    let dir = repo.join(".git/objects").join(&id[..2]);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&id[2..]), zlib_stored(&raw)).unwrap();
}

/// An object `hash-object --literally` stores without checking it.
fn literal(stock: &str, repo: &Path, kind: &str, body: &str) -> String {
    let file = repo.join("literal.tmp");
    std::fs::write(&file, body).unwrap();
    let id = git(stock, repo, &["hash-object", "-t", kind, "--literally", "-w", "literal.tmp"]);
    std::fs::remove_file(file).unwrap();
    id
}

/// Build the same repository with stock git in `<label>/stock` and
/// `<label>/zvcs`: two commits on `main`, then `extra` run in each.
fn twins(stock: &str, label: &str, extra: impl Fn(&Path)) -> (PathBuf, PathBuf) {
    let root = scratch(label);
    let pair: Vec<PathBuf> = ["stock", "zvcs"]
        .iter()
        .map(|side| {
            let dir = root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            git(stock, &dir, &["init", "-q", "-b", "main"]);
            std::fs::write(dir.join("a"), "one\n").unwrap();
            git(stock, &dir, &["add", "a"]);
            git(stock, &dir, &["commit", "-q", "-m", "one"]);
            std::fs::write(dir.join("a"), "two\n").unwrap();
            git(stock, &dir, &["commit", "-q", "-am", "two"]);
            extra(&dir);
            dir
        })
        .collect();
    (pair[0].clone(), pair[1].clone())
}

#[track_caller]
fn same(stock: &str, dirs: &(PathBuf, PathBuf), args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let theirs = run(stock, &dirs.0, args, env);
    let ours = run(ZVCS, &dirs.1, args, env);
    assert_eq!(ours, theirs, "git {args:?}: left is zvcs, right is stock");
    theirs
}

#[test]
fn a_finding_report_answers_zero_for_does_not_end_the_check() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dirs = twins(stock, "report", |dir| {
        let tree = git(stock, dir, &["rev-parse", "HEAD^{tree}"]);
        let head = git(stock, dir, &["rev-parse", "HEAD"]);
        let commit = literal(
            stock,
            dir,
            "commit",
            &format!(
                "tree {tree}\nauthor A <a@e> 99999999999999999999 +0000\ncommitter <c@e> 1700000000 +0000\n\nx\n"
            ),
        );
        git(stock, dir, &["update-ref", "refs/heads/overflow", &commit]);
        let tag = literal(
            stock,
            dir,
            "tag",
            &format!("object {head}\ntype commit\ntag t\ntagger T <t@e 1700000000 +0000\nextra x\n\nm\n"),
        );
        git(stock, dir, &["update-ref", "refs/tags/t", &tag]);
        let tag = literal(
            stock,
            dir,
            "tag",
            &format!("object {head}\ntype commit\ntag bad name\n\nm\n"),
        );
        git(stock, dir, &["update-ref", "refs/tags/n", &tag]);
        literal(
            stock,
            dir,
            "tag",
            &format!("object {head}\ntype bogus\ntag bt\ntagger T <t@e> 1700000000 +0000\n\nm\n"),
        );
    });
    // The ignored overflow lets `missingNameBeforeEmail` through; the bad
    // tagger is printed, yet the tag's ignored extra header is what
    // `fsck_object()` returns, so `--tags` still names that tag.
    let (code, out, err) = same(stock, &dirs, &["-c", "fsck.badDateOverflow=ignore", "fsck", "--tags"], &[]);
    assert!(err.contains("missingNameBeforeEmail"), "{err}");
    assert!(out.contains("(t) in"), "{out}");
    assert_eq!(code, 1);
    // An error-severity `badTagName` ends the tag before its missing tagger.
    let (_, _, err) = same(stock, &dirs, &["-c", "fsck.badTagName=error", "fsck"], &[]);
    assert!(err.contains("badTagName") && !err.contains("missingTaggerEntry"), "{err}");
    // A warning does not: both come out.
    let (_, _, err) = same(stock, &dirs, &["-c", "fsck.extraHeaderEntry=warn", "fsck"], &[]);
    assert!(err.contains("missingTaggerEntry") && err.contains("extraHeaderEntry"), "{err}");
    // `--connectivity-only` parses the unreachable tag of no known type only in
    // `mark_unreachable_referents()`, and `parse_tag_buffer()` says why it failed.
    let (_, _, err) = same(stock, &dirs, &["fsck", "--connectivity-only"], &[]);
    assert!(err.contains("error: unknown tag type 'bogus' in "), "{err}");
}

#[test]
fn broken_refs_and_a_linked_worktree_head_are_snapshot_errors() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dirs = twins(stock, "refs", |dir| {
        std::fs::write(dir.join(".git/refs/heads/garb"), "garbage\n").unwrap();
        std::fs::write(dir.join(".git/refs/heads/sym"), "ref: refs/heads/nonexist\n").unwrap();
        git(stock, dir, &["worktree", "add", "-q", "--detach", "wt", "HEAD~1"]);
        std::fs::write(dir.join(".git/worktrees/wt/HEAD"), format!("{}\n", "9".repeat(40))).unwrap();
    });
    let (code, _, err) = same(stock, &dirs, &["fsck"], &[]);
    assert_eq!(code, 10, "{err}");
    assert!(err.contains("error: refs/heads/garb: invalid sha1 pointer 0000"), "{err}");
    assert!(err.contains("error: worktrees/wt/HEAD: invalid sha1 pointer 9999"), "{err}");
    same(stock, &dirs, &["fsck", "--name-objects", "--unreachable"], &[]);
    // The linked worktree's index comes after the main one, under its own path
    // for the cache tree as well (builtin/fsck.c:790-791, :958-980).
    let (_, _, err) = same(stock, &dirs, &["fsck", "--verbose"], &[]);
    assert!(err.contains("Checking cache tree of .git/worktrees/wt/index\n"), "{err}");
}

#[test]
fn unreadable_loose_objects_read_as_git_reads_them() {
    let Some(stock) = stock_git::stock_git() else { return };
    let empty = "80df6bb16cb0ffcbaca032a2c613bda03189eaab";
    let dirs = twins(stock, "loose", |dir| {
        let path = dir.join(".git/objects").join(&empty[..2]);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(&empty[2..]), b"").unwrap();
    });
    let (code, _, err) = same(stock, &dirs, &["fsck"], &[]);
    assert_eq!(code, 1);
    assert!(err.contains(": No such file or directory\n"), "{err}");
    same(stock, &dirs, &["fsck", "--connectivity-only"], &[]);

    let dirs = twins(stock, "badtype", |dir| {
        write_loose(dir, "1111111111111111111111111111111111111111", "blub", b"hello");
        std::fs::write(dir.join(".git/objects/11/junk"), "j\n").unwrap();
    });
    let (code, _, err) = same(stock, &dirs, &["fsck", "--connectivity-only"], &[]);
    assert_eq!((code, err.as_str()), (128, "fatal: invalid object type\n"));
    let (_, _, err) = same(stock, &dirs, &["fsck", "--progress"], &[]);
    assert!(err.contains("Checking object directories: 100% (256/256), done."), "{err}");
}
