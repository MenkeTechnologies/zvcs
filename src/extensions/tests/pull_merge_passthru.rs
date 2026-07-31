//! The merge options `git pull` hands to `git merge`, and how it rejects the
//! ones it does not know.
//!
//! `run_merge()` (builtin/pull.c) pushes `--[no-]commit`, `--[no-]edit`,
//! `--[no-]log[=<n>]` and `--cleanup=<mode>` onto the merge command line
//! verbatim. Refusing them in the pull front-end makes flags that work
//! everywhere else fail for no reason a user can see — the merge port
//! implements all four.
//!
//! The option *errors* are `parse-options`', not ours: an unknown option prints
//! the whole usage block and exits 129, a missing value prints one line and
//! exits 129.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    dn: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A clone whose upstream has moved one commit ahead.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-pullpass-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let f = Fixture { root: root.clone(), dn: root.join("dn") };
        f.git(&up, &["init", "-q", "-b", "main"]);
        f.git(&up, &["config", "user.email", "t@e.x"]);
        f.git(&up, &["config", "user.name", "t"]);
        std::fs::write(up.join("f"), "a\n").unwrap();
        f.git(&up, &["add", "f"]);
        f.git(&up, &["commit", "-q", "-m", "c0"]);
        f.git(&root, &["clone", "-q", up.to_str().unwrap(), f.dn.to_str().unwrap()]);
        f.git(&f.dn, &["config", "user.email", "t@e.x"]);
        f.git(&f.dn, &["config", "user.name", "t"]);
        std::fs::write(up.join("f"), "a\nb\n").unwrap();
        f.git(&up, &["add", "f"]);
        f.git(&up, &["commit", "-q", "-m", "c1"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)` of a command run in the clone.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let dn = self.dn.clone();
        let out = self.cmd(&dn, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).1.trim().to_string()
    }
}

/// `--no-commit` reaches the merge: a non-fast-forward pull leaves `MERGE_HEAD`
/// and an unmoved `HEAD` instead of being refused up front.
#[test]
fn no_commit_reaches_the_merge() {
    let f = Fixture::new("nocommit");
    let before = f.head();

    let (code, out, err) = f.run(&["pull", "--no-ff", "--no-commit"]);
    assert_eq!(code, 0, "pull failed: {out}{err}");
    assert_eq!(f.head(), before, "--no-commit must not move HEAD");
    assert!(f.dn.join(".git/MERGE_HEAD").exists(), "the merge must be left in progress");
    assert!(
        err.contains("Automatic merge went well; stopped before committing as requested"),
        "stderr: {err}"
    );
}

/// `--log`, `--edit` and `--cleanup` are forwarded as well; a fast-forward pull
/// takes them without complaint, exactly as git's does.
#[test]
fn log_edit_and_cleanup_are_forwarded() {
    for args in [
        vec!["pull", "--no-log"],
        vec!["pull", "--log=2"],
        vec!["pull", "--no-edit"],
        vec!["pull", "--cleanup=verbatim"],
    ] {
        let f = Fixture::new(&format!("fwd-{}", args[1].replace(['-', '='], "")));
        let (code, out, err) = f.run(&args);
        assert_eq!(code, 0, "`git {args:?}` failed: {out}{err}");
        assert!(out.contains("Fast-forward"), "the pull should have fast-forwarded: {out}");
    }
}

/// An option `git pull` does not have is `parse-options`' rejection: the name
/// without its dashes, the whole usage block, and exit 129 — all on stderr,
/// since only `-h` writes the usage to stdout.
#[test]
fn an_unknown_option_prints_the_usage_block_to_stderr() {
    let f = Fixture::new("unknown");

    let (code, out, err) = f.run(&["pull", "--atomic"]);
    assert_eq!(code, 129, "wrong exit: {out}{err}");
    assert!(err.starts_with("error: unknown option `atomic'\n"), "stderr: {err}");
    assert!(err.contains("usage: git pull [<options>]"), "the usage block belongs here: {err}");
    assert_eq!(out, "", "nothing may go to stdout: {out}");

    let (code, _, err) = f.run(&["pull", "-z"]);
    assert_eq!(code, 129, "wrong exit: {err}");
    assert!(err.starts_with("error: unknown switch `z'\n"), "stderr: {err}");
}

/// A value-taking option with nothing after it is `get_arg()`'s one-liner —
/// named as typed, and with no usage block.
#[test]
fn a_missing_option_value_is_reported_without_the_usage_block() {
    let f = Fixture::new("missing-value");

    let (code, out, err) = f.run(&["pull", "--depth"]);
    assert_eq!(code, 129, "wrong exit: {out}{err}");
    assert_eq!(err, "error: option `depth' requires a value\n", "stderr: {err}");

    let (code, _, err) = f.run(&["fetch", "-j"]);
    assert_eq!(code, 129, "wrong exit: {err}");
    assert_eq!(err, "error: switch `j' requires a value\n", "stderr: {err}");
}

/// `-j`/`--jobs` is an `OPT_PASSTHRU` with an optional value, so a detached one
/// is *not* consumed by the pull — the bare `--jobs` is handed to the fetch,
/// whose own `--jobs` takes it. That is why `git pull --jobs 2` fetches from
/// `origin` and not from a remote named `2`.
#[test]
fn a_detached_jobs_value_is_consumed_by_the_fetch() {
    let f = Fixture::new("jobs");
    let (code, out, err) = f.run(&["pull", "--jobs", "2"]);
    assert_eq!(code, 0, "pull failed: {out}{err}");
    assert!(out.contains("Fast-forward"), "stdout: {out}");

    // With a non-number after it, the fetch is the one that objects.
    let f = Fixture::new("jobs-bad");
    let (code, _, err) = f.run(&["pull", "--jobs", "origin"]);
    assert_eq!(code, 1, "wrong exit: {err}");
    assert_eq!(
        err,
        "error: option `jobs' expects an integer value with an optional k/m/g suffix\n",
        "stderr: {err}"
    );
}

/// `--depth`'s value is checked by `cmd_fetch()` rather than by parse-options,
/// which is why a bad one is a `fatal:` and exits 128.
#[test]
fn a_bad_depth_is_a_fatal_not_a_usage_error() {
    let f = Fixture::new("depth");
    let (code, out, err) = f.run(&["fetch", "--depth", "x"]);
    assert_eq!(code, 128, "wrong exit: {out}{err}");
    assert_eq!(err, "fatal: depth x is not a positive number\n", "stderr: {err}");

    let (code, _, err) = f.run(&["fetch", "--depth", "0"]);
    assert_eq!(code, 128, "wrong exit: {err}");
    assert_eq!(err, "fatal: depth 0 is not a positive number\n", "stderr: {err}");
}
