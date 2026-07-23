//! `git status` long-format branch header honors `color.status.branch` (git's
//! `WT_STATUS_ONBRANCH` slot): the branch name in `On branch <name>` and the
//! object name in `HEAD detached at <sha>` are painted in that slot, defaulting
//! to uncolored. The prefix is the `header` slot for a real/unborn branch and the
//! `nobranch` slot for detached HEAD, preceded by git's leading empty `header`
//! write. These guard that zvcs matches git byte-for-byte on that header line
//! across every color-spec form the shared `color.c` parser accepts.
//!
//! Only the first line is compared: `color.status.branch` governs exactly that
//! line, and comparing it in isolation keeps the guard focused on the ported slot
//! rather than unrelated long-format coloring.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run system git in `dir` for fixture setup, asserting success.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// A repo with one commit and an unstaged modification so `status` prints a
/// non-empty long-format body under a branch header. Returns (repo, home).
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-colorcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hi\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    std::fs::write(repo.join("f"), "hi\nx\n").unwrap();
    (repo, home)
}

/// Force `color.status.branch` (and optionally `color.status.header`) then always-on
/// color, so output is deterministic without a TTY.
fn set_colors(repo: &Path, branch: &str, header: Option<&str>) {
    git(repo, &["config", "color.status.branch", branch]);
    if let Some(h) = header {
        git(repo, &["config", "color.status.header", h]);
    }
    git(repo, &["config", "color.ui", "always"]);
}

fn run_status(bin: &str, repo: &Path, home: &Path) -> Output {
    Command::new(bin)
        .args(["status"])
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

/// The raw bytes of the first output line (the branch header), reset excluded of
/// nothing — the escape sequences are part of what we assert.
fn header_line(o: &Output) -> Vec<u8> {
    o.stdout
        .split(|&b| b == b'\n')
        .next()
        .unwrap_or(&[])
        .to_vec()
}

fn assert_header_matches(repo: &Path, home: &Path, case: &str) {
    let z = run_status(BIN, repo, home);
    let g = run_status("git", repo, home);
    assert!(z.status.success(), "zvcs status failed: {case}");
    let (zl, gl) = (header_line(&z), header_line(&g));
    assert_eq!(
        zl,
        gl,
        "\ncase: {case}\nzvcs: {:?}\n git: {:?}",
        String::from_utf8_lossy(&zl),
        String::from_utf8_lossy(&gl),
    );
}

#[test]
fn status_branch_slot_on_branch_matches_git() {
    let (repo, home) = fixture("onbranch");
    // Every spec form the shared color.c parser handles: attribute+name, a bright
    // color, a 256 index, a 24-bit hex, and `normal` (no color). `header=yellow`
    // exercises the leading empty-header write and colored `On branch ` prefix.
    for (branch, header) in [
        ("blue bold", None),
        ("brightmagenta", None),
        ("214", None),
        ("#ff8800", None),
        ("normal", None),
        ("red", Some("yellow")),
    ] {
        set_colors(&repo, branch, header);
        assert_header_matches(&repo, &home, &format!("on-branch branch={branch:?} header={header:?}"));
    }
}

#[test]
fn status_branch_slot_detached_matches_git() {
    let (repo, home) = fixture("detached");
    git(&repo, &["checkout", "-q", "--detach"]);
    // Detached: prefix is the `nobranch` slot (default red), the object name is the
    // `branch` slot. `header=yellow` proves the leading empty-header write precedes
    // the red prefix, matching git's wt_longstatus_print ordering.
    for (branch, header) in [("blue bold", None), ("green", Some("yellow"))] {
        set_colors(&repo, branch, header);
        assert_header_matches(&repo, &home, &format!("detached branch={branch:?} header={header:?}"));
    }
}

#[test]
fn status_branch_slot_unborn_matches_git() {
    // Fresh repo, no commits: the unborn branch takes the same `On branch <name>`
    // path as a real branch — name in the `branch` slot.
    let root = std::env::temp_dir().join(format!("zvcs-colorcfg-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "work"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);

    set_colors(&repo, "magenta bold", None);
    assert_header_matches(&repo, &home, "unborn branch=magenta bold");
}
