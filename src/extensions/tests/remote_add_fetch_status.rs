//! `git remote add -f` and `git remote update` must report a failed fetch.
//!
//! Reported from the field as `Updating origin` / `error: Could not fetch
//! origin` with no reason attached. The reason turned out to be two separate
//! defects in the same six lines, and the exit code is the one that matters: a
//! script doing `git remote add -f … || exit` saw success.
//!
//! `fetch()` reports an ordinary failure as `Ok(ExitCode::from(128))` — `Err` is
//! reserved for the paths that die — so testing `is_err()` alone missed every
//! network, auth and no-such-repository failure there is.
//!
//! Expectations are stock git 2.55.0's, captured by running it. No network: the
//! remote is a filesystem path that is not a repository, which fails in the same
//! place a missing GitHub repository does.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-raf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let out = run(&repo, &home, &["init", "-q", "-b", "main"]);
    assert!(out.status.success());
    (repo, home)
}

/// The exit code is the defect a caller can act on: before this, the transport's
/// own error reached the terminal and the command still exited 0.
#[test]
fn remote_add_f_reports_a_failed_fetch() {
    let (repo, home) = fixture("addf");
    let missing = repo.join("no-such-remote");
    let url = missing.to_str().unwrap();

    let out = run(&repo, &home, &["remote", "add", "-f", "origin", url]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    // `Updating %s` is printf in builtin/remote.c, so it is stdout — it used to
    // go to stderr here, which reorders it against the transport's own output.
    assert_eq!(stdout, "Updating origin\n");
    assert!(
        stderr.ends_with("error: Could not fetch origin\n"),
        "the refusal must be the last thing said: {stderr:?}"
    );

    // The remote is still added: git adds first and fetches second, so a failed
    // fetch leaves the configuration behind.
    let listed = run(&repo, &home, &["remote"]);
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "origin\n");
}

/// `remote update` shares the fetch-status bug and the same fix.
#[test]
fn remote_update_reports_a_failed_fetch() {
    let (repo, home) = fixture("update");
    let missing = repo.join("no-such-remote");
    let url = missing.to_str().unwrap();
    assert!(run(&repo, &home, &["remote", "add", "origin", url]).status.success());

    let out = run(&repo, &home, &["remote", "update"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.ends_with("error: Could not fetch origin\n"),
        "stderr: {stderr:?}"
    );
}

/// The success path keeps its exit code and says nothing about failing.
#[test]
fn a_fetch_that_works_still_succeeds() {
    let (repo, home) = fixture("ok");
    let other = repo.parent().unwrap().join("upstream");
    std::fs::create_dir_all(&other).unwrap();
    assert!(run(&other, &home, &["init", "-q", "-b", "main"]).status.success());
    std::fs::write(other.join("f.txt"), "x\n").unwrap();
    assert!(run(&other, &home, &["add", "f.txt"]).status.success());
    assert!(run(
        &other,
        &home,
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "commit", "-qm", "c"]
    )
    .status
    .success());

    let out = run(&repo, &home, &["remote", "add", "-f", "origin", other.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Updating origin\n");
    assert!(!stderr.contains("Could not fetch"), "stderr: {stderr:?}");
}
