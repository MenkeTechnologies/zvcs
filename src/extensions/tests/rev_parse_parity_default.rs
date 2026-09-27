//! `git rev-parse --default <rev>` (builtin/rev-parse.c:832-837) records `def`,
//! and three places print it:
//!
//! * `show_file()` calls `show_default()` before it echoes a path, `--` or
//!   `--end-of-options` (:253-256), so the default lands *ahead* of the first
//!   non-revision token — even when the filter keeps that token off stdout;
//! * `--verify` with no revision falls back to it (:1192-1195);
//! * otherwise it is printed at the end (:1197-1198).
//!
//! Every `show_rev()` that passes the `DO_REVS` filter clears `def` (:146-148),
//! so a revision given anywhere suppresses it, and a default that does not
//! resolve is silently dropped (`show_default()`, :205-218). zvcs refused the
//! option as "not ported yet".
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-default-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "a\n");
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args, None)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or_default()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

impl Fixture {
    /// `main` is `one` → `two`; `side` stays on `one`.
    fn with_side(tag: &str) -> (Self, String, String) {
        let f = Fixture::new(tag);
        f.write("a", "a2\n");
        f.run(&["commit", "-q", "-am", "two"]);
        f.run(&["branch", "side", "HEAD~1"]);
        let head = f.run(&["rev-parse", "HEAD"]).0.trim().to_string();
        let one = f.run(&["rev-parse", "side"]).0.trim().to_string();
        (f, head, one)
    }
}

#[test]
fn the_default_stands_in_only_when_no_revision_was_shown() {
    let (f, head, one) = Fixture::with_side("fallback");
    assert_eq!(f.run(&["rev-parse", "--default", "side"]), (format!("{one}\n"), String::new(), 0));
    assert_eq!(f.run(&["rev-parse", "--default", "side", "HEAD"]), (format!("{head}\n"), String::new(), 0));
    // The last `--default` wins, and one that does not resolve prints nothing.
    assert_eq!(
        f.run(&["rev-parse", "--default", "side", "--default", "HEAD"]),
        (format!("{head}\n"), String::new(), 0)
    );
    assert_eq!(f.run(&["rev-parse", "--default", "nope"]), (String::new(), String::new(), 0));
    assert_eq!(
        f.run(&["rev-parse", "--default"]),
        (String::new(), "fatal: --default requires an argument\n".into(), 128)
    );
}

#[test]
fn the_default_goes_out_before_the_first_path_token() {
    let (f, _, one) = Fixture::with_side("paths");
    assert_eq!(f.run(&["rev-parse", "--default", "side", "a"]), (format!("{one}\na\n"), String::new(), 0));
    assert_eq!(
        f.run(&["rev-parse", "--default", "side", "--", "a"]),
        (format!("{one}\n--\na\n"), String::new(), 0)
    );
    // `show_file()` still calls `show_default()` when `--no-revs` hides the
    // revision, and `show_rev()`'s filter then swallows it.
    assert_eq!(f.run(&["rev-parse", "--no-revs", "--default", "side", "a"]), ("a\n".into(), String::new(), 0));
}

#[test]
fn verify_falls_back_to_the_default_only_with_no_revision() {
    let (f, head, one) = Fixture::with_side("verify");
    assert_eq!(f.run(&["rev-parse", "--verify", "--default", "side"]), (format!("{one}\n"), String::new(), 0));
    assert_eq!(
        f.run(&["rev-parse", "--default", "side", "--verify", "HEAD"]),
        (format!("{head}\n"), String::new(), 0)
    );
    assert_eq!(
        f.run(&["rev-parse", "--default", "side", "--verify", "HEAD", "HEAD"]),
        (String::new(), "fatal: Needed a single revision\n".into(), 128)
    );
    assert_eq!(f.run(&["rev-parse", "--verify", "-q", "--default", "nope"]), (String::new(), String::new(), 1));
}
