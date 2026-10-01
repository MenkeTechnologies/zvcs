//! `git history drop`, new in git 2.56.
//!
//! builtin/history.c (2.56.0) `cmd_history_drop()`: refuse a root or merge
//! target, replay the descendants onto the target's parent through
//! `compute_pending_ref_updates()`, try the worktree move dry
//! (`reset_working_tree()` with `RESET_WORKING_TREE_DRY_RUN`) before any
//! reference moves, apply the updates with the reflog message
//! `drop: dropping <arg>`, then move the worktree for real. Every object id,
//! message and exit status below was measured against stock git 2.56.0 on
//! these exact fixtures.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `(stdout, stderr, exit code)`.
fn outcome(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = run(dir, args);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-history-drop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// `main` is a-b-c-d, one file per commit named after it; `side` sits on c.
fn abcd(tag: &str) -> PathBuf {
    let repo = scratch(tag);
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for f in ["a", "b", "c", "d"] {
        std::fs::write(repo.join(f), format!("{f}\n")).unwrap();
        git(&repo, &["add", f]);
        git(&repo, &["commit", "-q", "-m", f]);
    }
    git(&repo, &["branch", "side", "HEAD~1"]);
    repo
}

const C_OLD: &str = "bade2e9e7a4579d10d10270dfb91ffb3fa126a4d";
const D_OLD: &str = "90ef2b4bcb971c49defdd65180fc3cb61a0239f0";
const C_NEW: &str = "4d4ca438f47d4600a449a3a37a0db65babe1dfc9";
const D_NEW: &str = "c2fb4878f12a08d9699e1657de6bc38c9b9d747c";

#[test]
fn dry_run_prints_the_updates_and_moves_nothing() {
    let repo = abcd("dry");
    let expect = format!("update refs/heads/side {C_NEW} {C_OLD}\nupdate refs/heads/main {D_NEW} {D_OLD}\n");
    assert_eq!(outcome(&repo, &["history", "drop", "--dry-run", "HEAD~2"]), (expect, String::new(), 0));
    assert_eq!(git(&repo, &["rev-parse", "main", "side"]), format!("{D_OLD}\n{C_OLD}\n"));
    assert!(repo.join("b").exists());

    let head_only = format!("update refs/heads/main {D_NEW} {D_OLD}\n");
    assert_eq!(
        outcome(&repo, &["history", "drop", "-n", "--update-refs=head", "HEAD~2"]),
        (head_only, String::new(), 0)
    );
}

#[test]
fn drop_rewrites_refs_reflog_index_and_worktree() {
    let repo = abcd("real");
    assert_eq!(outcome(&repo, &["history", "drop", "HEAD~2"]), (String::new(), String::new(), 0));
    assert_eq!(git(&repo, &["rev-parse", "main", "side"]), format!("{D_NEW}\n{C_NEW}\n"));
    assert_eq!(git(&repo, &["log", "--format=%s", "main"]), "d\nc\na\n");
    assert_eq!(git(&repo, &["reflog", "-1", "--format=%gs", "main"]), "drop: dropping HEAD~2\n");
    assert_eq!(git(&repo, &["reflog", "-1", "--format=%gs", "HEAD"]), "drop: dropping HEAD~2\n");
    // b's file left the worktree with its commit, and the index agrees.
    assert!(!repo.join("b").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn detached_head_is_updated_beside_the_branches() {
    let repo = abcd("detached");
    git(&repo, &["checkout", "-q", "--detach", "main"]);
    let expect = format!(
        "update refs/heads/side {C_NEW} {C_OLD}\nupdate HEAD {D_NEW} {D_OLD}\nupdate refs/heads/main {D_NEW} {D_OLD}\n"
    );
    assert_eq!(outcome(&repo, &["history", "drop", "--dry-run", "HEAD~2"]), (expect, String::new(), 0));
    assert_eq!(outcome(&repo, &["history", "drop", "HEAD~2"]), (String::new(), String::new(), 0));
    assert_eq!(git(&repo, &["rev-parse", "HEAD", "main"]), format!("{D_NEW}\n{D_NEW}\n"));
    assert!(!repo.join("b").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn local_changes_in_the_way_abort_before_any_ref_moves() {
    let repo = abcd("dirty");
    std::fs::write(repo.join("b"), "dirty\n").unwrap();
    let refusal = "error: Your local changes to the following files would be overwritten by checkout:\n\
                   \tb\n\
                   Please commit your changes or stash them before you switch branches.\n\
                   Aborting\n\
                   error: dropping this commit would overwrite local changes; aborting\n";
    for args in [&["history", "drop", "HEAD~2"][..], &["history", "drop", "--dry-run", "HEAD~2"][..]] {
        assert_eq!(outcome(&repo, args), (String::new(), refusal.to_owned(), 255), "{args:?}");
    }
    assert_eq!(git(&repo, &["rev-parse", "main"]), format!("{D_OLD}\n"));
}

#[test]
fn unrelated_local_changes_are_carried_along() {
    let repo = abcd("carry");
    std::fs::write(repo.join("zz"), "zz\n").unwrap();
    git(&repo, &["add", "zz"]);
    std::fs::write(repo.join("d"), "d\ndd\n").unwrap();
    assert_eq!(outcome(&repo, &["history", "drop", "HEAD~2"]), (String::new(), String::new(), 0));
    assert_eq!(git(&repo, &["status", "--short"]), " M d\nA  zz\n");
    assert_eq!(git(&repo, &["rev-parse", "main"]), format!("{D_NEW}\n"));
}

#[test]
fn untracked_file_in_the_way_is_found_only_by_the_real_move() {
    // The dry run has `.update = 0`, which `verify_absent_1()` returns early
    // on, so the refs move and only then does the checkout refuse.
    let repo = abcd("untracked");
    git(&repo, &["rm", "-q", "--cached", "b"]);
    let (out, err, code) = outcome(&repo, &["history", "drop", "HEAD~2"]);
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!(
            "error: The following untracked working tree files would be removed by checkout:\n\
             \tb\n\
             Please move or remove them before you switch branches.\n\
             Aborting\n\
             error: could not update working tree to new commit {D_NEW}\n"
        )
    );
    assert_eq!(code, 255);
    assert_eq!(git(&repo, &["rev-parse", "main"]), format!("{D_NEW}\n"));
    assert!(repo.join("b").exists());
}

#[test]
fn bare_repository_moves_refs_only() {
    let src = abcd("bare-src");
    let bare = scratch("bare");
    git(&bare, &["clone", "-q", "--bare", src.to_str().unwrap(), "."]);
    assert_eq!(outcome(&bare, &["history", "drop", "main~2"]), (String::new(), String::new(), 0));
    assert_eq!(git(&bare, &["rev-parse", "main"]), format!("{D_NEW}\n"));
}

#[test]
fn refusals() {
    let repo = abcd("refusals");
    let cases: [(&[&str], &str); 3] = [
        (&["history", "drop", "HEAD~3"], "error: cannot drop root commit HEAD~3: it has no parent to replay onto\n"),
        (&["history", "drop", "zzz"], "error: commit cannot be found: zzz\n"),
        (&["history", "drop"], "error: command expects a single revision\n"),
    ];
    for (args, err) in cases {
        assert_eq!(outcome(&repo, args), (String::new(), err.to_owned(), 255), "{args:?}");
    }

    git(&repo, &["checkout", "-q", "-b", "topic", "HEAD~1"]);
    std::fs::write(repo.join("t"), "t\n").unwrap();
    git(&repo, &["add", "t"]);
    git(&repo, &["commit", "-q", "-m", "t"]);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "M", "main"]);
    assert_eq!(
        outcome(&repo, &["history", "drop", "HEAD"]),
        (String::new(), "error: cannot drop merge commit: HEAD\n".to_owned(), 255)
    );
}

#[test]
fn descendant_conflict_and_emptiness() {
    // c1: f=1, c2: f=2, c3: f=3. Dropping c2 conflicts c3.
    let repo = scratch("conflict");
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for n in ["1", "2", "3"] {
        std::fs::write(repo.join("f"), format!("{n}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", &format!("c{n}")]);
    }
    assert_eq!(
        outcome(&repo, &["history", "drop", "HEAD~1"]),
        (String::new(), "error: failed replaying descendants\n".to_owned(), 255)
    );
    assert_eq!(git(&repo, &["rev-parse", "main"]), "e7f33787a94a7787b7348526fc8e31219df789bd\n");

    // c1: f=1, c2: f=2, c1: f=1 again, g. Dropping c2 empties the third.
    let repo = scratch("empty");
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for n in ["1", "2", "1"] {
        std::fs::write(repo.join("f"), format!("{n}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", &format!("c{n}")]);
    }
    std::fs::write(repo.join("g"), "1\n").unwrap();
    git(&repo, &["add", "g"]);
    git(&repo, &["commit", "-q", "-m", "g"]);
    const G: &str = "33111e03807788c66906a06748d46d52e7188f87";
    assert_eq!(
        outcome(&repo, &["history", "drop", "--empty=abort", "HEAD~2"]),
        (
            String::new(),
            "error: commit c183f0f343155a14cfe1b1c674300b4d136fa3be became empty after replay\n\
             error: failed replaying descendants\n"
                .to_owned(),
            255
        )
    );
    assert_eq!(
        outcome(&repo, &["history", "drop", "--empty=keep", "--dry-run", "HEAD~2"]),
        (format!("update refs/heads/main c4053b164d3e11cb82a3c6f31bd96abe5365b872 {G}\n"), String::new(), 0)
    );
    assert_eq!(
        outcome(&repo, &["history", "drop", "--dry-run", "HEAD~2"]),
        (format!("update refs/heads/main 172a7683c13bd8b234468359e15a3a305fa7f2e3 {G}\n"), String::new(), 0)
    );
}

const DROP_USAGE: &str = "usage: git history drop <commit> [--dry-run] [--update-refs=(branches|head)] [--empty=(drop|keep|abort)]

    --update-refs (branches|head)
                          control which refs should be updated
    -n, --[no-]dry-run    perform a dry-run without updating any refs
    --empty (drop|keep|abort)
                          how to handle descendants that become empty

";

#[test]
fn usage_and_option_errors() {
    let repo = abcd("usage");
    assert_eq!(outcome(&repo, &["history", "drop", "-h"]), (DROP_USAGE.to_owned(), String::new(), 0));
    assert_eq!(
        outcome(&repo, &["history", "drop", "--reedit-message", "x"]),
        (String::new(), format!("error: unknown option `reedit-message'\n{DROP_USAGE}"), 129)
    );
    assert_eq!(
        outcome(&repo, &["history", "drop", "--empty=zz", "x"]),
        (
            String::new(),
            "fatal: unrecognized '--empty=' action 'zz'; valid values are \"drop\", \"keep\", and \"abort\".\n".to_owned(),
            128
        )
    );
    let (out, _, code) = outcome(&repo, &["history", "-h"]);
    assert_eq!(code, 0);
    assert!(out.starts_with(
        "usage: git history drop <commit> [--dry-run] [--update-refs=(branches|head)] [--empty=(drop|keep|abort)]\n   or: git history fixup "
    ));
}
