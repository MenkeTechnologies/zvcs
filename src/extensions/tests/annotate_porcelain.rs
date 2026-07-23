//! `git annotate --porcelain` / `--line-porcelain` parity.
//!
//! `git annotate` is `git blame -c`, and `output()` checks `OUTPUT_PORCELAIN`
//! before `OUTPUT_ANNOTATE_COMPAT`, so the machine format wins over the compat
//! renderer. These tests pin the porcelain byte stream — group/per-line
//! headers, the author/committer/summary detail block, `boundary`, `previous`,
//! and `filename` — against the system `git` on the same repository, so both
//! binaries see identical object ids and metadata.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git_env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let ok = git_env(Command::new("git").args(args).current_dir(dir), home)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn commit(repo: &Path, home: &Path, msg: &str, date: &str) {
    let ok = git_env(
        Command::new("git").args(["commit", "-q", "-m", msg]).current_dir(repo),
        home,
    )
    .env("GIT_AUTHOR_DATE", date)
    .env("GIT_COMMITTER_DATE", date)
    .status()
    .unwrap()
    .success();
    assert!(ok, "commit failed");
}

/// Two-commit history whose blame yields a two-line hunk and a one-line hunk:
///
/// * lines 1-2 come from the root commit as a single hunk (`num_lines == 2`,
///   `boundary`, no `previous`) — this exercises the shorter per-line header for
///   the second line and the once-per-commit detail block in plain porcelain;
/// * line 3 comes from the child (`previous` points back at the root).
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-annporc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "alice@example.com"]);
    git(&repo, &home, &["config", "user.name", "Alice Example"]);

    std::fs::write(repo.join("f"), "alpha\nbeta\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    commit(&repo, &home, "add f", "1700000000 +0000");

    std::fs::write(repo.join("f"), "alpha\nbeta\ngamma\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    commit(&repo, &home, "third line", "1700000100 +0000");

    (repo, home)
}

fn run(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["annotate"];
    args.extend_from_slice(extra);
    args.push("f");
    git_env(Command::new(bin).args(&args).current_dir(repo), home)
        .output()
        .unwrap()
}

fn assert_parity(repo: &Path, home: &Path, flag: &str) {
    let z = run(BIN, repo, home, &[flag]);
    let g = run("git", repo, home, &[flag]);
    assert!(
        z.status.success(),
        "zvcs annotate {flag} failed: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "annotate {flag} must match git byte-for-byte"
    );
}

#[test]
fn annotate_porcelain_matches_git() {
    let (repo, home) = fixture("p");

    // Plain porcelain: the detail block appears once per commit; `-p` is the
    // same as `--porcelain`.
    assert_parity(&repo, &home, "--porcelain");
    assert_parity(&repo, &home, "-p");

    // Line-porcelain: the detail block is repeated before every line.
    assert_parity(&repo, &home, "--line-porcelain");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
