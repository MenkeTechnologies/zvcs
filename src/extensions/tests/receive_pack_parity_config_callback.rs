//! `cmd_receive_pack()` runs `repo_config(the_repository, receive_pack_config,
//! NULL)` (builtin/receive-pack.c:2652) after `enter_repo()` and before it
//! writes the advertisement. `receive_pack_config()` (:145-278) refuses a bad
//! boolean among its keys, runs `receive.denyCurrentBranch` and
//! `receive.denyDeleteCurrent` through `parse_deny_action()` (:128-143) — the
//! four words, else `git_config_bool()` — and ends in `git_default_config()`.
//! The receiving side dies, and the pushing side follows with
//! `die_initial_contact()`'s "Could not read from remote repository".
//!
//! zvcs read the deny actions with a parser that mapped anything unknown to
//! `ignore`, and the booleans with a lookup that dropped an unparsable value:
//! `receive.denyCurrentBranch = bogus` let a push into the checked-out branch.
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
    /// A bare `up.git` and a work repository `w` with one commit on `main` and a
    /// remote `o` pointing at `../up.git`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-receive-pack-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref", "--format=%(refname)"]).0
    }

    fn set_remote(&self, key: &str, value: &str) {
        self.run(&["--git-dir=../up.git", "config", key, value]);
    }
}

const NO_CONTACT: &str = "fatal: Could not read from remote repository.\n\n\
                          Please make sure you have the correct access rights\n\
                          and the repository exists.\n";

#[test]
fn each_boolean_key_ends_the_session_before_the_advertisement() {
    for (key, lower) in [
        ("receive.denyDeletes", "receive.denydeletes"),
        ("receive.fsckObjects", "receive.fsckobjects"),
        ("receive.autogc", "receive.autogc"),
        ("transfer.advertiseSID", "transfer.advertisesid"),
        ("core.ignorecase", "core.ignorecase"),
    ] {
        let f = Fixture::new(&lower.replace('.', "-"));
        f.set_remote(key, "bogus");
        let (out, err, code) = f.run(&["push", "o", "main"]);
        let want = format!("fatal: bad boolean config value 'bogus' for '{lower}'\n{NO_CONTACT}");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{key}");
        assert_eq!(f.remote_refs(), "", "{key}");
    }
}

#[test]
fn deny_actions_take_four_words_and_otherwise_a_boolean() {
    let f = Fixture::new("deny");
    f.set_remote("receive.denyCurrentBranch", "bogus");
    let (_, err, code) = f.run(&["push", "o", "main"]);
    assert_eq!(
        (err.as_str(), code),
        (
            format!("fatal: bad boolean config value 'bogus' for 'receive.denycurrentbranch'\n{NO_CONTACT}")
                .as_str(),
            128
        )
    );
    // The words compare without case, and a boolean is a boolean.
    for value in ["UpdateInstead", "0x0"] {
        f.set_remote("receive.denyCurrentBranch", value);
        let (_, _, code) = f.run(&["push", "o", "HEAD:refs/heads/x"]);
        assert_eq!(code, 0, "{value}");
    }
    assert_eq!(f.remote_refs(), "refs/heads/x\n");
}
