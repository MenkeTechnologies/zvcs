//! `git status` honors `status.short` (default to the short format) and
//! `status.branch` (default to the `## <branch>` header), with the command line
//! still overriding (`--long`, `--no-branch`). Regression guard for the status
//! command ignoring these presentation defaults.

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
    let root = std::env::temp_dir().join(format!("zvcs-statuscfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("tracked"), "hello\n").unwrap();
    git(&repo, &["add", "tracked"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    // An untracked file so both short and long output are non-empty.
    std::fs::write(repo.join("untracked"), "x\n").unwrap();
    (repo, home)
}

fn zvcs_status(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run(BIN, repo, home, extra)
}

fn real_status(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run("git", repo, home, extra)
}

fn run(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["status"];
    args.extend_from_slice(extra);
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn status_short_config_and_override() {
    let (repo, home) = fixture("short");

    // Default (no config): the long format.
    let d = out(&zvcs_status(&repo, &home, &[]));
    assert!(d.contains("Untracked files:"), "default is the long format:\n{d}");

    // status.short=true → the short format, byte-identical to git's.
    git(&repo, &["config", "status.short", "true"]);
    let z = out(&zvcs_status(&repo, &home, &[]));
    let g = out(&real_status(&repo, &home, &[]));
    assert_eq!(z, g, "status.short output must match git");
    assert!(z.contains("?? untracked"), "short format lists the untracked file:\n{z}");

    // --long overrides status.short.
    let z = out(&zvcs_status(&repo, &home, &["--long"]));
    assert!(z.contains("Untracked files:"), "--long must override status.short:\n{z}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn status_branch_config_and_override() {
    let (repo, home) = fixture("branch");
    git(&repo, &["config", "status.short", "true"]);

    // status.branch=true adds the `## main` header to the short format.
    git(&repo, &["config", "status.branch", "true"]);
    let z = out(&zvcs_status(&repo, &home, &[]));
    let g = out(&real_status(&repo, &home, &[]));
    assert_eq!(z, g, "status.branch output must match git");
    assert!(z.starts_with("## main"), "status.branch adds the header:\n{z}");

    // --no-branch overrides status.branch.
    let z = out(&zvcs_status(&repo, &home, &["--no-branch"]));
    assert!(!z.contains("## main"), "--no-branch must override status.branch:\n{z}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
