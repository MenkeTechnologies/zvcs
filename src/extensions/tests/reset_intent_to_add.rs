//! Parity coverage for `git reset -N` / `--pathspec-from-file`, exercised through
//! the ported binary and verified with the real `git` reading the index it writes.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
/// The empty-blob object id every intent-to-add stub carries.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn bin(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().unwrap()
}

fn stdout(o: Output) -> String {
    String::from_utf8(o.stdout).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A fresh repo with a single committed `base.txt`.
fn repo(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-reset-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    std::fs::write(p.join("base.txt"), "base\n").unwrap();
    git(&p, &["add", "base.txt"]);
    git(&p, &["commit", "-q", "-m", "init"]);
    p
}

#[test]
fn reset_n_keeps_removed_path_as_intent_to_add() {
    let r = repo("ita-path");
    std::fs::write(r.join("new.txt"), "hello\n").unwrap();
    git(&r, &["add", "new.txt"]);

    let o = bin(&r, &["reset", "-q", "-N", "--", "new.txt"]);
    assert!(o.status.success(), "reset -N: {}", stderr(&o));

    // The dropped addition survives as the empty-blob stub, not its staged content.
    let stage = stdout(git(&r, &["ls-files", "--stage", "new.txt"]));
    assert!(
        stage.starts_with(&format!("100644 {EMPTY_BLOB} 0")),
        "expected empty-blob i-t-a stub, got: {stage:?}"
    );
    // The intent-to-add flag round-tripped through the on-disk index: git treats the
    // path as *not* staged for commit (empty cached diff)...
    let cached = stdout(git(&r, &["diff", "--cached", "--name-only"]));
    assert!(!cached.contains("new.txt"), "i-t-a path must not be staged: {cached:?}");
    // ...yet it remains a tracked worktree addition.
    let status = stdout(git(&r, &["status", "--porcelain", "new.txt"]));
    assert!(status.contains("new.txt"), "status should list the i-t-a path: {status:?}");
}

#[test]
fn reset_n_whole_tree_keeps_staged_addition() {
    let r = repo("ita-whole");
    std::fs::write(r.join("new.txt"), "hi\n").unwrap();
    git(&r, &["add", "new.txt"]);

    let o = bin(&r, &["reset", "-q", "-N"]);
    assert!(o.status.success(), "reset -N (whole tree): {}", stderr(&o));

    let stage = stdout(git(&r, &["ls-files", "--stage", "new.txt"]));
    assert!(
        stage.starts_with(&format!("100644 {EMPTY_BLOB} 0")),
        "expected empty-blob i-t-a stub, got: {stage:?}"
    );
    let cached = stdout(git(&r, &["diff", "--cached", "--name-only"]));
    assert!(!cached.contains("new.txt"), "i-t-a path must not be staged: {cached:?}");
}

#[test]
fn reset_n_requires_mixed() {
    let r = repo("ita-mixed");
    let o = bin(&r, &["reset", "--soft", "-N"]);
    assert_eq!(o.status.code(), Some(128), "stderr: {}", stderr(&o));
    assert_eq!(stderr(&o), "fatal: the option '-N' requires '--mixed'\n");
}

#[test]
fn pathspec_file_nul_requires_from_file() {
    let r = repo("nul-alone");
    let o = bin(&r, &["reset", "--pathspec-file-nul"]);
    assert_eq!(o.status.code(), Some(128), "stderr: {}", stderr(&o));
    assert_eq!(
        stderr(&o),
        "fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'\n"
    );
}

#[test]
fn pathspec_from_file_conflicts_with_inline_pathspec() {
    let r = repo("pff-conflict");
    std::fs::write(r.join("list"), "base.txt\n").unwrap();
    let o = bin(&r, &["reset", "--pathspec-from-file=list", "base.txt"]);
    assert_eq!(o.status.code(), Some(128), "stderr: {}", stderr(&o));
    assert_eq!(
        stderr(&o),
        "fatal: '--pathspec-from-file' and pathspec arguments cannot be used together\n"
    );
}

#[test]
fn pathspec_from_file_resets_listed_paths() {
    let r = repo("pff-reset");
    std::fs::write(r.join("base.txt"), "changed\n").unwrap();
    git(&r, &["add", "base.txt"]);
    assert!(
        stdout(git(&r, &["diff", "--cached", "--name-only"])).contains("base.txt"),
        "precondition: base.txt should be staged"
    );

    std::fs::write(r.join("list"), "base.txt\n").unwrap();
    let o = bin(&r, &["reset", "-q", "--pathspec-from-file=list"]);
    assert!(o.status.success(), "reset --pathspec-from-file: {}", stderr(&o));

    let cached = stdout(git(&r, &["diff", "--cached", "--name-only"]));
    assert!(!cached.contains("base.txt"), "base.txt should be unstaged: {cached:?}");
}

#[test]
fn pathspec_from_file_nul_separated() {
    let r = repo("pff-nul");
    std::fs::write(r.join("base.txt"), "changed\n").unwrap();
    git(&r, &["add", "base.txt"]);

    std::fs::write(r.join("list"), "base.txt\0").unwrap();
    let o = bin(&r, &["reset", "-q", "--pathspec-from-file=list", "--pathspec-file-nul"]);
    assert!(o.status.success(), "reset nul: {}", stderr(&o));

    let cached = stdout(git(&r, &["diff", "--cached", "--name-only"]));
    assert!(!cached.contains("base.txt"), "base.txt should be unstaged: {cached:?}");
}
