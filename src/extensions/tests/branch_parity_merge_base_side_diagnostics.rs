//! The two sides of an `<a>...<b>` start-point are resolved out loud.
//!
//! `repo_get_oid_mb()` resolves each side with `repo_get_oid_committish()` and
//! then `lookup_commit_reference_gently(r, &oid_tmp, 0)` (object-name.c:1325-1339):
//! the first warns `refname '<x>' is ambiguous` like any other resolution, the
//! second is not quiet and reports a side that names a tree. zvcs resolved both
//! sides silently, so `git branch b amb...main` printed nothing, `git switch -c`
//! (which resolves the operand twice) printed neither of git's two warnings, and
//! `HEAD^{tree}...main` failed without the `is a tree, not a commit` line.
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
    /// `main` is `base` then `second`; `amb` is both a branch and a tag on `base`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-branch-mb-diagnostics-{tag}-{}", std::process::id()));
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
        f.run(&["branch", "amb", "HEAD~1"]);
        f.run(&["tag", "amb", "HEAD~1"]);
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
}

const AMBIGUOUS: &str = "warning: refname 'amb' is ambiguous.\n";

#[test]
fn branch_warns_for_an_ambiguous_side() {
    let f = Fixture::new("branch");
    let got = f.run(&["branch", "b", "amb...main"]);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", AMBIGUOUS, 0));
    let base = f.run(&["rev-parse", "main~1"]).0;
    assert_eq!(f.run(&["rev-parse", "b"]).0, base);
}

#[test]
fn switch_warns_once_per_resolution() {
    let f = Fixture::new("switch");
    let got = f.run(&["switch", "-c", "b", "amb...main"]);
    let want = format!("{AMBIGUOUS}{AMBIGUOUS}Switched to a new branch 'b'\n");
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", want.as_str(), 0));
}

#[test]
fn a_tree_side_is_reported_before_the_die() {
    let f = Fixture::new("tree");
    let tree = f.run(&["rev-parse", "HEAD^{tree}"]).0;
    let got = f.run(&["branch", "b", "HEAD^{tree}...main"]);
    let want = format!(
        "error: object {} is a tree, not a commit\n\
         fatal: not a valid object name: 'HEAD^{{tree}}...main'\n",
        tree.trim_end()
    );
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", want.as_str(), 128));
}
