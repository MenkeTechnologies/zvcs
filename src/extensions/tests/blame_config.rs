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
