//! `git show --full-diff`.
//!
//! `--full-diff` sets `revs->diff` and `revs->full_diff` (revision.c:2654-2656),
//! and `setup_revisions()` then keeps the pathspec to the walk instead of copying
//! it into `revs->diffopt.pathspec` (revision.c:3160-3167): each record's diff
//! covers the whole tree. `-L` refuses it with the unsupported formats
//! (revision.c:3206-3212). zvcs refused the option outright.
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
    /// `base` adds `a` and `b`; `both` changes the two.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-show-full-diff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a"), "a2\n").unwrap();
        std::fs::write(f.work.join("b"), "b2\n").unwrap();
        f.run(&["commit", "-q", "-am", "both"]);
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
            .env("GIT_PAGER", "cat")
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

#[test]
fn the_pathspec_no_longer_limits_the_diff() {
    let f = Fixture::new("limit");
    let (out, err, code) = f.run(&["show", "--format=%s", "--stat", "--", "a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("both\n\n a | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n", "", 0)
    );
    let (out, err, code) = f.run(&["show", "--format=%s", "--stat", "--full-diff", "--", "a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("both\n\n a | 2 +-\n b | 2 +-\n 2 files changed, 2 insertions(+), 2 deletions(-)\n", "", 0)
    );
    let (out, err, code) = f.run(&["show", "--format=%s", "--name-only", "--full-diff", "HEAD~1", "--", "b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("base\n\na\nb\n", "", 0));
}

#[test]
fn line_ranges_refuse_it() {
    let f = Fixture::new("line");
    let (out, err, code) = f.run(&["show", "--full-diff", "-L1,1:a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: -L does not yet support the requested diff format\n", 128)
    );
}
