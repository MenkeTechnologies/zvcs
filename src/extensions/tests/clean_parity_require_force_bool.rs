//! `clean.requireForce` read as a strict boolean.
//!
//! `git_clean_config()` reads the key with `git_config_bool()`
//! (builtin/clean.c:132-133), which dies on a value `git_parse_maybe_bool()`
//! refuses while the configuration is being read. So an unparseable value is
//! fatal for every `git clean` — `-n` and `-f` included — and is reported
//! instead of the `-f not given` refusal. zvcs treated anything that was not
//! `false` as "require force", printed the refusal, and ran `-n`/`-f` normally.
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
    /// One tracked file and one untracked `junk` file.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clean-require-force-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("junk"), "junk\n").unwrap();
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

const FATAL: &str = "fatal: bad boolean config value 'bogus' for 'clean.requireforce'\n";

#[test]
fn a_bogus_value_is_fatal_instead_of_the_refusal() {
    let f = Fixture::new("plain");
    let got = f.run(&["-c", "clean.requireForce=bogus", "clean"]);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", FATAL, 128));
    assert!(f.work.join("junk").exists());
}

#[test]
fn a_bogus_value_stops_dry_run_and_force_too() {
    let f = Fixture::new("modes");
    for mode in ["-n", "-f"] {
        let got = f.run(&["-c", "clean.requireForce=bogus", "clean", mode]);
        assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", FATAL, 128), "{mode}");
        assert!(f.work.join("junk").exists(), "{mode}");
    }
}

#[test]
fn boolean_spellings_still_decide_the_refusal() {
    let f = Fixture::new("spellings");
    // A bare key is true (parse.c: NULL value is 1); `no` lifts the refusal.
    let got = f.run(&["-c", "clean.requireForce", "clean"]);
    assert_eq!(
        (got.1.as_str(), got.2),
        ("fatal: clean.requireForce is true and -f not given: refusing to clean\n", 128)
    );
    let got = f.run(&["-c", "clean.requireForce=no", "clean"]);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("Removing junk\n", "", 0));
    assert!(!f.work.join("junk").exists());
}
