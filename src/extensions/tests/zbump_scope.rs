//! autobump must commit ONLY the submodule pointer bumps — never a developer's
//! unrelated `git add`ed work. The daemon runs autobump autonomously, so sweeping
//! staged files into the "zvcs: autobump" commit would silently commit (and thus
//! lose control of) uncommitted work.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn out_str(o: std::process::Output) -> String {
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn autobump_commits_only_pointer_not_unrelated_staged_work() {
    let root = std::env::temp_dir().join(format!("zvcs-bumpscope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Submodule source with two commits (so the checked-out sub can advance).
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);

    // Parent with the submodule added and committed.
    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    // Advance the checked-out submodule so its HEAD is ahead of the recorded gitlink.
    git(&parent.join("sub"), &["commit", "--allow-empty", "-q", "-m", "s1"]);

    // The developer stages an UNRELATED new file in the parent, intending to keep
    // working before committing it themselves.
    std::fs::write(parent.join("wip.txt"), b"half-finished\n").unwrap();
    git(&parent, &["add", "wip.txt"]);

    // Run the bump.
    let bump = Command::new(BIN).args(["zbump"]).current_dir(&parent).env("ZVCS_HOME", &home).output().unwrap();
    assert!(bump.status.success(), "zbump failed: {}", String::from_utf8_lossy(&bump.stderr));

    // The autobump commit must touch ONLY the submodule gitlink, not wip.txt.
    let changed = out_str(git(&parent, &["show", "--format=", "--name-only", "HEAD"]));
    assert!(changed.contains("sub"), "autobump commit must record the submodule pointer:\n{changed}");
    assert!(!changed.contains("wip.txt"), "autobump commit must NOT include the developer's unrelated staged file:\n{changed}");

    // And wip.txt must still be staged (preserved as the developer left it).
    let status = out_str(git(&parent, &["status", "--porcelain"]));
    assert!(status.lines().any(|l| l.starts_with("A ") && l.contains("wip.txt")), "wip.txt must remain staged, not committed:\n{status}");

    let _ = std::fs::remove_dir_all(&root);
}
