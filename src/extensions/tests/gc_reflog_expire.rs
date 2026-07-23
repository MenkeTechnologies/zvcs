//! `git gc` runs `reflog expire --all` before the repack.
//!
//! The reflog files under `logs/` are shared, git-written state, so this port
//! rewrites them exactly as stock git's `reflog expire --all` does: it drops the
//! same entries and keeps every survivor byte-for-byte (`gc` passes neither
//! `--rewrite` nor `--updateref`). Each case here crafts a reflog with entries
//! of known ages and reachability, then compares `zvcs gc`'s effect on `logs/`
//! against stock `git reflog expire --all` run on an identical copy — the two
//! must produce byte-identical reflog files.
//!
//! Two policies are exercised: the built-in default (total `now - 30 days`,
//! unreachable `now - 90 days`, which collapses to "drop everything older than
//! 30 days"), and the documented `gc.reflogExpire=90.days` /
//! `gc.reflogExpireUnreachable=30.days`, which opens the window where an
//! unreachable entry is dropped while a reachable one of the same age survives.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed in {}",
        dir.display()
    );
}

fn rev(dir: &Path, spec: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse {spec} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A one-line reflog record in git's on-disk format.
fn line(old: &str, new: &str, secs: i64, msg: &str) -> String {
    format!("{old} {new} A U Thor <a@b.c> {secs} +0000\t{msg}\n")
}

/// Build a fixture with commits `c0..c2` plus a dangling commit unreachable from
/// every ref, returning `(repo, c0, c1, c2, dangling)`.
fn fixture(tag: &str) -> (PathBuf, String, String, String, String) {
    let root = std::env::temp_dir().join(format!("zvcs-gcrfx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "a@b.c"]);
    git(&repo, &["config", "user.name", "A U Thor"]);
    std::fs::write(repo.join("f"), "0\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    let c0 = rev(&repo, "HEAD");
    std::fs::write(repo.join("f"), "1\n").unwrap();
    git(&repo, &["commit", "-q", "-m", "c1", "f"]);
    let c1 = rev(&repo, "HEAD");
    std::fs::write(repo.join("f"), "2\n").unwrap();
    git(&repo, &["commit", "-q", "-m", "c2", "f"]);
    let c2 = rev(&repo, "HEAD");
    // A commit reachable from no ref: create it, then move the branch back.
    std::fs::write(repo.join("f"), "D\n").unwrap();
    git(&repo, &["commit", "-q", "-m", "dangling", "f"]);
    let dangling = rev(&repo, "HEAD");
    git(&repo, &["reset", "-q", "--hard", &c2]);
    (repo, c0, c1, c2, dangling)
}

/// Overwrite `HEAD` and `refs/heads/main` reflogs with crafted, aged entries:
/// a reachable 100d entry (dropped by the total cutoff), a reachable 60d entry,
/// an unreachable 50d entry, an unreachable-old-side 20d entry, and a recent 5d
/// entry.
fn craft(repo: &Path, c0: &str, c1: &str, c2: &str, dangling: &str) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let day = 24 * 3600;
    let null = "0".repeat(40);
    let body = format!(
        "{}{}{}{}{}",
        line(&null, c0, now - 100 * day, "commit (initial): c0"),
        line(c0, c1, now - 60 * day, "commit: c1"),
        line(c1, dangling, now - 50 * day, "commit: dangling"),
        line(dangling, c2, now - 20 * day, "reset: moving to c2"),
        line(c1, c2, now - 5 * day, "commit: touch"),
    );
    std::fs::write(repo.join(".git/logs/HEAD"), &body).unwrap();
    std::fs::create_dir_all(repo.join(".git/logs/refs/heads")).unwrap();
    std::fs::write(repo.join(".git/logs/refs/heads/main"), &body).unwrap();
}

fn run(bin: &str, dir: &Path, args: &[&str]) {
    let home = dir.join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read(dir: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(dir.join(rel)).unwrap_or_default()
}

/// Copy `src` recursively to `dst` via `cp -r` (both are throwaway fixtures).
fn copy_tree(src: &Path, dst: &Path) {
    assert!(
        Command::new("cp").arg("-r").arg(src).arg(dst).status().unwrap().success(),
        "cp -r failed"
    );
}

/// Run one policy: craft the reflogs, copy the repo, expire via zvcs `gc` on one
/// side and stock `git reflog expire --all` on the other, and require the
/// resulting `logs/` files to match byte-for-byte. Returns the surviving HEAD
/// reflog so the caller can assert the expiry actually happened.
fn parity(tag: &str, config: &[(&str, &str)]) -> Vec<u8> {
    let (repo, c0, c1, c2, dangling) = fixture(tag);
    craft(&repo, &c0, &c1, &c2, &dangling);
    for (k, v) in config {
        git(&repo, &["config", k, v]);
    }
    let before = read(&repo, ".git/logs/HEAD");

    let git_repo = repo.parent().unwrap().join("repo-git");
    copy_tree(&repo, &git_repo);

    run(BIN, &repo, &["gc"]);
    run("git", &git_repo, &["reflog", "expire", "--all"]);

    let z_head = read(&repo, ".git/logs/HEAD");
    let g_head = read(&git_repo, ".git/logs/HEAD");
    let z_main = read(&repo, ".git/logs/refs/heads/main");
    let g_main = read(&git_repo, ".git/logs/refs/heads/main");

    assert_eq!(
        String::from_utf8_lossy(&z_head),
        String::from_utf8_lossy(&g_head),
        "[{tag}] HEAD reflog must match git byte-for-byte"
    );
    assert_eq!(
        String::from_utf8_lossy(&z_main),
        String::from_utf8_lossy(&g_main),
        "[{tag}] main reflog must match git byte-for-byte"
    );
    assert!(
        z_head.len() < before.len(),
        "[{tag}] expiry must actually drop entries (before {} bytes, after {})",
        before.len(),
        z_head.len()
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    z_head
}

#[test]
fn gc_reflog_expire_default_policy_matches_git() {
    // Built-in defaults: total is `now - 30 days`, so everything older than 30
    // days is dropped regardless of reachability. Only the 20d and 5d entries
    // survive.
    let head = parity("default", &[]);
    let kept = String::from_utf8_lossy(&head);
    assert!(kept.contains("reset: moving to c2"), "20d entry kept:\n{kept}");
    assert!(kept.contains("commit: touch"), "5d entry kept:\n{kept}");
    assert_eq!(kept.lines().count(), 2, "exactly two entries survive:\n{kept}");
}

#[test]
fn gc_reflog_expire_documented_policy_matches_git() {
    // total=90d keeps reachable entries out to 90 days; unreachable=30d drops the
    // unreachable 50d entry while keeping the reachable 60d one. Survivors: the
    // 60d reachable, the 20d (inside the unreachable grace), and the 5d entry.
    let head = parity(
        "documented",
        &[
            ("gc.reflogExpire", "90.days.ago"),
            ("gc.reflogExpireUnreachable", "30.days.ago"),
        ],
    );
    let kept = String::from_utf8_lossy(&head);
    assert!(kept.contains("commit: c1"), "reachable 60d entry kept:\n{kept}");
    assert!(!kept.contains("commit: dangling"), "unreachable 50d entry dropped:\n{kept}");
    assert!(kept.contains("reset: moving to c2"), "20d entry kept:\n{kept}");
    assert_eq!(kept.lines().count(), 3, "exactly three entries survive:\n{kept}");
}
