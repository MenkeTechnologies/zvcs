//! `zjobs -n` must clamp to a positive limit: SQLite treats a negative LIMIT as
//! "unlimited", so a stray `-n -1` / `-n 0` would dump the entire ledger.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

fn zjobs(home: &Path, cwd: &Path, extra: &[&str]) -> String {
    let mut args = vec!["zjobs"];
    args.extend_from_slice(extra);
    String::from_utf8_lossy(
        &Command::new(BIN).args(&args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap().stdout,
    )
    .into_owned()
}

fn job_lines(out: &str) -> usize {
    out.lines().filter(|l| l.starts_with('#')).count()
}

#[test]
fn zjobs_negative_n_is_clamped_not_unlimited() {
    let root = std::env::temp_dir().join(format!("zvcs-zjobsn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Two repos, indexed. A failing foreach records one failed job per repo (2).
    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    }
    assert!(Command::new(BIN).args(["zreindex", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).status().unwrap().success());
    Command::new(BIN)
        .args(["zforeach", "--", "git", "rev-parse", "--verify", "--quiet", "no-such-ref"])
        .current_dir(&root).env("ZVCS_HOME", &home).output().unwrap();

    // Sanity: the ledger really has 2 jobs (default limit shows both).
    assert_eq!(job_lines(&zjobs(&home, &root, &[])), 2, "precondition: 2 failed jobs recorded");

    // The bug: a negative/zero -n would dump ALL jobs. Clamped to >=1 → exactly 1.
    assert_eq!(job_lines(&zjobs(&home, &root, &["-n", "-1"])), 1, "`-n -1` must clamp to 1, not dump the ledger");
    assert_eq!(job_lines(&zjobs(&home, &root, &["-n", "0"])), 1, "`-n 0` must clamp to 1");

    let _ = std::fs::remove_dir_all(&root);
}
