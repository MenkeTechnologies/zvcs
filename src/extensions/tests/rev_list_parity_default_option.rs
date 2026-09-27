//! `--default <rev>` was refused by `log` and a usage error in `rev-list`.
//!
//! `handle_revision_opt()` stores the next argument as `revs->def`, and a
//! missing one is `error("bad --default argument")` — fatal to the caller
//! (revision.c:2429-2433). `setup_revisions()` pends it, under its own name and
//! with no path, only when nothing else was pended and no revision was named
//! (revision.c:3125-3133); a name that does not resolve goes through
//! `diagnose_missing_default()` (revision.c:2985-2998).
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
    /// A then B on `main`; `side` adds S on A.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-default-option-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], 0);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"], 0);
        f.run(&["commit", "-q", "-m", "A"], 0);
        f.run(&["checkout", "-q", "-b", "side"], 10);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"], 10);
        f.run(&["commit", "-q", "-m", "S"], 10);
        f.run(&["checkout", "-q", "main"], 20);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"], 20);
        f.run(&["commit", "-q", "-m", "B"], 20);
        f
    }

    fn run(&self, args: &[&str], at: u64) -> (String, String, i32) {
        let date = format!("@{} +0000", 1_700_000_000 + at);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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
fn the_default_stands_in_only_when_nothing_was_named() {
    let f = Fixture::new("stand-in");
    assert_eq!(f.run(&["log", "--format=%s", "--default", "side"], 0).0, "S\nA\n");
    assert_eq!(f.run(&["rev-list", "--count", "--default", "side"], 0).0, "2\n");
    // Named revisions win, and so does an exclusion that leaves nothing.
    assert_eq!(f.run(&["log", "--format=%s", "--default", "side", "main"], 0).0, "B\nA\n");
    assert_eq!(f.run(&["rev-list", "--default", "side", "--not", "main"], 0), (String::new(), String::new(), 0));
    // Pended with no path: a blob default is listed with an empty name.
    let blob = f.run(&["rev-parse", "main:a"], 0).0;
    let out = f.run(&["rev-list", "--objects", "--default", "main:a"], 0);
    assert_eq!(out, (format!("{} \n", blob.trim_end()), String::new(), 0));
}

#[test]
fn a_bad_default_is_fatal() {
    let f = Fixture::new("bad");
    for verb in ["log", "rev-list"] {
        let out = f.run(&[verb, "--default"], 0);
        assert_eq!(out, (String::new(), "error: bad --default argument\n".into(), 128), "{verb}");
        let out = f.run(&[verb, "--default", "nosuch"], 0);
        assert_eq!(
            out,
            (String::new(), "fatal: your current branch appears to be broken\n".into(), 128),
            "{verb}"
        );
    }
    f.run(&["symbolic-ref", "refs/heads/unborn", "refs/heads/nope"], 0);
    let out = f.run(&["rev-list", "--default", "refs/heads/unborn"], 0);
    assert_eq!(out.2, 128);
    assert!(out.1.ends_with("fatal: your current branch 'nope' does not have any commits yet\n"), "{}", out.1);
}
