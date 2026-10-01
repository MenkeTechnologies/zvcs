//! `hash-object` parses its options before it reads any configuration.
//!
//! `hash-object` carries no setup flag in `commands[]` (git.c:588), and
//! `cmd_hash_object()` runs `parse_options()` (builtin/hash-object.c:99-100)
//! before `setup_git_directory_gently()` and `repo_config()` (:102-115). So an
//! option it refuses is a 129 usage error ahead of a bad configuration value,
//! while the option *combinations* it refuses (:117-135) come after
//! `repo_config()` and lose to it. zvcs ran its config gates first and answered
//! 128 for both.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
        let root = std::env::temp_dir().join(format!("zvcs-hash-object-options-first-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .stdin(Stdio::null())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn bad_abbrev(&self, args: &[&str]) -> (String, String, i32) {
        let mut all = vec!["-c", "core.abbrev=bogus", "hash-object"];
        all.extend_from_slice(args);
        self.run(&all)
    }
}

const BAD_ABBREV: &str = "fatal: bad numeric config value 'bogus' for 'core.abbrev': invalid unit\n";

#[test]
fn a_refused_option_wins_over_a_bad_config_value() {
    let f = Fixture::new("option");
    let (out, err, code) = f.bad_abbrev(&["--bogus"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(err.starts_with("error: unknown option `bogus'\nusage: git hash-object "), "{err}");
    assert_eq!(f.bad_abbrev(&["--path"]), (String::new(), "error: option `path' requires a value\n".into(), 129));
    let (out, _, code) = f.bad_abbrev(&["-h"]);
    assert!(out.starts_with("usage: git hash-object "), "{out}");
    assert_eq!(code, 0);
    // The same from outside any repository.
    let (_, err, code) = f.run(&["-C", "/", "-c", "core.abbrev=bogus", "hash-object", "--bogus"]);
    assert_eq!(code, 129);
    assert!(err.starts_with("error: unknown option `bogus'\n"), "{err}");
}

#[test]
fn a_refused_combination_loses_to_a_bad_config_value() {
    let f = Fixture::new("combination");
    for args in [&["--stdin"][..], &["--stdin", "--stdin-paths"], &["--stdin", "--stdin"], &["-t", "bogus", "--stdin"]] {
        assert_eq!(f.bad_abbrev(args), (String::new(), BAD_ABBREV.into(), 128), "{args:?}");
    }
}
