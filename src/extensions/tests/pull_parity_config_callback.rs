//! `git pull`'s own config callback, run before the fetch.
//!
//! `cmd_pull()` calls `repo_config(the_repository, git_pull_config, NULL)`
//! (builtin/pull.c:1018) ahead of `parse_options()` and of everything that
//! prepares the repository settings. `git_pull_config()` (builtin/pull.c:226-252)
//! reads `rebase.autostash`, `pull.autostash` and `submodule.recurse` through
//! `git_config_bool()` before falling through to `git_default_config`. zvcs ran
//! the default callback alone, so a bad `rebase.autoStash` or `pull.autostash`
//! fetched and merged, a bad `submodule.recurse` was reported by the fetch at
//! exit 1, and the settings block was consulted ahead of the callback.
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
    /// `side` is one commit ahead of `main`, so `pull . side` would fast-forward.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pull-config-callback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("file"), "side\n").unwrap();
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).0
    }
}

fn bad_bool(key: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: bad boolean config value 'bogus' for '{key}'\n"), 128)
}

#[test]
fn each_key_stops_the_pull_before_it_fetches() {
    let f = Fixture::new("keys");
    let before = f.head();
    for (key, lower) in [
        ("rebase.autoStash", "rebase.autostash"),
        ("pull.autostash", "pull.autostash"),
        ("submodule.recurse", "submodule.recurse"),
    ] {
        let setting = format!("{key}=bogus");
        assert_eq!(f.run(&["-c", &setting, "pull", ".", "side"]), bad_bool(lower), "{key}");
        // The callback runs ahead of `parse_options()`, so `-h` is refused too.
        assert_eq!(f.run(&["-c", &setting, "pull", "-h"]), bad_bool(lower), "{key} -h");
    }
    assert_eq!(f.head(), before);
    assert!(!f.work.join(".git/FETCH_HEAD").exists());
}

/// `git_pull_config` runs before `prepare_repo_settings()`, so a refused value
/// in either layer of the callback beats one only the settings block reads.
#[test]
fn the_callback_is_read_before_the_settings_block() {
    let f = Fixture::new("order");
    assert_eq!(
        f.run(&["-c", "core.packedGitLimit=bogus", "-c", "rebase.autoStash=bogus", "pull", ".", "side"]),
        bad_bool("rebase.autostash")
    );
    assert_eq!(
        f.run(&["-c", "core.packedGitLimit=bogus", "-c", "core.abbrev=bogus", "pull", ".", "side"]),
        (
            String::new(),
            "fatal: bad numeric config value 'bogus' for 'core.abbrev': invalid unit\n".to_owned(),
            128
        )
    );
}

#[test]
fn valid_values_still_pull() {
    let f = Fixture::new("valid");
    let (_, _, code) = f.run(&["-c", "pull.autostash=yes", "-c", "rebase.autoStash", "pull", "-q", ".", "side"]);
    assert_eq!(code, 0);
    assert_eq!(f.head(), f.run(&["rev-parse", "side"]).0);
}
