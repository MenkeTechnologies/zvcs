//! `zforeach` reports per-repo failures and exits non-zero when the command
//! fails in any repo.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

#[test]
fn zforeach_reports_failures_and_exits_nonzero() {
    let root = std::env::temp_dir().join(format!("zvcs-fefail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    }
    assert!(Command::new(BIN).args(["zreindex", "--sync", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).status().unwrap().success());

    // A command that fails in every repo (unknown ref).
    let out = Command::new(BIN)
        .args(["zforeach", "--", "git", "rev-parse", "--verify", "--quiet", "nope-no-such-ref"])
        .current_dir(&root)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "zforeach must exit non-zero when a command fails");
    assert!(stderr.contains("2 failed") || stderr.contains("failed"), "summary should report failures:\n{stderr}");

    // A command that succeeds everywhere → exit zero.
    let ok = Command::new(BIN)
        .args(["zforeach", "--", "git", "rev-parse", "HEAD"])
        .current_dir(&root)
        .env("ZVCS_HOME", &home)
        .status()
        .unwrap()
        .success();
    assert!(ok, "zforeach must exit zero when all commands succeed");

    let _ = std::fs::remove_dir_all(&root);
}
