//! `zlog` shows a cross-repo reflog timeline; `zundo` rewinds a repo one step.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

fn head(dir: &Path) -> String {
    String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

#[test]
fn zlog_timeline_and_zundo_rewind() {
    let root = std::env::temp_dir().join(format!("zvcs-oplog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), b"1\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    let c0 = head(&repo);
    std::fs::write(repo.join("a.txt"), b"2\n").unwrap();
    git(&repo, &["commit", "-qam", "c1"]);
    let c1 = head(&repo);
    assert_ne!(c0, c1);

    // zlog shows the timeline (both commit messages).
    let (log, ok) = zvcs(&home, &repo, &["zlog"]);
    assert!(ok, "zlog failed");
    assert!(log.contains("c0") && log.contains("c1"), "zlog timeline:\n{log}");

    // zundo rewinds one step: back to c0, worktree restored.
    let (out, ok2) = zvcs(&home, &repo, &["zundo"]);
    assert!(ok2, "zundo failed: {out}");
    assert_eq!(head(&repo), c0, "zundo must move HEAD back to c0");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\n", "worktree must be restored");

    // Dirty worktree refuses undo (no clobber).
    std::fs::write(repo.join("a.txt"), b"dirty\n").unwrap();
    let (_o, ok3) = zvcs(&home, &repo, &["zundo"]);
    assert!(!ok3, "zundo must refuse on a dirty worktree");

    let _ = std::fs::remove_dir_all(&root);
}
