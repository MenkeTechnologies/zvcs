//! `core.logAllRefUpdates` is refused where refs are written, not where they are read.
//!
//! Through 2.55 git parsed the key while it built the ref store
//! (`repo_settings_get_log_all_ref_updates()`), so `log`, `branch --list` and
//! anything else that resolved `HEAD` died on a value like `none`. 2.56 reads
//! the files backend's write options lazily — `files_ref_store_write_options()`
//! (refs/files-backend.c:145-156), called from `files_transaction_finish()`
//! (:3327) — through a `repo_config()` callback, `files_ref_store_config()`
//! (:130-143), that parses *every* occurrence of `core.logallrefupdates` and
//! `core.prefersymlinkrefs` in config order and dies on the first bad one.
//!
//! zvcs kept 2.55's timing: readers died, a valueless key was refused, and a
//! bad value followed by a good one was let through.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-all-ref-updates-write-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .envs(env.iter().copied())
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

    fn refs(&self) -> String {
        self.run(&["for-each-ref", "--format=%(refname)"]).0
    }

    /// Every `*.lock` left under `.git` — a refusal inside the transaction must
    /// not strand the locks it already took.
    fn stray_locks(&self) -> Vec<PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "lock") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.work.join(".git"), &mut out);
        out
    }
}

const NONE_REFUSAL: &str = "fatal: bad boolean config value 'none' for 'core.logallrefupdates'\n";

#[test]
fn readers_run_under_a_value_only_writers_refuse() {
    let f = Fixture::new("readers");
    let none = ["-c", "core.logAllRefUpdates=none"];
    for args in [&["log", "--oneline", "--format=%s"][..], &["branch", "--list"], &["status", "--short"]] {
        let argv: Vec<&str> = none.iter().chain(args).copied().collect();
        let got = f.run(&argv);
        assert_eq!((got.1.as_str(), got.2), ("", 0), "{argv:?}");
    }
    assert_eq!(f.run(&["-c", "core.logAllRefUpdates=none", "log", "--format=%s"]).0, "base\n");
}

#[test]
fn the_parity_premise_worktree_none_with_an_unknown_extension_logs() {
    let f = Fixture::new("premise");
    f.run(&["config", "extensions.worktreeConfig", "true"]);
    f.run(&["config", "--worktree", "core.logAllRefUpdates", "none"]);
    let env = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "extensions.noSuchExtension"),
        ("GIT_CONFIG_VALUE_0", "true"),
    ];
    let got = f.run_env(&["log", "--full-history", "--format=%s", "--", "main..HEAD"], &env);
    assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", "", 0));
}

#[test]
fn ref_writers_refuse_and_leave_no_locks() {
    let f = Fixture::new("writers");
    let before = f.refs();
    for args in [
        &["update-ref", "refs/heads/x", "HEAD"][..],
        &["branch", "y"],
        &["symbolic-ref", "HEAD", "refs/heads/main"],
        &["commit", "-q", "--allow-empty", "-m", "e"],
    ] {
        let argv: Vec<&str> = ["-c", "core.logAllRefUpdates=none"].iter().chain(args).copied().collect();
        let got = f.run(&argv);
        assert_eq!((got.0.as_str(), got.1.as_str(), got.2), ("", NONE_REFUSAL, 128), "{argv:?}");
        assert_eq!(f.stray_locks(), Vec::<PathBuf>::new(), "{argv:?}");
    }
    assert_eq!(f.refs(), before);
}

#[test]
fn every_occurrence_is_parsed_in_config_order() {
    let f = Fixture::new("order");
    let got = f.run(&[
        "-c",
        "core.logAllRefUpdates=none",
        "-c",
        "core.logAllRefUpdates=true",
        "update-ref",
        "refs/heads/z",
        "HEAD",
    ]);
    assert_eq!((got.1.as_str(), got.2), (NONE_REFUSAL, 128));

    let got = f.run(&[
        "-c",
        "core.preferSymlinkRefs=bogus",
        "-c",
        "core.logAllRefUpdates=none",
        "update-ref",
        "refs/heads/z",
        "HEAD",
    ]);
    assert_eq!(
        (got.1.as_str(), got.2),
        ("fatal: bad boolean config value 'bogus' for 'core.prefersymlinkrefs'\n", 128)
    );

    let got = f.run(&[
        "-c",
        "core.logAllRefUpdates=none",
        "-c",
        "core.preferSymlinkRefs=bogus",
        "update-ref",
        "refs/heads/z",
        "HEAD",
    ]);
    assert_eq!((got.1.as_str(), got.2), (NONE_REFUSAL, 128));
    assert_eq!(f.refs(), "refs/heads/main\n");
}

#[test]
fn always_in_any_case_and_a_valueless_key_are_accepted() {
    let f = Fixture::new("accepted");
    let got = f.run(&["-c", "core.logAllRefUpdates=ALWAYS", "update-ref", "refs/heads/v", "HEAD"]);
    assert_eq!((got.1.as_str(), got.2), ("", 0));
    let got = f.run(&["-c", "core.logAllRefUpdates", "update-ref", "refs/heads/w", "HEAD"]);
    assert_eq!((got.1.as_str(), got.2), ("", 0));
    assert!(f.work.join(".git/logs/refs/heads/w").exists());
}
