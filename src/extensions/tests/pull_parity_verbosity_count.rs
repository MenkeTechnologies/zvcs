//! `git pull`'s verbosity is one signed count.
//!
//! `OPT__VERBOSITY(&opt_verbosity)` goes through `parse_opt_verbosity_cb()`
//! (parse-options-cb.c:65-85): `-v` after `-q` resets the count to 1 and `-q`
//! after `-v` to -1, and `argv_push_verbosity()` (builtin/pull.c:125-134)
//! hands the result to the fetch and the merge as that many `-v` or `-q`. So
//! `pull -q -v` is verbose and `pull -v -q` quiet. zvcs kept two independent
//! flags and forwarded both, so `pull -q -v` silenced the fetch.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// `up` has `a` on `main`, `work` clones it, then `up` adds `b`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pull-verbosity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn short(&self, dir: &str, rev: &str) -> String {
        self.run_in(&self.root.join(dir), &["rev-parse", "--short", rev]).0.trim_end().to_owned()
    }

    fn url(&self) -> String {
        std::fs::canonicalize(self.root.join("up")).unwrap().display().to_string()
    }
}

#[test]
fn a_later_verbose_overrides_an_earlier_quiet() {
    let f = Fixture::new("qv");
    let (old, new) = (f.short("work", "HEAD"), f.short("up", "main"));
    for argv in [&["pull", "-q", "-v"][..], &["pull", "-qv"][..]] {
        let f = Fixture::new(&argv.len().to_string());
        assert_eq!(
            f.run(argv),
            (
                format!("Updating {old}..{new}\nFast-forward\n"),
                format!("From {}\n   {old}..{new}  main       -> origin/main\n", f.url()),
                0
            )
        );
    }
}

#[test]
fn a_later_quiet_overrides_an_earlier_verbose() {
    let f = Fixture::new("vq");
    assert_eq!(f.run(&["pull", "-v", "-q"]), (String::new(), String::new(), 0));
}
