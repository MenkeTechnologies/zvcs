//! `interpolate_path()` hands a `%(prefix)/` value to `system_path()`
//! (path.c:706-707), which always answers (exec-cmd.c:290-299), so
//! `git_config_pathname()` never dies on that form. The port's pathname reader
//! returned "no expansion" for it and died with `failed to expand user dir`,
//! which ended a `checkout` under `core.hooksPath=%(prefix)/x` at 128 after the
//! branch had already been switched; stock git 2.55.0 exits 0.
//!
//! The prefix itself is the installation's, so it differs between stock (its
//! compiled-in `prefix`) and this port (`$HOME/.zvcs`). The expansions asserted
//! below are this port's; their shape — absolute paths passed through, an empty
//! tail keeping its slash, a missing `:(optional)` path unset — is measured
//! against stock.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-prefixpath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-qm", "m"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_EXEC_PATH")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.work, args)
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn prefix(&self) -> String {
        format!("{}/.zvcs", self.root.display())
    }
}

#[test]
fn checkout_under_a_prefix_hooks_path_does_not_die() {
    let f = Fixture::new("checkout");
    f.ok(&["branch", "side"]);
    f.ok(&["config", "core.hooksPath", "%(prefix)/x"]);
    let out = f.run(&["checkout", "side"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("Switched to branch 'side'"), "stderr: {stderr}");
    assert!(!stderr.contains("failed to expand user dir"), "stderr: {stderr}");
}

#[test]
fn config_type_path_resolves_the_prefix_form() {
    let f = Fixture::new("typepath");
    let get = |value: &str| {
        let arg = format!("a.p={value}");
        f.run(&["-c", &arg, "config", "--type=path", "a.p"])
    };

    let out = get("%(prefix)/x");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{}/x\n", f.prefix()));

    // `system_path()` returns an absolute path as it stands.
    let out = get("%(prefix)//abs");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/abs\n");

    // The join is `"%s/%s"`, so an empty tail keeps the slash.
    let out = get("%(prefix)/");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{}/\n", f.prefix()));

    // A missing `:(optional)` path is unset: nothing printed, exit 1.
    let out = get(":(optional)%(prefix)/nope");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn optional_relative_path_is_looked_for_from_the_work_tree_top() {
    // Measured against git 2.55.0: setup moves to the top of the work tree before
    // `git config` runs, so `is_missing_file()` resolves a relative name from
    // there, and a file at the top counts as present from a subdirectory.
    let f = Fixture::new("optbase");
    std::fs::write(f.work.join("topfile"), "").unwrap();
    let sub = f.work.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let out = f.run_in(&sub, &["-c", "a.p=:(optional)topfile", "config", "--type=path", "a.p"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "topfile\n");

    let out = f.run_in(&sub, &["-c", "a.p=:(optional)nothere", "config", "--type=path", "a.p"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}
