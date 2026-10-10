//! `git rm` refreshes the index for the pathspec before it judges anything.
//!
//! `refresh_index(…, &pathspec, …)` (builtin/rm.c) hashes a matching entry whose stat data cannot
//! vouch for it, and that read dies on a bad `--attr-source` or an unreadable
//! `core.bigFileThreshold`. zvcs went straight to the local-modifications check and removed the
//! file (or refused it with exit 1). A racily clean file is one rewritten to the same size after the
//! index was written. Expectations measured from
//! stock git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

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
        let root = std::env::temp_dir().join(format!("zvcs-rm-refresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        std::fs::write(f.root.join("b"), "b\n").unwrap();
        f.run(&[], &["add", "."]);
        f.run(&[], &["commit", "-q", "-m", "one"]);
        // Same size as the committed `a\n`, written after the index: racily clean.
        std::fs::write(f.root.join("a"), "x\n").unwrap();
        f
    }

    fn run(&self, config: &[&str], args: &[&str]) -> (String, i32) {
        let mut cmd = Command::new(BIN);
        for c in config {
            cmd.args(["-c", c]);
        }
        let out = cmd
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn a_bad_attr_source_dies_in_the_refresh_of_a_matching_entry() {
    let f = Fixture::new("attr");
    let want = ("fatal: bad --attr-source or GIT_ATTR_SOURCE\n".to_string(), 128);
    assert_eq!(f.run(&[], &["--attr-source=does-not-exist", "rm", "--force", "a"]), want);
    assert_eq!(f.run(&[], &["--attr-source=does-not-exist", "rm", "--cached", "a"]), want);
    assert!(f.root.join("a").exists());
}

#[test]
fn an_unreadable_threshold_dies_there_too() {
    let f = Fixture::new("big");
    assert_eq!(
        f.run(&["core.bigFileThreshold=input"], &["rm", "-f", "a"]),
        ("fatal: bad numeric config value 'input' for 'core.bigfilethreshold': invalid unit\n".to_string(), 128)
    );
}
