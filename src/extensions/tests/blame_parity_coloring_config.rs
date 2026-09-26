//! `blame.coloring` warnings, one per configured value.
//!
//! `git_blame_config()` handles `blame.coloring` once per configured value, in
//! order, and an unknown one is `warning(_("invalid value for '%s': '%s'"),
//! "blame.coloring", value)` without stopping the command
//! (builtin/blame.c:770-785). zvcs read the values off the merged snapshot,
//! which holds a `-c blame.coloring=<v>` twice, so every command-line value
//! warned twice.
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
    /// `file` = `one\n` in a first commit, `two\n` appended in a second.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-blame-coloring-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("file"), "one\ntwo\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"]);
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

#[test]
fn each_bad_value_warns_once() {
    let f = Fixture::new("warn");
    let (out, err, code) = f.run(&[
        "-c",
        "blame.coloring=bogus",
        "-c",
        "blame.coloring=x",
        "blame",
        "-s",
        "file",
    ]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: invalid value for 'blame.coloring': 'bogus'\n\
             warning: invalid value for 'blame.coloring': 'x'\n",
            0
        )
    );
    assert_eq!(out.lines().count(), 2);

    // A file value is walked first, then the command line's.
    f.run(&["config", "blame.coloring", "bad"]);
    let (_, err, code) = f.run(&["-c", "blame.coloring=bogus", "blame", "-s", "file"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: invalid value for 'blame.coloring': 'bad'\n\
             warning: invalid value for 'blame.coloring': 'bogus'\n",
            0
        )
    );
}
