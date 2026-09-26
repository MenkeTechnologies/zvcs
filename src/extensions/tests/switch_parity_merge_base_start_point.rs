//! `switch` resolves its operand with `repo_get_oid_mb()`.
//!
//! `parse_branchname_arg()` resolves the start-point of `-c`/`-C` and the
//! target of `--detach` with `repo_get_oid_mb()` (builtin/checkout.c:1476),
//! where `<a>...<b>` is the single merge base of the two sides and an empty side
//! means `HEAD` (object-name.c:1308-1353). zvcs used the plain revision parser,
//! so every `...` operand was `fatal: invalid reference: <a>...<b>`.
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
    /// `main` is `base` then `second`; `s2` stays on `base`, the merge base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-switch-merge-base-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("file"), "second\n").unwrap();
        f.run(&["commit", "-q", "-am", "second"]);
        f.run(&["branch", "s2", "HEAD~1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.work.join("file")).unwrap()
    }

    fn subject(&self) -> String {
        self.run(&["log", "-1", "--format=%s"]).0
    }
}

#[test]
fn create_starts_at_the_merge_base() {
    let f = Fixture::new("create");
    let got = f.run(&["switch", "-c", "b", "s2...main"]);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", "Switched to a new branch 'b'\n", 0));
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/b\n");
    assert_eq!((f.file().as_str(), f.subject().as_str()), ("base\n", "base\n"));
}

#[test]
fn force_create_with_an_empty_side_means_head() {
    let f = Fixture::new("force");
    let got = f.run(&["switch", "-C", "main", "s2..."]);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", "Reset branch 'main'\n", 0));
    assert_eq!((f.file().as_str(), f.subject().as_str()), ("base\n", "base\n"));
}

#[test]
fn detach_lands_on_the_merge_base() {
    let f = Fixture::new("detach");
    let got = f.run(&["switch", "-d", "s2...main"]);
    assert_eq!(got.2, 0);
    assert!(got.1.starts_with("HEAD is now at ") && got.1.ends_with(" base\n"), "{got:?}");
    assert_eq!(f.run(&["symbolic-ref", "-q", "HEAD"]).2, 1);
    assert_eq!(f.file(), "base\n");
}

#[test]
fn explicit_track_refuses_the_merge_base_as_no_branch() {
    let f = Fixture::new("track");
    let got = f.run(&["switch", "-c", "b", "--track", "s2...main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (
            "",
            "fatal: cannot set up tracking information; starting point 's2...main' is not a branch\n",
            128
        )
    );
    // The worktree had already moved to the merge base; the branch was never made.
    assert_eq!((f.file().as_str(), f.subject().as_str()), ("base\n", "second\n"));
    assert_eq!(f.run(&["rev-parse", "-q", "--verify", "refs/heads/b"]).2, 1);
}

#[test]
fn orphan_refuses_a_merge_base_start_point() {
    let f = Fixture::new("orphan");
    let got = f.run(&["switch", "--orphan", "o", "s2...main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        ("", "fatal: '--orphan' cannot take <start-point>\n", 128)
    );
}
