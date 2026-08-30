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
//! The **exit code** is the third. It used to be a constant: no check emitted
//! `Level::Fail`, so a script gating on `git zdoctor` was gating on nothing, and
//! this file recorded that. The two states that actually stop zvcs working — a
//! ledger it cannot read, a home it cannot write — are failures now, and the
//! cases below assert both the marker and the exit code, with an advisory
//! environment still exiting 0 so a non-zero exit keeps meaning something.

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
fn warnings_alone_do_not_fail_the_report() {
    // An environment that is merely unconfigured — no git on PATH, nothing
    // installed, no daemon — is not a broken one. Every check here is advisory,
    // so the report warns and still exits 0, which is what makes a non-zero exit
    // mean something when it does happen.
    let (repo, home) = fixture("warnings");

    let bare = doctor(&repo, &home, false);
    let report = text(&bare);
    assert!(report.contains("[WARN]"), "an unconfigured environment reported no warnings:\n{report}");
    assert!(!report.contains("[FAIL]"), "an unconfigured environment must not be reported as broken:\n{report}");
    assert!(bare.status.success(), "warnings alone must not fail the report:\n{report}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_ledger_the_tool_cannot_read_is_a_failure_not_an_ok() {
    // The check used to be `db_path().exists()`, so a corrupt ledger reported
    // `[ OK ]` and exit 0 while every read verb exited 1 with "file is not a
    // database". A health check that passes over the state that breaks the tool
    // is worse than none: it is the answer people trust instead of looking.
    let (repo, home) = fixture("ledger");
    assert!(run(&repo, &home, &["zreindex", "--sync", repo.to_str().unwrap()]).status.success());
    let db = home.join("zvcs").join("db.sqlite");
    assert!(db.exists(), "precondition: the ledger was created");

    // Healthy: OK, and the detail carries what it learned by asking.
    let healthy = doctor(&repo, &home, false);
    assert_eq!(status_of(&text(&healthy), "ledger"), "OK");
    assert!(healthy.status.success());

    // Corrupt: the file is still exactly where it was, and unusable.
    std::fs::write(&db, vec![0u8; 4096]).unwrap();
    let corrupt = doctor(&repo, &home, false);
    let report = text(&corrupt);
    assert_eq!(status_of(&report, "ledger"), "FAIL", "a corrupt ledger must be a failure:\n{report}");
    assert!(!corrupt.status.success(), "a failing check must make the report exit non-zero:\n{report}");
    // The verbs agree with the diagnosis.
    assert!(!run(&repo, &home, &["zrepos"]).status.success(), "precondition: the ledger really is unusable");

    // Absent is not broken: it is created on demand, and stays a warning.
    std::fs::remove_file(&db).unwrap();
    let absent = doctor(&repo, &home, false);
    assert_eq!(status_of(&text(&absent), "ledger"), "WARN", "an absent ledger is not a failure");
    assert!(absent.status.success());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_home_that_cannot_be_written_is_a_failure() {
    use std::os::unix::fs::PermissionsExt;
    let (repo, home) = fixture("home");

    let zhome = home.join("zvcs");
    let before = doctor(&repo, &home, false);
    assert_eq!(status_of(&text(&before), "home"), "OK");

    std::fs::set_permissions(&zhome, std::fs::Permissions::from_mode(0o500)).unwrap();
    let probe = zhome.join(".probe-can-i-write");
    let enforced = std::fs::write(&probe, b"").is_err();
    let _ = std::fs::remove_file(&probe);
    if !enforced {
        // A process that ignores permissions (root in a container) cannot run
        // this case; say so rather than pass on nothing.
        eprintln!("skipping: this process can write into a 0o500 directory (running as root?)");
        let _ = std::fs::set_permissions(&zhome, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
        return;
    }

    // Everything that records anything writes here, and each of those verbs
    // fails when it cannot. The report has to say so too.
    let out = doctor(&repo, &home, false);
    let report = text(&out);
    assert_eq!(status_of(&report, "home"), "FAIL", "an unwritable home must be a failure:\n{report}");
    assert!(!out.status.success(), "a failing check must make the report exit non-zero:\n{report}");

    let _ = std::fs::set_permissions(&zhome, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
