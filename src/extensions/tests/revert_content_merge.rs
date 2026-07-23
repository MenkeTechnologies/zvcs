//! `git revert` of a commit whose change must be backed out *through* later,
//! non-overlapping work needs a content-level three-way merge of the affected
//! file — the trivial path-level resolution cannot serve it. These tests drive
//! that path and the conflict-stop path, checking the port against the system
//! `git` in byte-identical fixtures.
//!
//! Fixtures are built with the system `git` under a fixed identity and date so
//! the reverted commit's object id — and therefore the `This reverts commit
//! <hash>.` sentence in the generated message — is identical for both binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const DATE: &str = "1112911993 +0000"; // 2005-04-07 in UTC

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

fn init(repo: &Path, home: &Path) {
    run("git", repo, home, &["init", "-q", "-b", "main"]);
    run("git", repo, home, &["config", "user.name", "A U Thor"]);
    run("git", repo, home, &["config", "user.email", "author@example.com"]);
}

fn commit(repo: &Path, home: &Path, body: &str, msg: &str) {
    std::fs::write(repo.join("file.txt"), body).unwrap();
    run("git", repo, home, &["add", "file.txt"]);
    run("git", repo, home, &["commit", "-q", "-m", msg]);
}

/// Build a fresh work area with `home` and `repo` directories.
fn work_area(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-revertcm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home)
}

const BASE: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";

/// `C2` changes the top region, `C3` a well-separated bottom region. Reverting
/// `C2` while `HEAD == C3` must undo the top change and keep the bottom one — a
/// genuine content-level three-way merge of `file.txt`, since both sides moved
/// the whole blob off the reverted commit's tree.
fn build_content_merge_fixture(tag: &str) -> (PathBuf, PathBuf, String) {
    let (repo, home) = work_area(tag);
    init(&repo, &home);
    commit(&repo, &home, BASE, "base");
    commit(&repo, &home, "a\nB\nc\nd\ne\nf\ng\nh\n", "cap b"); // C2
    commit(&repo, &home, "a\nB\nc\nd\ne\nf\nG\nh\n", "cap g"); // C3, HEAD
    // The revert target is C2 (HEAD~1); resolve its full id so both binaries
    // name the same commit.
    let o = run("git", &repo, &home, &["rev-parse", "HEAD~1"]);
    let target = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (repo, home, target)
}

/// A content-level revert that resolves cleanly must produce the same worktree
/// file and the same commit message as stock git, and succeed.
#[test]
fn content_level_revert_matches_git() {
    let (zrepo, zhome, ztarget) = build_content_merge_fixture("clean-zvcs");
    let (grepo, ghome, gtarget) = build_content_merge_fixture("clean-git");
    assert_eq!(ztarget, gtarget, "fixtures must name the identical target commit");

    let zo = run(BIN, &zrepo, &zhome, &["revert", "--no-edit", &ztarget]);
    let go = run("git", &grepo, &ghome, &["revert", "--no-edit", &gtarget]);

    assert!(
        zo.status.success(),
        "zvcs content-level revert failed: {}",
        String::from_utf8_lossy(&zo.stderr)
    );
    assert!(
        go.status.success(),
        "git content-level revert failed: {}",
        String::from_utf8_lossy(&go.stderr)
    );

    // The top change is undone (`B` -> `b`), the bottom change preserved (`G`).
    let zfile = std::fs::read(zrepo.join("file.txt")).unwrap();
    let gfile = std::fs::read(grepo.join("file.txt")).unwrap();
    assert_eq!(
        gfile, zfile,
        "reverted worktree file must match git byte-for-byte\nzvcs: {:?}\ngit:  {:?}",
        String::from_utf8_lossy(&zfile),
        String::from_utf8_lossy(&gfile)
    );
    assert_eq!(
        zfile, b"a\nb\nc\nd\ne\nf\nG\nh\n",
        "content merge must revert the top and keep the bottom"
    );

    let zmsg = run("git", &zrepo, &zhome, &["log", "-1", "--format=%B"]);
    let gmsg = run("git", &grepo, &ghome, &["log", "-1", "--format=%B"]);
    assert_eq!(
        String::from_utf8_lossy(&gmsg.stdout),
        String::from_utf8_lossy(&zmsg.stdout),
        "generated revert message must match git byte-for-byte"
    );

    let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
}

/// `C2` and `C3` both rewrite the *same* line, so reverting `C2` conflicts with
/// `C3`. The port must stop the way git does: exit 1, write `REVERT_HEAD` and a
/// `MERGE_MSG` carrying the `# Conflicts:` hint, and leave conflict markers in
/// the worktree file.
#[test]
fn conflicting_revert_stops_like_git() {
    let (repo, home) = work_area("conflict-zvcs");
    init(&repo, &home);
    commit(&repo, &home, "a\nx\nc\n", "base");
    commit(&repo, &home, "a\ny\nc\n", "to y"); // C2 (target)
    commit(&repo, &home, "a\nz\nc\n", "to z"); // C3, HEAD
    let o = run("git", &repo, &home, &["rev-parse", "HEAD~1"]);
    let target = String::from_utf8_lossy(&o.stdout).trim().to_string();

    // git's exit status for the same conflicting revert, in a parallel repo.
    let (grepo, ghome) = work_area("conflict-git");
    init(&grepo, &ghome);
    commit(&grepo, &ghome, "a\nx\nc\n", "base");
    commit(&grepo, &ghome, "a\ny\nc\n", "to y");
    commit(&grepo, &ghome, "a\nz\nc\n", "to z");
    let go = run("git", &grepo, &ghome, &["revert", "--no-edit", &target]);

    let zo = run(BIN, &repo, &home, &["revert", "--no-edit", &target]);

    assert_eq!(
        go.status.code(),
        Some(1),
        "git conflicting revert should exit 1: {}",
        String::from_utf8_lossy(&go.stderr)
    );
    assert_eq!(
        zo.status.code(),
        Some(1),
        "zvcs conflicting revert must exit 1 like git: {}",
        String::from_utf8_lossy(&zo.stderr)
    );

    assert!(
        repo.join(".git/REVERT_HEAD").exists(),
        "a stopped revert must record REVERT_HEAD"
    );
    let merge_msg = std::fs::read_to_string(repo.join(".git/MERGE_MSG")).unwrap();
    assert!(
        merge_msg.contains("# Conflicts:") && merge_msg.contains("#\tfile.txt"),
        "MERGE_MSG must carry git's conflict hint:\n{merge_msg}"
    );

    let worktree = std::fs::read_to_string(repo.join("file.txt")).unwrap();
    assert!(
        worktree.contains("<<<<<<< HEAD") && worktree.contains(">>>>>>> parent of"),
        "conflicting revert must leave git-style conflict markers:\n{worktree}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
}
