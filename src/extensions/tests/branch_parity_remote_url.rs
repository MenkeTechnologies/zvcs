//! `git remote set-url`'s `<oldurl>` is an *extended regular expression*
//! (`regcomp(&old_regex, oldurl, REG_EXTENDED)`, builtin/remote.c:1908) matched
//! against every configured URL, and a pattern that matches none of them is
//! fatal (`No such URL found`, :1916-1917). `--delete` additionally refuses to
//! empty the non-push list (:1918-1919).
//!
//! Alongside, two existence questions `git remote` asks of the *repository's*
//! configuration only:
//!
//!   * `remote_is_configured(remote, 1)` reads `configured_in_repo`, set only
//!     for `CONFIG_SCOPE_LOCAL` / `CONFIG_SCOPE_WORKTREE` keys
//!     (remote.c:502-504, :856-863), so a `remote.<name>.*` key in the user's
//!     global file names no remote here.
//!   * `check_remote_collision()` (builtin/remote.c:162-175) refuses a new
//!     remote whose name nests inside an existing one, or contains it.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    global: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-remoteurl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let global = root.join("gitconfig");
        std::fs::write(&global, "").unwrap();
        let f = Fixture { root, work, global };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn urls(&self, key: &str) -> String {
        let (out, _, _) = self.run(&["config", "--get-all", key]);
        out
    }
}

/// `<oldurl>` is a regular expression, and the replacement applies to every URL
/// it matches.
#[test]
fn an_old_url_argument_is_an_extended_regular_expression() {
    let f = Fixture::new("re");
    f.git(&["remote", "add", "someremote", "foo"]);
    f.git(&["remote", "set-url", "--push", "someremote", "quux"]);

    f.git(&["remote", "set-url", "--push", "someremote", "replaced", "qu+x"]);
    assert_eq!(f.urls("remote.someremote.pushurl"), "replaced\n");
}

/// A pattern matching nothing is fatal and changes nothing, for both the fetch
/// and the push list.
#[test]
fn an_old_url_that_matches_nothing_is_fatal() {
    let f = Fixture::new("nomatch");
    f.git(&["remote", "add", "someremote", "baz"]);

    let (out, err, code) = f.run(&["remote", "set-url", "someremote", "zot", "bar"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: No such URL found: bar\n");
    assert_eq!(f.urls("remote.someremote.url"), "baz\n");

    // The push list is empty, so nothing can match there either.
    let (out, err, code) = f.run(&["remote", "set-url", "--push", "someremote", "zot", "baz"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: No such URL found: baz\n");
    assert_eq!(f.urls("remote.someremote.pushurl"), "");
    assert_eq!(f.urls("remote.someremote.url"), "baz\n");
}

/// `--delete` will not empty the fetch list, and says so before writing.
#[test]
fn delete_refuses_to_remove_every_non_push_url() {
    let f = Fixture::new("delall");
    f.git(&["remote", "add", "someremote", "baz"]);
    f.git(&["remote", "set-url", "--add", "someremote", "bbb"]);

    let (out, err, code) = f.run(&["remote", "set-url", "--delete", "someremote", ".*"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: Will not delete all non-push URLs\n");
    assert_eq!(f.urls("remote.someremote.url"), "baz\nbbb\n");

    // One of the two is fine.
    f.git(&["remote", "set-url", "--delete", "someremote", "bbb"]);
    assert_eq!(f.urls("remote.someremote.url"), "baz\n");
}

/// A `remote.<name>.*` key that lives only in the global file does not make
/// `<name>` an existing remote, so a rename onto it succeeds.
#[test]
fn a_global_remote_key_does_not_claim_the_name() {
    let f = Fixture::new("globalkey");
    f.git(&["remote", "add", "origin", "one"]);
    std::fs::write(&f.global, "[remote \"upstream\"]\n\tprune = true\n").unwrap();

    let (out, err, code) = f.run(&["remote", "rename", "origin", "upstream"]);
    assert_eq!((out.as_str(), code), ("", 0), "{err}");
    assert_eq!(f.stdout(&["remote"]), "upstream\n");
}

/// A remote name that nests inside an existing one, or contains it, is refused
/// before anything is written.
#[test]
fn a_nested_remote_name_collides_with_its_neighbour() {
    let f = Fixture::new("collide");
    f.git(&["remote", "add", "outer", "url"]);

    let (out, err, code) = f.run(&["remote", "add", "outer/inner", "url"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: remote name 'outer/inner' is a subset of existing remote 'outer'\n"
    );
    assert_eq!(f.stdout(&["remote"]), "outer\n");

    f.git(&["remote", "remove", "outer"]);
    f.git(&["remote", "add", "outer/inner", "url"]);
    let (out, err, code) = f.run(&["remote", "add", "outer", "url"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: remote name 'outer' is a superset of existing remote 'outer/inner'\n"
    );
    assert_eq!(f.stdout(&["remote"]), "outer/inner\n");
}
