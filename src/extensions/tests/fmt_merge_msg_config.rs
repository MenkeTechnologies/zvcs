//! `git fmt-merge-msg` honors the `merge.*` knobs that shape the shortlog block
//! appended to a merge message: `merge.log` (int/bool — up to <n> shortlog lines
//! of the merged commits), its deprecated `merge.summary` alias (bool → 20), and
//! `merge.branchdesc` (splice `branch.<name>.description` into the block). The CLI
//! `--log[=<n>]` / `--no-log` overrides the config default. Each case is asserted
//! byte-for-byte against the same input fed to whatever is on PATH, since the
//! generated message must match stock git exactly.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with `main` checked out and a `side` branch carrying `commits`
/// single-parent commits ahead of the base. Returns `(repo, home, side_sha)`.
fn fixture(tag: &str, commits: usize) -> (PathBuf, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-fmtmsg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "Tester"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "side"]);
    for i in 1..=commits {
        let mut body = std::fs::read_to_string(repo.join("f")).unwrap();
        body.push_str(&format!("l{i}\n"));
        std::fs::write(repo.join("f"), body).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", &format!("side commit {i}")]);
    }
    git(&repo, &["checkout", "-q", "main"]);
    let out = Command::new("git")
        .args(["rev-parse", "side"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let side = String::from_utf8(out.stdout).unwrap().trim().to_string();
    (repo, home, side)
}

/// A single `FETCH_HEAD`-shaped mergeable line for `side` pulled from `.`.
fn input_for(side: &str) -> String {
    format!("{side}\t\tbranch 'side' of .\n")
}

/// Run one binary's `fmt-merge-msg` with `stdin` piped in, returning stdout.
fn fmt_merge_msg(bin: &str, repo: &Path, home: &Path, args: &[&str], stdin: &str) -> Vec<u8> {
    let mut child = Command::new(bin)
        .arg("fmt-merge-msg")
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{bin} fmt-merge-msg {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// zvcs must byte-match stock git for the same repo config, args and stdin.
fn assert_matches(repo: &Path, home: &Path, args: &[&str], stdin: &str) {
    let real = fmt_merge_msg("git", repo, home, args, stdin);
    let zvcs = fmt_merge_msg(BIN, repo, home, args, stdin);
    assert_eq!(
        String::from_utf8_lossy(&zvcs),
        String::from_utf8_lossy(&real),
        "fmt-merge-msg output diverged for args {args:?}"
    );
}

fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn merge_log_limits_the_shortlog() {
    let (repo, home, side) = fixture("log5", 7);
    git(&repo, &["config", "merge.log", "5"]);
    assert_matches(&repo, &home, &[], &input_for(&side));
    cleanup(&repo);
}

#[test]
fn no_log_overrides_merge_log() {
    let (repo, home, side) = fixture("nolog", 7);
    git(&repo, &["config", "merge.log", "5"]);
    assert_matches(&repo, &home, &["--no-log"], &input_for(&side));
    cleanup(&repo);
}

#[test]
fn merge_summary_true_is_the_log_alias() {
    let (repo, home, side) = fixture("summary", 3);
    git(&repo, &["config", "merge.summary", "true"]);
    assert_matches(&repo, &home, &[], &input_for(&side));
    cleanup(&repo);
}

#[test]
fn log_flag_overrides_disabled_config() {
    let (repo, home, side) = fixture("logflag", 3);
    // Config disables the shortlog; the CLI flag re-enables it.
    git(&repo, &["config", "merge.log", "false"]);
    assert_matches(&repo, &home, &["--log=2"], &input_for(&side));
    cleanup(&repo);
}

#[test]
fn merge_branchdesc_splices_the_branch_description() {
    let (repo, home, side) = fixture("branchdesc", 2);
    git(&repo, &["config", "merge.log", "20"]);
    git(&repo, &["config", "branch.side.description", "Fixes the frobnicator\nsecond line"]);
    git(&repo, &["config", "merge.branchdesc", "true"]);
    assert_matches(&repo, &home, &[], &input_for(&side));
    cleanup(&repo);
}
