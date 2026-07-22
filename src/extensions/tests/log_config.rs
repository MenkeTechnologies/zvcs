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
fn log_show_root_default_and_config_suppress() {
    let (repo, home) = fixture("showroot");

    // Default (log.showRoot unset ⇒ true): the root commit shows its empty-tree
    // diff, so `-p` renders the file's creation patch.
    let d = stdout(&log(&repo, &home, &["-p"]));
    assert!(
        d.contains("diff --git a/f b/f"),
        "default log.showRoot=true must show the root diff:\n{d}"
    );

    // log.showRoot=false hides the root commit's diff entirely.
    git(&repo, &["config", "log.showRoot", "false"]);
    let d = stdout(&log(&repo, &home, &["-p"]));
    assert!(
        !d.contains("diff --git"),
        "log.showRoot=false must suppress the root diff:\n{d}"
    );
    // The commit header itself is still printed — only the diff is gone.
    assert!(d.lines().next().unwrap().starts_with("commit "), "header still shown:\n{d}");

    // --root forces the root diff back on, overriding log.showRoot=false. git has
    // no --no-root, so the command line can only turn it on.
    let d = stdout(&log(&repo, &home, &["-p", "--root"]));
    assert!(
        d.contains("diff --git a/f b/f"),
        "--root must override log.showRoot=false:\n{d}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_show_root_false_suppresses_all_diff_formats() {
    let (repo, home) = fixture("showroot-formats");
    git(&repo, &["config", "log.showRoot", "false"]);

    // log.showRoot gates the root commit at the tree-diff level, so every diff
    // format the root would otherwise produce is suppressed — not just `-p`.
    for fmt in ["--stat", "--name-only", "--name-status", "--numstat", "--shortstat"] {
        let d = stdout(&log(&repo, &home, &[fmt]));
        // Nothing beyond the commit header/message is emitted for the root: no
        // diffstat, no name list, no numstat/shortstat summary.
        let change_lines: Vec<&str> = d
            .lines()
            .filter(|l| {
                !l.starts_with("commit ")
                    && !l.starts_with("Author:")
                    && !l.starts_with("Date:")
                    && !l.trim_start().starts_with("c0")
                    && !l.trim().is_empty()
            })
            .collect();
        assert!(
            change_lines.is_empty(),
            "log.showRoot=false must emit no {fmt} change lines for the root:\n{d}\nleftover: {change_lines:?}"
        );
    }

    // With --root the same formats do report the file.
    let d = stdout(&log(&repo, &home, &["--stat", "--root"]));
    assert!(
        d.contains("1 file changed"),
        "--root must restore the root's --stat summary:\n{d}"
    );

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
