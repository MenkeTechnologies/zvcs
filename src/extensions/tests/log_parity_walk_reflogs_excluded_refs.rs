//! `log -g --not --all` walked the reflogs instead of refusing them.
//!
//! Under `-g`, `add_pending_object_with_path()` hands every commit a ref-set
//! option pends to `add_reflog_for_walk()` (revision.c:305-318), and that dies
//! on an UNINTERESTING one with the name `handle_one_ref()` gave it:
//! `die("cannot walk reflogs for %s", name)` (reflog-walk.c:165-166). So
//! `--not --all` names the first ref in full, `--not --branches` its short
//! name. zvcs refused only an excluded operand typed out, and treated the
//! excluded ref sets as an ordinary `^` range.
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
    /// `main` with one commit and `topic` one commit ahead of it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-walk-reflogs-excluded-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "A"]);
        f.run(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(f.work.join("t"), "t\n").unwrap();
        f.run(&["add", "t"]);
        f.run(&["commit", "-q", "-m", "T"]);
        f.run(&["checkout", "-q", "main"]);
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
fn an_excluded_ref_set_is_refused_by_its_first_name() {
    let f = Fixture::new("sets");
    for (args, name) in [
        (&["log", "-g", "--not", "--all"][..], "refs/heads/main"),
        (&["log", "-g", "--not", "--branches"], "main"),
        (&["log", "-g", "main", "--not", "--glob=refs/heads/t*"], "refs/heads/topic"),
    ] {
        let want = format!("fatal: cannot walk reflogs for {name}\n");
        assert_eq!(f.run(args), (String::new(), want, 128), "{args:?}");
    }
    // Without `-g` the same exclusion is an ordinary range.
    let (out, _, code) = f.run(&["log", "--format=%s", "topic", "--not", "--branches=m*"]);
    assert_eq!((out.as_str(), code), ("T\n", 0));
}
