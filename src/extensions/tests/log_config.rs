//! `git log` honors `log.abbrevCommit` (default for `--abbrev-commit`) and
//! `log.date` (default for `--date`), with the command line still overriding.
//! An invalid `log.date` is fatal at config read, matching git — even when a
//! valid `--date` would otherwise take over. Regression guard for the log
//! command ignoring its config entirely.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-logcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    // A fixed author/commit date so the formatted `%cd` is deterministic.
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "c0",
            "--date=1136214245 +0000",
        ],
    );
    (repo, home)
}

fn log(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["log"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env("GIT_AUTHOR_DATE", "1136214245 +0000")
        .env("GIT_COMMITTER_DATE", "1136214245 +0000")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn log_abbrev_commit_config_and_override() {
    let (repo, home) = fixture("abbrev");
    let full = {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };

    // Default: the `commit` header carries the full 40-char id.
    let d = stdout(&log(&repo, &home, &["-1"]));
    assert!(d.contains(&format!("commit {full}")), "default shows the full id:\n{d}");

    // log.abbrevCommit=true → abbreviated header, like `--abbrev-commit`.
    git(&repo, &["config", "log.abbrevCommit", "true"]);
    let d = stdout(&log(&repo, &home, &["-1"]));
    assert!(
        !d.contains(&format!("commit {full}")),
        "config should abbreviate the id:\n{d}"
    );
    assert!(d.lines().next().unwrap().starts_with("commit "), "still a commit header:\n{d}");

    // --no-abbrev-commit overrides the config back to the full id.
    let d = stdout(&log(&repo, &home, &["-1", "--no-abbrev-commit"]));
    assert!(
        d.contains(&format!("commit {full}")),
        "--no-abbrev-commit must override config:\n{d}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_date_config_and_override() {
    let (repo, home) = fixture("date");

    // log.date=short → `%ad` (the fixed author date) renders as an ISO short date.
    git(&repo, &["config", "log.date", "short"]);
    let d = stdout(&log(&repo, &home, &["-1", "--pretty=%ad"]));
    assert_eq!(d.trim(), "2006-01-02", "log.date=short must format %ad:\n{d}");

    // --date=unix overrides log.date.
    let d = stdout(&log(&repo, &home, &["-1", "--date=unix", "--pretty=%ad"]));
    assert_eq!(d.trim(), "1136214245", "--date must override log.date:\n{d}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_date_invalid_is_fatal() {
    let (repo, home) = fixture("baddate");
    git(&repo, &["config", "log.date", "bogus"]);

    // git validates log.date at config read; an invalid value is fatal (128)
    // even though a valid --date is present on the command line.
    let out = log(&repo, &home, &["-1", "--date=unix", "--pretty=%cd"]);
    assert_eq!(out.status.code(), Some(128), "invalid log.date must exit 128");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown date format bogus"),
        "expected git's error message:\n{err}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
