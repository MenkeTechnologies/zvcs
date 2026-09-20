//! `GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME` is read by `repo_default_branch_name()`
//! ahead of `init.defaultBranch`, so it decides the branch `git init` points an
//! unborn `HEAD` at, the branch `git clone` of an empty repository lands on, and
//! the value `git var GIT_DEFAULT_BRANCH` prints:
//!
//! ```c
//! const char *env = getenv("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME");
//!
//! if (env && *env)
//!         ret = xstrdup(env);
//! if (!ret && repo_config_get_string(r, config_key, &ret) < 0)
//!         die(_("could not retrieve `%s`"), config_display_key);
//! ```
//! (`refs.c:691-701`, v2.55.0)
//!
//! Two properties the C encodes and this port has to match: the override beats
//! `init.defaultBranch` (the key is not even read when it is set), and an *empty*
//! value is no override at all, because the guard is `env && *env`.
//!
//! This is the knob git's own test suite sets for every test file, so ignoring it
//! meant every `git checkout main` in t4202/t6012/t6111 ran against a repository
//! whose only branch was `master`.
#![cfg(unix)]

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
        let root = std::env::temp_dir().join(format!("zvcs-log-initbranch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn cmd(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    /// stdout of a command that must succeed.
    fn stdout(&self, dir: &PathBuf, envs: &[(&str, &str)], args: &[&str]) -> String {
        let mut c = self.cmd(dir, args);
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn sub(&self, name: &str) -> PathBuf {
        let p = self.root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

/// `git init` with the override set points the unborn `HEAD` at that name, and
/// with it unset falls back to the compiled-in `master`.
#[test]
fn init_head_follows_the_test_override() {
    let f = Fixture::new("init");
    let a = f.sub("a");
    f.stdout(&a, &[("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "main")], &["init", "-q", "."]);
    assert_eq!(f.stdout(&a, &[], &["symbolic-ref", "HEAD"]), "refs/heads/main\n");

    let b = f.sub("b");
    f.stdout(&b, &[], &["init", "-q", "."]);
    assert_eq!(f.stdout(&b, &[], &["symbolic-ref", "HEAD"]), "refs/heads/master\n");
}

/// `if (env && *env)` — an empty value is not an override, so `init.defaultBranch`
/// still decides. A non-empty one wins over the config key outright.
#[test]
fn override_beats_config_and_an_empty_value_does_not() {
    let f = Fixture::new("cfg");

    let a = f.sub("a");
    f.stdout(&a, &[("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "fromenv")], &[
        "-c",
        "init.defaultBranch=fromcfg",
        "init",
        "-q",
        ".",
    ]);
    assert_eq!(f.stdout(&a, &[], &["symbolic-ref", "HEAD"]), "refs/heads/fromenv\n");

    let b = f.sub("b");
    f.stdout(&b, &[("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "")], &[
        "-c",
        "init.defaultBranch=fromcfg",
        "init",
        "-q",
        ".",
    ]);
    assert_eq!(f.stdout(&b, &[], &["symbolic-ref", "HEAD"]), "refs/heads/fromcfg\n");
}

/// `git var GIT_DEFAULT_BRANCH` reads the same function, so it reports the
/// override too.
#[test]
fn var_default_branch_reports_the_override() {
    let f = Fixture::new("var");
    let a = f.sub("a");
    f.stdout(&a, &[], &["init", "-q", "."]);
    assert_eq!(
        f.stdout(&a, &[("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "trunk")], &["var", "GIT_DEFAULT_BRANCH"]),
        "trunk\n"
    );
    assert_eq!(f.stdout(&a, &[], &["var", "GIT_DEFAULT_BRANCH"]), "master\n");
}
