//! `git blame` honors `blame.showEmail` as the default for `-e`/`--show-email`,
//! with the command line still overriding (`--no-show-email`). Regression guard
//! for the config being ignored (author name always shown).

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
    let root = std::env::temp_dir().join(format!("zvcs-blamecfg-{tag}-{}", std::process::id()));
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
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["blame"];
    args.extend_from_slice(extra);
    args.push("f");
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn blame_show_email_config_and_override() {
    let (repo, home) = fixture("showemail");

    // Default: author name.
    let d = stdout(&blame(&repo, &home, &[]));
    assert!(d.contains("Alice"), "default shows the name:\n{d}");
    assert!(!d.contains("<alice@example.com>"), "default hides the email:\n{d}");

    // blame.showEmail=true → email column.
    git(&repo, &["config", "blame.showEmail", "true"]);
    let d = stdout(&blame(&repo, &home, &[]));
    assert!(d.contains("<alice@example.com>"), "config should show the email:\n{d}");

    // --no-show-email overrides the config back to the name.
    let d = stdout(&blame(&repo, &home, &["--no-show-email"]));
    assert!(d.contains("Alice"), "--no-show-email must override config:\n{d}");
    assert!(!d.contains("<alice@example.com>"), "email suppressed by override:\n{d}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// ---------------------------------------------------------------------------
// blame.date / `--date=<mode>`: the default date format for the human-format
// timestamp column, overridable on the command line. git validates the mode at
// config-read time (fatal, exit 128) exactly like the CLI flag.
// ---------------------------------------------------------------------------

/// Single-commit fixture with a fixed author/committer date so the blamed
/// timestamp is deterministic across machines and runs.
fn dated_fixture(tag: &str, date: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-blamedate-{tag}-{}", std::process::id()));
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
    assert!(
        Command::new("git")
            .args(["commit", "-q", "-m", "c0"])
            .current_dir(&repo)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap()
            .success(),
        "dated commit failed"
    );
    (repo, home)
}

/// Run `git blame [extra] f` in `repo` under an isolated, deterministic
/// environment. `bin` is either the zvcs binary or the system `git`, run with
/// byte-identical env so their outputs are directly comparable.
fn run_blame(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["blame"];
    args.extend_from_slice(extra);
    args.push("f");
    Command::new(bin)
        .args(&args)
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

fn zvcs_blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_blame(BIN, repo, home, extra)
}

fn real_blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_blame("git", repo, home, extra)
}

#[test]
fn blame_date_modes_match_git() {
    // UTC commit: `iso-strict` renders the `Z` zone and is shorter than its
    // fixed column width, exercising both the Z-form and left-justified padding.
    let (repo, home) = dated_fixture("modes", "1700000000 +0000");

    for m in [
        "iso",
        "iso8601",
        "iso-strict",
        "iso8601-strict",
        "short",
        "raw",
        "unix",
        "rfc",
        "rfc2822",
        "default",
    ] {
        let flag = format!("--date={m}");
        let z = zvcs_blame(&repo, &home, &[&flag]);
        let g = real_blame(&repo, &home, &[&flag]);
        assert!(
            z.status.success(),
            "zvcs --date={m} failed: {}",
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&z.stdout),
            "--date={m} must match git byte-for-byte"
        );
    }

    // Separate-argument form (`--date short`) is accepted like git's.
    let z = zvcs_blame(&repo, &home, &["--date", "short"]);
    let g = real_blame(&repo, &home, &["--date", "short"]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "`--date short` must match git"
    );

    // No flag and no config defaults to iso8601, matching git's blame default.
    let z = zvcs_blame(&repo, &home, &[]);
    let g = real_blame(&repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "default date column must match git"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_config_default_and_override() {
    let (repo, home) = dated_fixture("config", "1700000000 +0000");

    // blame.date supplies the default mode.
    git(&repo, &["config", "blame.date", "short"]);
    let z = zvcs_blame(&repo, &home, &[]);
    let g = real_blame(&repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "blame.date=short must apply and match git"
    );
    assert!(
        stdout(&z).contains("2023-11-14 1)"),
        "short is YYYY-MM-DD only:\n{}",
        stdout(&z)
    );

    // `--date` overrides blame.date.
    let z = zvcs_blame(&repo, &home, &["--date=raw"]);
    let g = real_blame(&repo, &home, &["--date=raw"]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "--date must override blame.date and match git"
    );
    assert!(
        stdout(&z).contains("1700000000 +0000"),
        "override to raw:\n{}",
        stdout(&z)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_invalid_is_fatal() {
    let (repo, home) = dated_fixture("invalid", "1700000000 +0000");

    // Unknown `--date` mode: git's exact fatal and exit code.
    let z = zvcs_blame(&repo, &home, &["--date=bogus"]);
    assert_eq!(z.status.code(), Some(128), "invalid --date exits 128");
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format bogus\n"
    );

    // Empty value is also unknown (matches git's empty-format message).
    let z = zvcs_blame(&repo, &home, &["--date="]);
    assert_eq!(z.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format \n"
    );

    // git validates blame.date at read time, so an invalid config value is
    // fatal even when a valid `--date` override is also present.
    git(&repo, &["config", "blame.date", "nope"]);
    let z = zvcs_blame(&repo, &home, &["--date=raw"]);
    assert_eq!(
        z.status.code(),
        Some(128),
        "invalid blame.date is fatal regardless of --date"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format nope\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_unsupported_modes_rejected() {
    let (repo, home) = dated_fixture("unsup", "1700000000 +0000");

    // These are valid git modes that need machinery blame.rs lacks (relative /
    // human rendering, local-timezone conversion, strftime). They must be
    // rejected rather than emitting wrong bytes, and must NOT be mislabeled as
    // an unknown-format fatal (which would be exit 128).
    for m in ["relative", "human", "iso-local", "default-local", "format:%Y"] {
        let flag = format!("--date={m}");
        let z = zvcs_blame(&repo, &home, &[&flag]);
        assert!(!z.status.success(), "--date={m} must be rejected");
        assert_ne!(
            z.status.code(),
            Some(128),
            "--date={m} is a valid git mode, not an unknown-format fatal"
        );
        let err = String::from_utf8_lossy(&z.stderr);
        assert!(
            err.contains("unsupported --date mode"),
            "--date={m} must be reported as unsupported:\n{err}"
        );
    }

    // `format` without a colon is git's missing-separator fatal (exit 128).
    let z = zvcs_blame(&repo, &home, &["--date=format"]);
    assert_eq!(z.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: date format missing colon separator: format\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
