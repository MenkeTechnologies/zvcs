//! `git zdoctor` — the "is my zvcs set up correctly" screen.
//!
//! A health check is only worth running if its answers move with the thing they
//! describe, so these cases change the environment and assert the report
//! follows: with the binary on PATH as `git` the shadow check reads OK and
//! without it WARN; before `git zshadow` the dashed-forms and completion checks
//! read WARN and after it OK.
//!
//! Two structural properties come with it. The **set of checks** is asserted by
//! name, because a check that quietly stops being emitted leaves a report that
//! still looks healthy. And the version line is compared against
//! `CARGO_PKG_VERSION` — the same constant the binary compiles in — so a
//! hand-maintained version string in the report cannot drift from the crate.
//!
//! One thing is pinned as it is rather than as it reads: **`zdoctor` cannot
//! currently fail.** Its contract is "exits non-zero only if a hard FAIL is
//! found", and no check emits `Level::Fail` — the source says so at
//! `doctor.rs:23`. So the scriptable exit code exists but nothing can trip it,
//! and a script gating on `git zdoctor` is gating on a constant. That is a
//! contract decision (which conditions deserve to be hard failures), not a
//! defect to patch from a test, so the case records today's answer and says
//! what it costs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `zdoctor`, optionally with a PATH that makes this binary the `git`.
fn doctor(dir: &Path, home: &Path, shadowed: bool) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("zdoctor")
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if shadowed {
        cmd.env("PATH", format!("{}:{}", home.join("bin").display(), std::env::var("PATH").unwrap_or_default()));
    } else {
        // A PATH with no `git` at all, so the shadow check has a definite answer.
        cmd.env("PATH", home.join("empty").display().to_string());
    }
    cmd.output().unwrap()
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// The status a named check reported: `Ok`, `Warn`, or `Fail`.
fn status_of(report: &str, label: &str) -> String {
    for line in report.lines() {
        if let Some((marker, rest)) = line.trim().split_once(']') {
            if rest.trim_start().starts_with(&format!("{label}:")) {
                return marker.trim_start_matches('[').trim().to_string();
            }
        }
    }
    panic!("no check named `{label}` in the report:\n{report}");
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zdoctor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    std::fs::create_dir_all(home.join("empty")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run(&repo, &home, &["init", "-q", "-b", "main", "."]).status.success());
    (repo, home)
}

#[test]
fn every_check_is_reported_and_the_version_matches_the_crate() {
    let (repo, home) = fixture("checks");
    let report = text(&doctor(&repo, &home, false));

    for label in [
        "version", "git shadow", "home", "daemon", "ledger", "man pages", "MANPATH",
        "dashed forms", "completion",
    ] {
        // `status_of` panics with the whole report when a check is missing.
        let _ = status_of(&report, label);
    }

    // The report's version is the binary's, not a string somebody maintains.
    assert!(
        report.contains(env!("CARGO_PKG_VERSION")),
        "the version check does not report {}:\n{report}",
        env!("CARGO_PKG_VERSION")
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_shadow_check_follows_whether_this_binary_is_the_git_on_path() {
    // The check that answers the question people actually run zdoctor for.
    let (repo, home) = fixture("shadow");

    let without = text(&doctor(&repo, &home, false));
    assert_eq!(status_of(&without, "git shadow"), "WARN", "{without}");

    std::fs::create_dir_all(home.join("bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(BIN, home.join("bin/git")).unwrap();
    let with = text(&doctor(&repo, &home, true));
    assert_eq!(status_of(&with, "git shadow"), "OK", "{with}");
    assert!(with.contains("zvcs is the git on PATH"), "{with}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn zshadow_turns_the_installation_checks_from_warn_to_ok() {
    // dashed forms and completion are the two checks `git zshadow` exists to
    // satisfy, so they are the ones that prove the report tracks the system
    // rather than printing a fixed screen.
    let (repo, home) = fixture("shadow-install");
    let before = text(&doctor(&repo, &home, false));
    assert_eq!(status_of(&before, "dashed forms"), "WARN", "{before}");
    assert_eq!(status_of(&before, "completion"), "WARN", "{before}");

    let installed = run(&repo, &home, &["zshadow"]);
    assert!(installed.status.success(), "zshadow failed: {}", text(&installed));

    let after = text(&doctor(&repo, &home, false));
    assert_eq!(status_of(&after, "dashed forms"), "OK", "{after}");
    assert_eq!(status_of(&after, "completion"), "OK", "{after}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_exit_code_is_zero_because_no_check_can_currently_fail() {
    // Recorded, not endorsed. `zdoctor` exits non-zero "only if a hard FAIL is
    // found" and no check emits `Level::Fail` (doctor.rs:23), so the exit code
    // is a constant and a script gating on it is gating on nothing. Which
    // conditions deserve to be hard failures is a decision about the verb's
    // contract; if that changes, this case should be rewritten to assert the
    // new one rather than deleted.
    let (repo, home) = fixture("exit");

    // Deliberately unhealthy: no git on PATH, nothing installed, no daemon.
    let bare = doctor(&repo, &home, false);
    assert!(bare.status.success(), "zdoctor learned to fail — rewrite this case");
    let report = text(&bare);
    assert!(report.contains("[WARN]"), "an unconfigured environment reported no warnings:\n{report}");
    assert!(!report.contains("[FAIL]"), "a check emitted FAIL — rewrite this case:\n{report}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
