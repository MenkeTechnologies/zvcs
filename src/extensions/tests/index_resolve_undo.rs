//! The `REUC` (resolve-undo) index extension, end to end against stock git.
//!
//! `REUC` is the only trace a resolved conflict leaves behind: git copies each
//! unmerged stage's mode and blob id into it as the entry is removed from the
//! index (`record_resolve_undo()` from `remove_index_entry_at()`,
//! read-cache.c:1370-1371) and writes it back out with the index
//! (read-cache.c:2222). Nothing can recompute it afterwards — the stages it names
//! only existed before the resolution — so an index written without it silently
//! costs `git checkout --merge` and `git checkout --conflict=<style>` their
//! ability to put the conflict back.
//!
//! These tests therefore assert the thing that matters: not that zvcs can read
//! its own extension back, but that **stock git** finds the record and uses it.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A real git, or `None` when this machine has none to compare against.
///
/// The probe asks a candidate to run a superset verb: zvcs serves `zjobs` itself,
/// a real git does not. That test is only sound with `PATH` **emptied**: git's
/// `execv_dashed_external()` resolves an unknown verb to a `git-<verb>` on `PATH`,
/// and zvcs's own installation puts `~/.zvcs/bin/git-zjobs` there as a symlink to
/// the shadow binary — so with the ambient `PATH` every stock git on this machine
/// answers `zjobs` successfully and would be misread as zvcs, leaving every test
/// in this file to return early while reporting a pass. Candidates are absolute
/// paths for the same reason: `PATH=""` makes a bare `git` unspawnable.
fn stock_git() -> Option<String> {
    for cand in ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"] {
        if !Path::new(cand).exists() {
            continue;
        }
        match Command::new(cand).args(["zjobs"]).env("PATH", "").output() {
            Ok(out) if !out.status.success() => return Some(cand.to_string()),
            _ => continue,
        }
    }
    None
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

