//! `commit.cleanup` naming no mode is fatal before the commit does any work.
//!
//! `parse_and_validate_options()` runs `get_cleanup_mode()` on the configured value when no
//! `--cleanup` was given, so `fatal: Invalid cleanup mode 1k` (128) comes ahead of the staging
//! and the `pre-commit` hook. zvcs resolved the config only when it built the message, so the
//! hook ran first and its refusal (exit 1) hid the real error. Expectations measured from stock
//! git 2.56.0.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-commit-cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&[], &["add", "a"]);
        f.run(&[], &["commit", "-q", "-m", "one"]);
        let hook = f.root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho ran > hook-ran\necho pre-commit refuses >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(f.root.join("a"), "changed\n").unwrap();
        f
    }

    fn run(&self, config: &[&str], args: &[&str]) -> (String, i32) {
        let mut cmd = Command::new(BIN);
        for c in config {
            cmd.args(["-c", c]);
        }
        let out = cmd
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn a_bad_configured_mode_dies_before_staging_and_the_hook() {
    let f = Fixture::new("bad");
    assert_eq!(
        f.run(&["commit.cleanup=1k"], &["commit", "-a", "-m", "x"]),
        ("fatal: Invalid cleanup mode 1k\n".to_string(), 128)
    );
    assert!(!f.root.join("hook-ran").exists(), "pre-commit ran");
}

#[test]
fn an_explicit_cleanup_overrides_the_configured_one() {
    let f = Fixture::new("override");
    let (stderr, code) = f.run(&["commit.cleanup=1k"], &["commit", "--cleanup=strip", "-a", "-m", "x"]);
    assert_eq!((stderr.as_str(), code), ("pre-commit refuses\n", 1));
    assert!(f.root.join("hook-ran").exists());
}
