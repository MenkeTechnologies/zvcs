//! `git show` shares `git log`'s display config: `log.date` (default for
//! `--date`), `log.abbrevCommit` (default for `--abbrev-commit`), and
//! `log.showRoot` (default for a root commit's empty-tree diff, forced on by
//! `--root`). show has its own rendering path — it does not delegate to log — so
//! these guard that its config plumbing stays in step with git's. Each test both
//! asserts the deterministic rendering and diffs the full output against the real
//! `git` binary byte-for-byte. An invalid `log.date` is fatal at config read,
//! even when a valid `--date` would otherwise take over.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The fixed author/committer date used for every fixture commit: git's classic
/// `1136214245 +0000` == `Mon Jan 2 15:04:05 2006 +0000`. The single-digit day
/// also pins the default `Date:` line's un-padded `%d` (one space before `2`).
const DATE: &str = "1136214245 +0000";

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with a root commit `c0` (creates `f`) and a child `c1` (modifies `f`),
/// both at the fixed [`DATE`], plus a `home` for `HOME`/`ZVCS_HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-showcfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c0", "--date", DATE]);
    std::fs::write(repo.join("f"), "hello\nworld\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c1", "--date", DATE]);
    (repo, home)
}

/// Run zvcs `show` with a deterministic environment (fixed clock, no system/user
/// config leaking in).
fn show(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["show"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

/// The same invocation against the real `git` binary, as a byte-for-byte oracle.
/// git reads the repo's `.git/config` for the shared keys just as zvcs does.
fn real(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["show"];
    args.extend_from_slice(extra);
    Command::new("git")
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Assert zvcs and real git produce identical stdout for `extra`.
fn assert_matches_git(repo: &Path, home: &Path, extra: &[&str]) {
    let mine = show(repo, home, extra);
    let theirs = real(repo, home, extra);
    assert_eq!(
        stdout(&theirs),
        stdout(&mine),
        "`show {extra:?}` must match real git byte-for-byte"
    );
}

fn rev(repo: &Path, spec: &str) -> String {
    let out = Command::new("git").args(["rev-parse", spec]).current_dir(repo).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn date_line(o: &Output) -> String {
    stdout(o).lines().find(|l| l.starts_with("Date:")).unwrap_or("").to_owned()
}

#[test]
fn show_date_config_and_override() {
    let (repo, home) = fixture("date");

    // log.date=short → the `Date:` line renders as an ISO short date.
    git(&repo, &["config", "log.date", "short"]);
    assert_eq!(date_line(&show(&repo, &home, &["-s", "HEAD"])), "Date:   2006-01-02");
    assert_matches_git(&repo, &home, &["HEAD"]);

    // --date=unix overrides log.date (which is still `short`).
    assert_eq!(
        date_line(&show(&repo, &home, &["-s", "--date=unix", "HEAD"])),
        "Date:   1136214245"
    );
    assert_matches_git(&repo, &home, &["--date=unix", "HEAD"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn show_default_date_padding_matches_git() {
    let (repo, home) = fixture("padding");

    // The default (`DATE_NORMAL`) `Date:` line uses git's un-padded `%d`: a
    // single-digit day is preceded by one space, not two. Guards a regression to
    // a `%e`-style space pad.
    assert_eq!(
        date_line(&show(&repo, &home, &["-s", "HEAD"])),
        "Date:   Mon Jan 2 15:04:05 2006 +0000"
    );
    assert_matches_git(&repo, &home, &["HEAD"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn show_abbrev_commit_config_and_override() {
    let (repo, home) = fixture("abbrev");
    let full = rev(&repo, "HEAD");

    // Default: the `commit` header carries the full 40-char id.
    let d = stdout(&show(&repo, &home, &["-s", "HEAD"]));
    assert!(d.contains(&format!("commit {full}")), "default shows the full id:\n{d}");

    // log.abbrevCommit=true → abbreviated header, like `--abbrev-commit`.
    git(&repo, &["config", "log.abbrevCommit", "true"]);
    let d = stdout(&show(&repo, &home, &["-s", "HEAD"]));
    assert!(!d.contains(&format!("commit {full}")), "config should abbreviate the id:\n{d}");
    assert!(d.lines().next().unwrap().starts_with("commit "), "still a commit header:\n{d}");
    assert_matches_git(&repo, &home, &["HEAD"]);

    // --no-abbrev-commit overrides the config back to the full id.
    let d = stdout(&show(&repo, &home, &["-s", "--no-abbrev-commit", "HEAD"]));
    assert!(
        d.contains(&format!("commit {full}")),
        "--no-abbrev-commit must override config:\n{d}"
    );
    assert_matches_git(&repo, &home, &["--no-abbrev-commit", "HEAD"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn show_show_root_config_suppress() {
    let (repo, home) = fixture("showroot");
    let root = rev(&repo, "HEAD~1");

    // Default (log.showRoot unset ⇒ true): the root commit shows its empty-tree
    // creation diff.
    let d = stdout(&show(&repo, &home, &[&root]));
    assert!(
        d.contains("diff --git a/f b/f"),
        "default log.showRoot=true must show the root diff:\n{d}"
    );
    assert_matches_git(&repo, &home, &[&root]);

    // log.showRoot=false hides the root commit's diff entirely; only the header
    // and message remain, with no trailing separator.
    git(&repo, &["config", "log.showRoot", "false"]);
    let d = stdout(&show(&repo, &home, &[&root]));
    assert!(!d.contains("diff --git"), "log.showRoot=false must suppress the root diff:\n{d}");
    assert!(d.lines().next().unwrap().starts_with("commit "), "header still shown:\n{d}");
    assert_matches_git(&repo, &home, &[&root]);

    // A non-root commit is unaffected by log.showRoot=false.
    assert_matches_git(&repo, &home, &["HEAD"]);

    // --root forces the root diff back on, overriding log.showRoot=false.
    let d = stdout(&show(&repo, &home, &["--root", &root]));
    assert!(
        d.contains("diff --git a/f b/f"),
        "--root must override log.showRoot=false:\n{d}"
    );
    assert_matches_git(&repo, &home, &["--root", &root]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn show_tag_honors_log_date() {
    let (repo, home) = fixture("tag");
    // An annotated tag whose tagger date is fixed (git tags take the date from
    // GIT_COMMITTER_DATE), so its `Date:` line is deterministic and subject to
    // log.date just like a commit's.
    assert!(
        Command::new("git")
            .args(["tag", "-a", "v1", "-m", "release"])
            .current_dir(&repo)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("GIT_AUTHOR_DATE", DATE)
            .status()
            .unwrap()
            .success(),
        "git tag failed"
    );

    git(&repo, &["config", "log.date", "short"]);
    let d = stdout(&show(&repo, &home, &["v1"]));
    assert!(
        d.lines().any(|l| l == "Date:   2006-01-02"),
        "log.date=short must format the tag's Date line:\n{d}"
    );
    assert_matches_git(&repo, &home, &["v1"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn show_date_invalid_is_fatal() {
    let (repo, home) = fixture("baddate");
    git(&repo, &["config", "log.date", "bogus"]);

    // git validates log.date at config read; an invalid value is fatal (128) even
    // though a valid --date is present on the command line.
    let out = show(&repo, &home, &["--date=unix", "HEAD"]);
    assert_eq!(out.status.code(), Some(128), "invalid log.date must exit 128");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown date format bogus"), "expected git's error message:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