fn ok(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(bin, dir, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-reuc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

/// Build a repository whose `f.txt` is in conflict, using `bin` for every step.
///
/// The three blobs are fixed content, so the recorded stage ids are the same
/// whichever binary built the repository — which is what lets the byte-for-byte
/// comparison below mean something.
fn conflicted(bin: &str, dir: &Path) {
    ok(bin, dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("f.txt"), b"base\n").unwrap();
    ok(bin, dir, &["add", "f.txt"]);
    ok(bin, dir, &["commit", "-q", "-m", "base"]);
    ok(bin, dir, &["checkout", "-b", "other"]);
    std::fs::write(dir.join("f.txt"), b"theirs\n").unwrap();
    ok(bin, dir, &["commit", "-qam", "theirs"]);
    ok(bin, dir, &["checkout", "main"]);
    std::fs::write(dir.join("f.txt"), b"ours\n").unwrap();
    ok(bin, dir, &["commit", "-qam", "ours"]);
    // Expected to fail: this is the conflict.
    let _ = run(bin, dir, &["merge", "other"]);
}

/// The `REUC` extension of an index file, signature and size header included, or
/// `None` if the index carries none.
fn reuc_bytes(index: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(index).expect("index is readable");
    let at = data.windows(4).position(|w| w == b"REUC")?;
    let size = u32::from_be_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
    Some(data[at..at + 8 + size].to_vec())
}

/// Resolving a conflict through zvcs must leave stock git able to undo it.
///
/// This is the whole point of the extension, so the assertion is stock git's own
/// `checkout --merge`, which reports `Recreated 1 merge conflict` and puts the
/// three stages back (`unmerge_index_entry()`, resolve-undo.c:104-128). Before
/// the extension was written, that command had nothing to work from and left the
/// resolved file alone.
#[test]
fn stock_git_recreates_a_conflict_zvcs_resolved() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping resolve-undo interop test");
        return;
    };

    let repo = tmp("recreate");
    conflicted(BIN, &repo);
    std::fs::write(repo.join("f.txt"), b"resolved\n").unwrap();
    ok(BIN, &repo, &["add", "f.txt"]);

    let record = ok(&git, &repo, &["ls-files", "--resolve-undo"]);
    assert_eq!(
        record.lines().count(),
        3,
        "stock git must find all three recorded stages:\n{record}"
    );

    let fsck = run(&git, &repo, &["fsck", "--strict"]);
    assert!(
        fsck.status.success(),
        "git fsck --strict must pass on the index zvcs wrote:\n{}",
        String::from_utf8_lossy(&fsck.stderr)
    );

    let out = run(&git, &repo, &["checkout", "--merge", "f.txt"]);
    assert!(
        out.status.success(),
        "git checkout --merge must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let told = String::from_utf8_lossy(&out.stderr);
    assert!(
        told.contains("Recreated 1 merge conflict"),
        "stock git must report the conflict as recreated, said: {told}"
    );

    let stages = ok(&git, &repo, &["ls-files", "-s"]);
    assert_eq!(
        stages.lines().count(),
        3,
        "the three conflict stages must be back in the index:\n{stages}"
    );
    let worktree = std::fs::read_to_string(repo.join("f.txt")).unwrap();
    assert!(
        worktree.contains("<<<<<<<") && worktree.contains("ours") && worktree.contains("theirs"),
        "the conflicted file must be back in the worktree:\n{worktree}"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// The extension zvcs writes must be byte-for-byte the one stock git writes.
///
/// `resolve_undo_write()` (resolve-undo.c:33-50) has no room for interpretation —
/// path, three octal modes, then the object ids of the stages that existed — so a
/// difference here is a difference in the format, not in the repository. Both
/// sides build the same conflict from the same fixed blobs and resolve it the same
/// way, so the recorded stages are identical by construction.
#[test]
fn the_extension_zvcs_writes_is_the_one_stock_git_writes() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping resolve-undo byte comparison");
        return;
    };

    let root = tmp("bytes");
    let (stock_repo, zvcs_repo) = (root.join("stock"), root.join("zvcs"));
    std::fs::create_dir_all(&stock_repo).unwrap();
    std::fs::create_dir_all(&zvcs_repo).unwrap();

    for (bin, repo) in [(git.as_str(), &stock_repo), (BIN, &zvcs_repo)] {
        conflicted(bin, repo);
        std::fs::write(repo.join("f.txt"), b"resolved\n").unwrap();
        ok(bin, repo, &["add", "f.txt"]);
    }

    let expected = reuc_bytes(&stock_repo.join(".git/index"))
        .expect("stock git records resolve-undo when a conflict is resolved");
    let actual = reuc_bytes(&zvcs_repo.join(".git/index"))
        .expect("zvcs must record resolve-undo when a conflict is resolved");
    assert_eq!(
        actual, expected,
        "REUC bytes must match stock git's exactly\nzvcs:  {}\nstock: {}",
        hex(&actual),
        hex(&expected)
    );

    let _ = std::fs::remove_dir_all(&root);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Removing a conflicted path records it too.
///
/// `git rm -f <conflicted>` goes through the same `remove_index_entry_at()`, so
/// the stages are preserved even though no stage-0 entry replaced them — the case
/// the parity harness surfaced as `conflicted::rm::rm -f conflict.txt`.
#[test]
fn removing_a_conflicted_path_still_records_its_stages() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping resolve-undo removal test");
        return;
    };

    let repo = tmp("rm");
    conflicted(BIN, &repo);
    ok(BIN, &repo, &["rm", "-f", "f.txt"]);

    let record = ok(&git, &repo, &["ls-files", "--resolve-undo"]);
    assert_eq!(
        record.lines().count(),
        3,
        "stock git must find the stages of the removed conflicted path:\n{record}"
    );
    assert!(
        ok(&git, &repo, &["ls-files", "-s"]).trim().is_empty(),
        "the path itself must be gone from the index"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// `update-index --unresolve` consumes the record it restores from.
///
/// git puts the stages back and then drops the record (`string_list_remove()`,
/// resolve-undo.c:151-152), so a second `--unresolve` has nothing to do. Leaving
/// the record behind would let the same conflict be resurrected indefinitely.
#[test]
fn unresolve_restores_the_stages_and_forgets_the_record() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping unresolve test");
        return;
    };

    let repo = tmp("unresolve");
    conflicted(BIN, &repo);
    std::fs::write(repo.join("f.txt"), b"resolved\n").unwrap();
    ok(BIN, &repo, &["add", "f.txt"]);
    ok(BIN, &repo, &["update-index", "--unresolve", "f.txt"]);

    let stages = ok(&git, &repo, &["ls-files", "-s"]);
    assert_eq!(
        stages.lines().count(),
        3,
        "--unresolve must put the three stages back:\n{stages}"
    );
    assert!(
        ok(&git, &repo, &["ls-files", "--resolve-undo"]).trim().is_empty(),
        "--unresolve must consume the record, as stock git's does"
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// `--clear-resolve-undo` must actually clear it on disk.
///
/// The option has always dropped the records in memory; with the extension now
/// serialised, "in memory" and "on disk" can finally disagree, so this pins them
/// together.
#[test]
fn clear_resolve_undo_removes_the_extension_from_the_file() {
    let repo = tmp("clear");
    conflicted(BIN, &repo);
    std::fs::write(repo.join("f.txt"), b"resolved\n").unwrap();
    ok(BIN, &repo, &["add", "f.txt"]);
    assert!(
        reuc_bytes(&repo.join(".git/index")).is_some(),
        "the resolution must have been recorded first"
    );

    ok(BIN, &repo, &["update-index", "--clear-resolve-undo"]);
    assert!(
        reuc_bytes(&repo.join(".git/index")).is_none(),
        "--clear-resolve-undo must leave no REUC extension in the index file"
    );

    let _ = std::fs::remove_dir_all(&repo);
}
