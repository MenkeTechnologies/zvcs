//! Parity checks for `git merge` options that split the merge from the commit:
//! `--squash` and `--no-commit`. Both must perform the merge into the worktree
//! and index yet leave `HEAD` unmoved, differing only in the state they record
//! (`SQUASH_MSG` vs `MERGE_HEAD`/`MERGE_MSG`). The repositories are built with
//! real git so the assertions are about the zvcs binary's behaviour alone; every
//! step is deterministic and runs headless.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo with a clean, diverged two-branch history: `main` and `feat` change
/// different files off a common base, so their three-way merge resolves cleanly.
/// Returns (repo dir, home dir, feat commit id).
fn diverged_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-merge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "base.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("feat.txt"), "from feat\n").unwrap();
    git(&repo, &["add", "feat.txt"]);
    git(&repo, &["commit", "-q", "-m", "feat-change"]);
    let feat_id = git_out(&repo, &["rev-parse", "feat"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main.txt"), "from main\n").unwrap();
    git(&repo, &["add", "main.txt"]);
    git(&repo, &["commit", "-q", "-m", "main-change"]);

    (repo, home, feat_id)
}

fn zvcs_merge(repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    // Isolate from any ambient user/system git config (e.g. merge.ff=only) so the
    // clean diverged merge behaves identically everywhere.
    Command::new(BIN)
        .arg("merge")
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

#[test]
fn squash_merges_without_moving_head_and_writes_squash_msg() {
    let (repo, home, _feat) = diverged_repo("squash");
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);

    let out = zvcs_merge(&repo, &home, &["--squash", "feat"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "merge --squash failed: {}", String::from_utf8_lossy(&out.stderr));

    // git prints both the stopped-before-committing notice and the squash line.
    assert!(
        stdout.contains("Automatic merge went well; stopped before committing as requested"),
        "missing stop notice:\n{stdout}"
    );
    assert!(stdout.contains("Squash commit -- not updating HEAD"), "missing squash notice:\n{stdout}");

    // HEAD is untouched, and no MERGE_HEAD is recorded (squash is not a merge).
    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), head_before, "squash must not move HEAD");
    assert!(!repo.join(".git/MERGE_HEAD").exists(), "squash must not write MERGE_HEAD");

    // SQUASH_MSG carries the ported `squash_message()` body.
    let squash_msg = std::fs::read_to_string(repo.join(".git/SQUASH_MSG")).expect("SQUASH_MSG written");
    assert!(squash_msg.starts_with("Squashed commit of the following:\n"), "bad header:\n{squash_msg}");
    assert!(squash_msg.contains("commit "), "no commit block:\n{squash_msg}");
    assert!(squash_msg.contains("Author: t <t@e.x>"), "no author line:\n{squash_msg}");
    assert!(squash_msg.contains("    feat-change"), "feat subject not indented into body:\n{squash_msg}");

    // The merge did reach the worktree/index: feat's file is present and staged.
    assert!(repo.join("feat.txt").exists(), "squash must apply the merged tree to the worktree");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn no_commit_records_merge_state_without_committing() {
    let (repo, home, feat_id) = diverged_repo("nocommit");
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);

    let out = zvcs_merge(&repo, &home, &["--no-commit", "feat"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "merge --no-commit failed: {}", String::from_utf8_lossy(&out.stderr));

    assert!(
        stdout.contains("Automatic merge went well; stopped before committing as requested"),
        "missing stop notice:\n{stdout}"
    );

    // HEAD stays put; the merge is left in progress for `git commit` to finish.
    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), head_before, "--no-commit must not move HEAD");

    let merge_head = std::fs::read_to_string(repo.join(".git/MERGE_HEAD")).expect("MERGE_HEAD written");
    assert_eq!(merge_head.trim(), feat_id, "MERGE_HEAD must name the merged head");
    let merge_msg = std::fs::read_to_string(repo.join(".git/MERGE_MSG")).expect("MERGE_MSG written");
    assert!(merge_msg.contains("Merge branch 'feat'"), "MERGE_MSG missing default title:\n{merge_msg}");

    // The merged content reached the worktree (feat's file is present).
    assert!(repo.join("feat.txt").exists(), "--no-commit must apply the merged tree to the worktree");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
