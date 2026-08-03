//! `git add <submodule>` must stage a moved submodule's current HEAD as the
//! parent gitlink (mode 160000) — the same as stock git. Regression: the native
//! `add` porcelain used to skip gitlinks entirely (walk kept only File/Symlink),
//! so `git add vendor/*` silently no-op'd through zvcs while real git staged the
//! pointer move — breaking the meta-repo's core submodule-bump workflow. Unlike
//! `git zbump`, plain `add` has no fast-forward gate and creates no commit.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// PATH with any zvcs shadow dir removed, so `git` in setup resolves to the real
/// system git (the shadow's own binary is exercised via `BIN` by absolute path).
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .env("PATH", real_git_path())
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

/// The staged gitlink SHA for `path` (empty string if not a gitlink / not staged).
fn staged_gitlink(parent: &Path, path: &str) -> String {
    out_str(git(parent, &["ls-files", "--stage", "--", path]))
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
}

#[test]
fn add_stages_moved_submodule_gitlink() {
    let root = std::env::temp_dir().join(format!("zvcs-addgitlink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Submodule source with two distinct commits so the pointer can actually move.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    let c0 = out_str(git(&sub_src, &["rev-parse", "HEAD"])).trim().to_string();
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s1"]);
    let c1 = out_str(git(&sub_src, &["rev-parse", "HEAD"])).trim().to_string();
    assert_ne!(c0, c1);

    // Parent embeds the submodule (at c1, the source's HEAD) and commits.
    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "vendor/sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c1, "precondition: embedded at c1");

    // Move the checked-out submodule to a DIFFERENT commit — a real gitlink change.
    git(&parent.join("vendor/sub"), &["checkout", "-q", &c0]);
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c1, "index still at c1 before add");

    // The fix under test: `git add vendor/*` (shell would expand the glob; pass the
    // resolved path) must stage the moved gitlink to the submodule's current HEAD.
    let add = Command::new(BIN)
        .args(["add", "vendor/sub"])
        .current_dir(&parent)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(add.status.success(), "zvcs add failed: {}", String::from_utf8_lossy(&add.stderr));

    // Gitlink must now be staged at c0 — exactly what stock git records.
    assert_eq!(
        staged_gitlink(&parent, "vendor/sub"),
        c0,
        "git add must stage the submodule's current HEAD ({c0}) as the gitlink"
    );

    // `add` must NOT create a commit (that is `zbump`, not `add`): HEAD unchanged.
    let head_subject = out_str(git(&parent, &["log", "-1", "--format=%s"]));
    assert_eq!(head_subject.trim(), "add sub", "git add must not commit the pointer move");

    // A no-op re-add of an unchanged gitlink stages nothing and still succeeds,
    // matching git (idempotent).
    let again = Command::new(BIN)
        .args(["add", "vendor/sub"])
        .current_dir(&parent)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(again.status.success(), "second add failed: {}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c0, "gitlink unchanged after no-op re-add");

    let _ = std::fs::remove_dir_all(&root);
}

/// `git stage` is stock git's own alias for `cmd_add` (git-stage(1): "This is a
/// synonym for git-add(1)"), so it has to record the same pointer moves.
///
/// Regression: `stage` ran its own worktree walk that kept only files and symlinks,
/// with no gitlink pass of its own, so `git stage <submodule>` exited 0 having
/// staged nothing at all — a silent no-op next to a working `git add`.
#[test]
fn stage_stages_moved_submodule_gitlink_like_add() {
    let root = std::env::temp_dir().join(format!("zvcs-stagegitlink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    let c0 = out_str(git(&sub_src, &["rev-parse", "HEAD"])).trim().to_string();
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s1"]);
    let c1 = out_str(git(&sub_src, &["rev-parse", "HEAD"])).trim().to_string();

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "vendor/sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);
    git(&parent.join("vendor/sub"), &["checkout", "-q", &c0]);
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c1, "index still at c1 before stage");

    let stage = Command::new(BIN)
        .args(["stage", "vendor/sub"])
        .current_dir(&parent)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(stage.status.success(), "zvcs stage failed: {}", String::from_utf8_lossy(&stage.stderr));
    assert_eq!(
        staged_gitlink(&parent, "vendor/sub"),
        c0,
        "git stage must stage the submodule's current HEAD ({c0}), exactly as git add does"
    );

    // `-u` restages tracked paths, which includes a moved gitlink.
    git(&parent, &["reset", "-q"]);
    git(&parent, &["stage", "-u"]);
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c0, "stage -u must pick the gitlink up too");

    // An unchanged gitlink stages nothing and still exits 0.
    let again = Command::new(BIN)
        .args(["stage", "vendor/sub"])
        .current_dir(&parent)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(again.status.success(), "second stage failed: {}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(staged_gitlink(&parent, "vendor/sub"), c0, "gitlink unchanged after no-op re-stage");

    let _ = std::fs::remove_dir_all(&root);
}
