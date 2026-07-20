//! `zup`/reconcile must move a clean worktree across a fast-forward that ADDS,
//! MODIFIES, and DELETES files — writing the changed files, removing the deleted
//! ones, and leaving a *clean* index (so `git status` doesn't disagree
//! afterwards, which would mean the reused stats are wrong).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn porcelain(dir: &Path) -> String {
    String::from_utf8(Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output().unwrap().stdout).unwrap()
}

#[test]
fn zup_ff_applies_add_modify_delete_and_leaves_clean_index() {
    let root = std::env::temp_dir().join(format!("zvcs-recwt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);

    // work: c0 with a.txt and b.txt, push.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    std::fs::write(work.join("a.txt"), b"1\n").unwrap();
    std::fs::write(work.join("b.txt"), b"keep\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);

    // work2: modify a.txt, delete b.txt, add c.txt; push c1.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work2"]);
    let work2 = root.join("work2");
    git(&work2, &["checkout", "-q", "main"]);
    std::fs::write(work2.join("a.txt"), b"2\n").unwrap();
    std::fs::remove_file(work2.join("b.txt")).unwrap();
    std::fs::write(work2.join("c.txt"), b"new\n").unwrap();
    git(&work2, &["add", "-A"]);
    git(&work2, &["commit", "-q", "-m", "c1"]);
    git(&work2, &["push", "-q", "origin", "main"]);

    // work is behind and unfetched → zup fast-forwards it, applying all changes.
    let out = Command::new(BIN).args(["zup"]).current_dir(&work).env("ZVCS_HOME", &home).output().unwrap();
    assert!(out.status.success(), "zup failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert_eq!(std::fs::read_to_string(work.join("a.txt")).unwrap(), "2\n", "modified file not updated");
    assert!(!work.join("b.txt").exists(), "deleted file not removed");
    assert_eq!(std::fs::read_to_string(work.join("c.txt")).unwrap(), "new\n", "added file not written");

    // The index must agree with the worktree (reused stats correct) → clean status.
    let st = porcelain(&work);
    assert!(st.trim().is_empty(), "index/worktree disagree after ff (stale stats?):\n{st}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zup_skips_dirty_worktree_without_clobbering() {
    // An update is available upstream, but the local worktree has uncommitted
    // work → reconcile must skip it, preserving the change and not moving HEAD.
    let root = std::env::temp_dir().join(format!("zvcs-recdirty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    std::fs::write(work.join("a.txt"), b"1\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let c0 = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap().trim().to_string();

    // Advance origin to c1 from a second clone.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work2"]);
    let work2 = root.join("work2");
    git(&work2, &["checkout", "-q", "main"]);
    git(&work2, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    git(&work2, &["push", "-q", "origin", "main"]);

    // Dirty the local worktree, then zup: must skip, not clobber.
    std::fs::write(work.join("a.txt"), b"IN FLIGHT\n").unwrap();
    let out = Command::new(BIN).args(["zup"]).current_dir(&work).env("ZVCS_HOME", &home).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("dirty"), "zup should report the dirty skip:\n{report}");

    assert_eq!(std::fs::read_to_string(work.join("a.txt")).unwrap(), "IN FLIGHT\n", "in-flight change must be preserved");
    let head_now = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap().trim().to_string();
    assert_eq!(head_now, c0, "HEAD must not move on a dirty repo");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zup_refuses_to_overwrite_untracked_file() {
    // The new tree adds a tracked file that collides with an UNTRACKED file on
    // disk. is_dirty() ignores untracked files, so reconcile must refuse the ff
    // rather than silently clobber it (headless data loss).
    let root = std::env::temp_dir().join(format!("zvcs-untracked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    std::fs::write(work.join("a.txt"), b"1\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let c0 = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap().trim().to_string();

    // Remote adds a tracked file `new.txt`.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work2"]);
    let work2 = root.join("work2");
    git(&work2, &["checkout", "-q", "main"]);
    std::fs::write(work2.join("new.txt"), b"FROM REMOTE\n").unwrap();
    git(&work2, &["add", "new.txt"]);
    git(&work2, &["commit", "-q", "-m", "c1"]);
    git(&work2, &["push", "-q", "origin", "main"]);

    // Locally, an UNTRACKED new.txt exists with the user's content.
    std::fs::write(work.join("new.txt"), b"MY UNTRACKED WORK\n").unwrap();

    let out = Command::new(BIN).args(["zup"]).current_dir(&work).env("ZVCS_HOME", &home).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("would overwrite untracked") || report.contains("skipped"), "reconcile should refuse:\n{report}");

    // The untracked file must be intact and HEAD must not have moved.
    assert_eq!(std::fs::read_to_string(work.join("new.txt")).unwrap(), "MY UNTRACKED WORK\n", "untracked file was clobbered!");
    let head_now = String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap().trim().to_string();
    assert_eq!(head_now, c0, "HEAD must not move when the ff is refused");

    let _ = std::fs::remove_dir_all(&root);
}
