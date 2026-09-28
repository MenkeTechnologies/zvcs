//! `git remote rename <legacy> <same name>` migrates the file into config.
//!
//! `mv()` (builtin/remote.c:882-889) finds the remote through `remote_get()`,
//! legacy files included, and renaming one onto itself runs
//! `migrate_file()` (builtin/remote.c:762-788): the URLs, push and fetch
//! refspecs become `remote.<name>.*`, in that order, and the `remotes/` or
//! `branches/` file is removed — the migration the deprecation warning asks
//! for. Renaming it to another name fails at the section rename
//! (builtin/remote.c:900-906), since no section exists. zvcs answered
//! `No such remote` for both.
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
    /// An empty repository with `remotes/r` and `branches/b`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-remote-legacy-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "-b", "main", "work"]);
        let git = f.work.join(".git");
        std::fs::create_dir_all(git.join("remotes")).unwrap();
        std::fs::create_dir_all(git.join("branches")).unwrap();
        std::fs::write(
            git.join("remotes/r"),
            "URL: ../up\nPull: refs/heads/main:refs/remotes/r/main\nPush: refs/heads/main:refs/heads/pushed\n",
        )
        .unwrap();
        std::fs::write(git.join("branches/b"), "../up#topic\n").unwrap();
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

    fn config(&self, key: &str) -> String {
        self.run(&["config", "--get-all", key]).0
    }
}

#[test]
fn renaming_onto_itself_migrates_the_file() {
    let f = Fixture::new("migrate");
    let (out, _, code) = f.run(&["remote", "rename", "r", "r"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(!f.work.join(".git/remotes/r").exists());
    assert_eq!(f.config("remote.r.url"), "../up\n");
    assert_eq!(f.config("remote.r.push"), "refs/heads/main:refs/heads/pushed\n");
    assert_eq!(f.config("remote.r.fetch"), "refs/heads/main:refs/remotes/r/main\n");

    let (_, _, code) = f.run(&["remote", "rename", "b", "b"]);
    assert_eq!(code, 0);
    assert!(!f.work.join(".git/branches/b").exists());
    assert_eq!(f.config("remote.b.push"), "HEAD:refs/heads/topic\n");
    assert_eq!(f.config("remote.b.fetch"), "refs/heads/topic:refs/heads/b\n");
    // Migrated, it is an ordinary configured remote.
    assert_eq!(f.run(&["remote", "rename", "r", "r"]).2, 3);
}

#[test]
fn renaming_to_another_name_finds_no_section() {
    let f = Fixture::new("other");
    let (_, err, code) = f.run(&["remote", "rename", "b", "nb"]);
    assert_eq!(code, 1);
    assert!(
        err.ends_with("error: Could not rename config section 'remote.b' to 'remote.nb'\n"),
        "{err}"
    );
    assert!(f.work.join(".git/branches/b").exists());
}
