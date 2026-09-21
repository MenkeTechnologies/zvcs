//! Where `--whitespace=error` actually stops a `git apply`, which is not where the
//! option's name suggests.
//!
//! `die_on_ws_error` does exactly one thing inside `apply_patch()`:
//! `if (state->whitespace_error && ...) state->apply = 0;` (apply.c:4942). It
//! clears `apply` and leaves `check` alone, so `apply.c:4962`'s
//! `if (state->check || state->apply)` still holds under `--check` and
//! `check_patch_list()` runs in full. The verdict — `%d line adds whitespace
//! errors.` and the 128 — is printed much later, by `apply_all_patches()`
//! (apply.c:5151-5157).
//!
//! The consequence is an ordering that only shows up when *both* things are wrong
//! at once: a failing `check_patch_list()` makes `apply_patch()` return -1
//! (apply.c:4968-4970), and `apply_all_patches()` jumps straight to its exit on a
//! negative result (apply.c:5129) — past the whitespace block entirely. Such a run
//! reports the patch's own failure and exits **1**, never mentioning whitespace.
//!
//! Short-circuiting on the whitespace count as soon as it is known gets every one
//! of these cases' *clean* half right and this one wrong: it reports a whitespace
//! verdict at 128 for a patch that does not apply at all, hiding the real reason
//! and changing the exit code a caller branches on.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/usr/local/bin/git`, not the port).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A one-line change that adds a trailing space — one whitespace error, on the
/// patch's line 7. That space is written `\x20` so no editor, formatter or
/// patch-of-this-file can silently take it away; it is the entire point.
const TRAILING_WS: &str = concat!(
    "diff --git a/f.txt b/f.txt\n",
    "--- a/f.txt\n",
    "+++ b/f.txt\n",
    "@@ -1,3 +1,3 @@\n",
    " a\n",
    "-b\n",
    "+B\x20\n",
    " c\n",
);

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
        let root = std::env::temp_dir().join(format!("zvcs-applywsg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", "a\nb\nc\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        std::fs::write(f.root.join("p.patch"), TRAILING_WS.as_bytes()).unwrap();
        f
    }

    /// Move `f.txt` away from what the patch's context expects, so the hunk cannot
    /// be placed.
    fn break_context(&self) {
        self.write("f.txt", "a\nZZ\nc\n");
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body.as_bytes()).unwrap();
    }

    fn read(&self, path: &str) -> String {
        String::from_utf8(std::fs::read(self.work.join(path)).unwrap()).unwrap()
    }

    fn apply(&self, args: &[&str]) -> (i32, String) {
        let patch = self.root.join("p.patch");
        let patch = patch.to_str().unwrap();
        let mut argv = vec!["apply"];
        argv.extend_from_slice(args);
        argv.push(patch);
        let out = self.cmd(&argv).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The half everyone gets right, kept as the control for the two below: a patch
/// that *would* apply, refused for its whitespace alone. Stock, measured:
///   p.patch:7: trailing whitespace.
///   B
///   error: 1 line adds whitespace errors.
/// at exit 128, with `f.txt` untouched — both under `--check` and on a plain run.
#[test]
fn whitespace_error_refuses_an_otherwise_applicable_patch_at_128() {
    let f = Fixture::new("clean");

    let (code, err) = f.apply(&["--check", "--whitespace=error"]);
    assert_eq!(code, 128, "{err:?}");
    assert!(
        err.ends_with("error: 1 line adds whitespace errors.\n"),
        "{err:?}"
    );
    assert!(err.contains("p.patch:7: trailing whitespace."), "{err:?}");

    let (code, err) = f.apply(&["--whitespace=error"]);
    assert_eq!(code, 128, "{err:?}");
    assert!(
        err.ends_with("error: 1 line adds whitespace errors.\n"),
        "{err:?}"
    );
    assert_eq!(f.read("f.txt"), "a\nb\nc\n", "nothing was written");
}

/// apply.c:4962 vs :5129. Under `--check` the check still runs, it fails, and the
/// whitespace verdict is never reached. Stock, measured:
///   p.patch:7: trailing whitespace.
///   B
///   error: patch failed: f.txt:1
///   error: f.txt: patch does not apply
/// at exit **1** — not 128, and with no `adds whitespace errors` line anywhere.
#[test]
fn a_failed_check_outranks_the_whitespace_verdict_and_exits_one() {
    let f = Fixture::new("failedcheck");
    f.break_context();

    let (code, err) = f.apply(&["--check", "--whitespace=error"]);
    assert_eq!(code, 1, "{err:?}");
    assert!(
        err.contains("error: patch failed: f.txt:1")
            && err.contains("error: f.txt: patch does not apply"),
        "the patch's own failure is what is reported: {err:?}"
    );
    assert!(
        !err.contains("adds whitespace errors"),
        "apply.c:5129 jumps past the whitespace block on a negative result: {err:?}"
    );
    // The per-line warning still comes out: it is recorded at parse time
    // (`record_ws_error()`, apply.c:1682), long before any of this.
    assert!(err.contains("p.patch:7: trailing whitespace."), "{err:?}");
}

/// The same input *without* `--check`. `state->apply` is now 0 for the whitespace
/// reason and `state->check` was never set, so apply.c:4962 is false: git does not
/// check the patch at all and the whitespace verdict is what comes out. Stock,
/// measured: exit 128 with `error: 1 line adds whitespace errors.` and no
/// `patch does not apply` line — for a patch that plainly does not apply.
///
/// This is the case that pins the gate to `state->apply`/`state->check` rather than
/// to "did the patch work": the same patch and the same tree give two different
/// exit codes depending only on `--check`.
#[test]
fn without_check_the_same_unappliable_patch_reports_whitespace_at_128() {
    let f = Fixture::new("nocheck");
    f.break_context();

    let (code, err) = f.apply(&["--whitespace=error"]);
    assert_eq!(code, 128, "{err:?}");
    assert!(
        err.ends_with("error: 1 line adds whitespace errors.\n"),
        "{err:?}"
    );
    assert!(
        !err.contains("patch does not apply"),
        "no check ran, so nothing reported the patch: {err:?}"
    );
    assert_eq!(f.read("f.txt"), "a\nZZ\nc\n", "nothing was written");
}

/// The control for the exit code: with any other whitespace action the check runs
/// and fails the same way, at 1. Stock, measured: identical stderr to the
/// `--check --whitespace=error` case above, which is the point — `error` differs
/// from `warn` only on a run that gets past the check.
#[test]
fn a_non_fatal_whitespace_action_fails_the_same_patch_identically() {
    let f = Fixture::new("warn");
    f.break_context();

    let (code, err) = f.apply(&["--check", "--whitespace=warn"]);
    assert_eq!(code, 1, "{err:?}");
    assert!(
        err.contains("error: patch failed: f.txt:1")
            && err.contains("error: f.txt: patch does not apply"),
        "{err:?}"
    );
    assert!(!err.contains("adds whitespace errors"), "{err:?}");
}
