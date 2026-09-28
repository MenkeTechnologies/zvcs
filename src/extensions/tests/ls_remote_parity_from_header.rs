//! `ls-remote`'s `From <url>` header comes after the refs are in.
//!
//! `cmd_ls_remote()` prints the header — only when `<repository>` was left off
//! — after `transport_get_remote_refs()` has answered
//! (builtin/ls-remote.c:149-156), so a remote that cannot be reached gets the
//! transport's `fatal:` alone. zvcs printed the header before connecting.
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
    /// `work` clones `up`, which is then moved away.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-ls-remote-from-header-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&f.root.join("up"), &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
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
}

#[test]
fn an_unreachable_default_remote_gets_no_header() {
    let f = Fixture::new("gone");
    std::fs::rename(f.root.join("up"), f.root.join("up.moved")).unwrap();
    let (out, err, code) = f.run_in(&f.work, &["ls-remote"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.starts_with("fatal: '"), "{err}");
    assert!(!err.contains("From "), "{err}");
}

#[test]
fn a_reachable_default_remote_gets_it() {
    let f = Fixture::new("there");
    let (_, err, code) = f.run_in(&f.work, &["ls-remote", "--heads"]);
    let url = std::fs::canonicalize(f.root.join("up")).unwrap();
    assert_eq!((err, code), (format!("From {}\n", url.display()), 0));
}
