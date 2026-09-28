//! Clustered short options on `git fetch`.
//!
//! `parse_short_opt()` reads a word such as `-pv` one character at a time, and
//! a value-taking switch swallows the rest of its word: `-j2` is `--jobs 2`,
//! `-oopt` is `--server-option opt` (parse-options.c:47-62, 426-461). An
//! unknown character is ``error: unknown switch `x'`` with the usage block,
//! exit 129, and a bad `-j` value names the switch, not `jobs`. zvcs matched
//! whole words only and refused every cluster as `unsupported option`.
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
    /// `up` has `a-b` on `main`; `work` clones it and copies `origin/main` to
    /// branch `copy`; then `up` rewinds `main` to `a` and commits `c` on it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-short-clusters-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["branch", "copy", "origin/main"]);
        f.run_in(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "c"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
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

    fn rev(&self, dir: &str, rev: &str) -> String {
        self.run_in(&self.root.join(dir), &["rev-parse", rev]).0.trim_end().to_owned()
    }

}

#[test]
fn a_cluster_of_switches_is_each_switch() {
    let f = Fixture::new("flags");
    // `-p` prunes, `-v` makes the unchanged-ref rows visible, `-q` wins in `-vq`.
    f.run_in(&f.root.join("up"), &["branch", "extra"]);
    f.run(&["fetch", "-q"]);
    f.run_in(&f.root.join("up"), &["branch", "-D", "extra"]);
    let (out, err, code) = f.run(&["fetch", "-pv"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(err.contains(" - [deleted]         (none)     -> origin/extra\n"), "{err}");
    assert!(err.contains(" = [up to date]      main       -> origin/main\n"), "{err}");
    assert_eq!(f.run(&["fetch", "-vq"]), (String::new(), String::new(), 0));
}

#[test]
fn a_value_switch_takes_the_rest_of_its_word() {
    let f = Fixture::new("values");
    f.run(&["remote", "add", "two", "../up"]);
    let (out, _, code) = f.run(&["fetch", "-j2", "--all", "-q"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(f.rev("work", "two/main"), f.rev("up", "main"));
    assert_eq!(
        f.run(&["fetch", "-j2x", "--all"]),
        (
            String::new(),
            "error: switch `j' expects an integer value with an optional k/m/g suffix\n".into(),
            129
        )
    );
}

#[test]
fn an_unknown_character_is_an_unknown_switch() {
    let f = Fixture::new("unknown");
    let (out, err, code) = f.run(&["fetch", "-vx"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(
        err.starts_with("error: unknown switch `x'\nusage: git fetch [<options>] [<repository> [<refspec>...]]\n"),
        "{err}"
    );
}
