//! `read_mailmap()` (mailmap.c:214-243) reads `mailmap.file` through
//! `repo_config_get_pathname()` and `mailmap.blob` through
//! `repo_config_get_string()`; both die through `git_die_config()`
//! (config.c:2561-2577) when the last value has no `=`, and
//! `git_config_pathname()` dies on its own when a `~user` cannot be expanded
//! (config.c:1318-1320). The port used to treat all three as unset.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
        let root = std::env::temp_dir().join(format!("zvcs-mailmapdie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-qm", "m"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.work, args)
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }
}

fn assert_died(out: &Output, stderr: &str) {
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr);
    assert_eq!(out.status.code(), Some(128));
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn valueless_mailmap_keys_on_the_command_line_die() {
    let f = Fixture::new("cli");
    assert_died(
        &f.run(&["-c", "mailmap.blob", "log", "-1", "--format=%aN"]),
        "error: missing value for 'mailmap.blob'\n\
         fatal: unable to parse 'mailmap.blob' from command-line config\n",
    );
    // The key is printed as git spells its literal, not as typed; and
    // `mailmap.file` is read before `mailmap.blob`, so it is the one reported.
    assert_died(
        &f.run(&["-c", "mailmap.blob", "-c", "MailMap.File", "shortlog", "-s", "HEAD"]),
        "error: missing value for 'mailmap.file'\n\
         fatal: unable to parse 'mailmap.file' from command-line config\n",
    );
    // Last value wins: a valueless spelling overridden later is fine, one
    // overriding a value is not.
    let out = f.run(&["-c", "mailmap.file", "-c", "mailmap.file=x", "log", "-1", "--format=%aN"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "A\n");
    assert_died(
        &f.run(&["-c", "mailmap.file=x", "-c", "mailmap.file", "check-mailmap", "A <a@x>"]),
        "error: missing value for 'mailmap.file'\n\
         fatal: unable to parse 'mailmap.file' from command-line config\n",
    );
}

#[test]
fn valueless_mailmap_blob_in_repository_config_names_file_and_line() {
    let f = Fixture::new("repo");
    let config = f.work.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[mailmap]\n\tblob\n");
    let line = text.lines().count();
    std::fs::write(&config, text).unwrap();
    assert_died(
        &f.run(&["check-mailmap", "A <a@x>"]),
        &format!(
            "error: missing value for 'mailmap.blob'\n\
             fatal: bad config variable 'mailmap.blob' in file '.git/config' at line {line}\n"
        ),
    );
}

#[test]
fn valueless_mailmap_file_in_global_config_dies_outside_a_repository_too() {
    let f = Fixture::new("global");
    let global = f.root.join(".gitconfig");
    std::fs::write(&global, "[user]\n\tx = 1\n[mailmap]\n\tfile\n").unwrap();
    let expected = format!(
        "error: missing value for 'mailmap.file'\n\
         fatal: bad config variable 'mailmap.file' in file '{}' at line 4\n",
        global.display()
    );
    assert_died(&f.run(&["shortlog", "-s", "HEAD"]), &expected);
    // Outside a repository `shortlog` reads stdin; the die comes first.
    let outside = f.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    assert_died(&f.run_in(&outside, &["shortlog", "-s"]), &expected);
}

#[test]
fn unexpandable_user_in_mailmap_file_dies_even_when_optional() {
    let f = Fixture::new("tilde");
    for value in ["mailmap.file=~zvcs-no-such-user/x", "mailmap.file=:(optional)~zvcs-no-such-user/x"] {
        assert_died(
            &f.run(&["-c", value, "log", "-1", "--format=%aN"]),
            "fatal: failed to expand user dir in: '~zvcs-no-such-user/x'\n",
        );
    }
    // A missing `:(optional)` file stays unset.
    let out = f.run(&["-c", "mailmap.file=:(optional)nope", "log", "-1", "--format=%aN"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "A\n");
    assert!(out.status.success());
}
