//! `git repo info` does not stop at an unknown key.
//!
//! ```c
//! if (!field) {
//!         ret = error(_("key '%s' not found"), key);
//!         continue;
//! }
//! ```
//! (`print_fields()`, builtin/repo.c:141-144)
//!
//! Every key after it is still looked up and printed, a repeated key is printed
//! again, and the -1 only surfaces as the exit code (255). Measured against stock
//! git 2.55.0 on this exact fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-repoinfo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        assert!(f.run(&["init", "-q", "-b", "main"]).status.success());
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }
}

#[test]
fn keys_after_an_unknown_one_are_still_printed() {
    let f = Fixture::new("lines");
    let out = f.run(&["repo", "info", "layout.bare", "info", "layout.bare", "nope", "object.format"]);
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "layout.bare=false\nlayout.bare=false\nobject.format=sha1\n"
    );
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "error: key 'info' not found\nerror: key 'nope' not found\n"
    );
}

#[test]
fn nul_format_continues_the_same_way() {
    let f = Fixture::new("nul");
    let out = f.run(&["repo", "info", "-z", "bogus", "layout.shallow"]);
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(out.stdout, b"layout.shallow\nfalse\0");
}
