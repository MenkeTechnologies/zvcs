//! The entry point must consume leading git-global options so `git -C <dir>
//! <zverb>` reaches the verb (running in <dir>) instead of treating `-C` as the
//! subcommand.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

#[test]
fn dash_c_runs_verb_in_the_named_directory() {
    let root = std::env::temp_dir().join(format!("zvcs-dashc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "c0"]);

    // Run from `root` (NOT a repo) but target `repo` via -C. Without -C handling
    // this treats "-C" as the subcommand and errors.
    let out = Command::new(BIN)
        .args(["-C", repo.to_str().unwrap(), "zstatus"])
        .current_dir(&root)
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "git -C <repo> zstatus failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("main"), "zstatus should report the repo at -C dir:\n{stdout}");

    // A pager no-op flag before the verb is also tolerated.
    let ok = Command::new(BIN)
        .args(["--no-pager", "-C", repo.to_str().unwrap(), "zstatus"])
        .current_dir(&root)
        .env("ZVCS_HOME", &home)
        .status()
        .unwrap()
        .success();
    assert!(ok, "--no-pager -C <repo> zstatus should succeed");

    let _ = std::fs::remove_dir_all(&root);
}

/// Faithful port of `cmd_main()` in git.c: the top-level `-v`/`--version` flags
/// are rewritten to the `version` subcommand before dispatch, so `git --version`
/// prints the version instead of erroring "not yet ported". Regression guard for
/// the missing rewrite that made `git --version` fail.
#[test]
fn top_level_version_flag_prints_version() {
    for flag in ["--version", "-v"] {
        let out = Command::new(BIN).arg(flag).output().unwrap();
        assert!(out.status.success(), "git {flag} failed: {}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("git version "), "git {flag} should print the version:\n{stdout}");
    }

    // The rewrite happens after global-option consumption, so a leading global
    // still reaches it: `git -C <dir> --version` prints the version too.
    let out = Command::new(BIN).args(["-C", ".", "--version"]).output().unwrap();
    assert!(out.status.success(), "git -C . --version failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version "));
}

/// `-h`/`--help` are likewise rewritten to the `help` subcommand (git.c), which
/// prints the top-level usage banner rather than erroring.
#[test]
fn top_level_help_flag_prints_usage() {
    for flag in ["--help", "-h"] {
        let out = Command::new(BIN).arg(flag).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("usage: git"), "git {flag} should print usage:\n{stdout}");
    }
}
