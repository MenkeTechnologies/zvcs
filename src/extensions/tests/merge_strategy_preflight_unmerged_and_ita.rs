//! The `git diff-index --quiet --cached HEAD --` pre-flight of `git-merge-resolve.sh:11-14`
//! and `git-merge-octopus.sh:44-47`, on the two index shapes the ports got wrong.
//!
//! Expected output was read off `/opt/homebrew/bin/git` 2.55.0.
//!
//! 1. **A conflicted index.** `do_oneway_diff()` queues a `diff_unmerge()` pair for a
//!    stage≠0 path (diff-lib.c:467-473), so the name listing carries it and the script
//!    refuses with exit 2. Both strategies bailed with an "unsupported" error instead.
//! 2. **An `add -N` path.** A `--cached` plumbing diff leaves `ita_invisible_in_index`
//!    off (diff-lib.c:452-459), so the intent-to-add entry is a change and is listed.
//!    The pre-flight listed only the staged deletion.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("nonexistent-global"))
        .env("GIT_CONFIG_SYSTEM", dir.join("nonexistent-system"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zvcs-preflight-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn write(dir: &Path, path: &str, body: &str) {
    std::fs::write(dir.join(path), body).unwrap();
}

/// `base` holds `c.txt` and `keep.txt`; `side` rewrites `c.txt` and `third` adds
/// `t.txt`. Returns the merge base.
fn diverged(dir: &Path) -> String {
    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, "c.txt", "a\n");
    write(dir, "keep.txt", "k\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "base"]);
    let base = git(dir, &["rev-parse", "HEAD"]);
    git(dir, &["branch", "side"]);
    git(dir, &["branch", "third"]);
    git(dir, &["checkout", "-q", "side"]);
    write(dir, "c.txt", "b\n");
    git(dir, &["commit", "-qam", "side"]);
    git(dir, &["checkout", "-q", "third"]);
    write(dir, "t.txt", "t\n");
    git(dir, &["add", "t.txt"]);
    git(dir, &["commit", "-qm", "third"]);
    git(dir, &["checkout", "-q", "main"]);
    base
}

/// Leave `c.txt` at stages 1/2/3 by merging `side` into a `main` that also rewrote it.
fn conflicted(dir: &Path) -> String {
    let base = diverged(dir);
    write(dir, "c.txt", "c\n");
    git(dir, &["commit", "-qam", "main"]);
    let merge = run(dir, &["merge", "side"]);
    assert_eq!(merge.status.code(), Some(1), "the fixture merge must conflict");
    base
}

const REFUSAL: &str = "Error: Your local changes to the following files would be overwritten by merge\n";

fn assert_refused(out: &Output, listed: &[&str]) {
    let mut want = REFUSAL.to_owned();
    for path in listed {
        want.push_str(&format!("    {path}\n"));
    }
    assert_eq!(String::from_utf8_lossy(&out.stdout), want);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn resolve_refuses_a_conflicted_index() {
    let dir = temp_root("resolve-unmerged");
    let base = conflicted(&dir);
    let stages = git(&dir, &["ls-files", "-s"]);

    let out = run(&dir, &["merge-resolve", &base, "--", "HEAD", "side"]);
    assert_refused(&out, &["c.txt"]);
    assert_eq!(git(&dir, &["ls-files", "-s"]), stages, "the refusal must not touch the index");
}

#[test]
fn octopus_refuses_a_conflicted_index() {
    let dir = temp_root("octopus-unmerged");
    let base = conflicted(&dir);
    let stages = git(&dir, &["ls-files", "-s"]);

    let out = run(&dir, &["merge-octopus", &base, "--", "HEAD", "side", "third"]);
    assert_refused(&out, &["c.txt"]);
    assert_eq!(git(&dir, &["ls-files", "-s"]), stages, "the refusal must not touch the index");
}

#[test]
fn resolve_lists_an_intent_to_add_path() {
    let dir = temp_root("resolve-ita");
    let base = diverged(&dir);
    write(&dir, "ita.txt", "ita\n");
    git(&dir, &["add", "-N", "ita.txt"]);
    git(&dir, &["rm", "-q", "--cached", "keep.txt"]);

    let out = run(&dir, &["merge-resolve", &base, "--", "HEAD", "side"]);
    assert_refused(&out, &["ita.txt", "keep.txt"]);
}
