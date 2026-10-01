//! `git diff` under a `$GIT_OBJECT_DIRECTORY` / `$GIT_COMMON_DIR` that
//! `is_git_directory()` cannot use.
//!
//! The probe (setup.c:433-447) tests `access(X_OK)` on `$GIT_OBJECT_DIRECTORY`
//! in place of `<gitdir>/objects`, and looks for `objects` and `refs` under
//! `$GIT_COMMON_DIR`, so an unusable value un-recognises every candidate. A
//! command that sets up strictly dies with "not a git repository"; `cmd_diff()`
//! sets up gently, so it is handed `nongit` instead and goes on to
//! `DIFF_NO_INDEX_IMPLICIT` (builtin/diff.c:466-476): with fewer than two
//! operands that is the no-index usage block behind a warning, exit 129.
//!
//! The port only consulted the probe for strict verbs and let gentle ones open
//! the repository regardless, so `git diff` printed the worktree patch (or exit 1
//! under `--quiet`) where stock refuses. The same gentle arm covers `git config
//! --local`, which stock refuses outside a repository.
//!
//! Every expectation was measured from stock git 2.56.0 over the same fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const IMPLICIT_WARNING: &str =
    "warning: Not a git repository. Use --no-index to compare two paths outside a working tree\n";
const USAGE_LINE: &str = "usage: git diff --no-index [<options>] <path> <path> [<pathspec>...]\n";

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
    /// One commit of `f`, then a worktree edit, so an ordinary `git diff` has a
    /// patch to print and `--quiet` would exit 1.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-objdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."], &[]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"], &[]);
        f.ok(&["commit", "-q", "-m", "one"], &[]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f
    }

    fn run(&self, args: &[&str], env: &[(&str, String)]) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_COMMON_DIR")
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, args: &[&str], env: &[(&str, String)]) -> String {
        let out = self.run(args, env);
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn path(&self, rel: &str) -> String {
        self.work.join(rel).to_string_lossy().into_owned()
    }
}

/// The implicit no-index refusal: warning, usage block, exit 129, no stdout.
fn assert_implicit_refusal(out: &Output, what: &str) {
    assert_eq!(out.status.code(), Some(129), "{what}: {out:?}");
    assert!(out.stdout.is_empty(), "{what}: stdout {:?}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let head = format!("{IMPLICIT_WARNING}{USAGE_LINE}");
    assert!(stderr.starts_with(&head), "{what}: stderr {stderr:?}");
}

#[test]
fn missing_object_directory_makes_diff_the_implicit_no_index() {
    let f = Fixture::new("missing");
    let env = [("GIT_OBJECT_DIRECTORY", f.path(".git/no-such-objects"))];
    for args in [
        vec!["diff"],
        vec!["diff", "--quiet"],
        vec!["diff", "--quiet", "--ignore-matching-lines=v1", "--no-color"],
        vec!["diff", "--stat", "HEAD"],
        vec!["diff", "f"],
    ] {
        assert_implicit_refusal(&f.run(&args, &env), &format!("{args:?}"));
    }
    // An explicit `--no-index` with two operands still compares them.
    assert_eq!(f.run(&["diff", "--no-index", "f", "f"], &env).status.code(), Some(0));
}

/// A relative value is resolved against the working directory by `access()`,
/// and a regular file fails `X_OK` just as a missing path does.
#[test]
fn relative_and_non_directory_values_are_unusable_too() {
    let f = Fixture::new("relfile");
    assert_implicit_refusal(&f.run(&["diff"], &[("GIT_OBJECT_DIRECTORY", "nosuch".into())]), "relative");
    assert_implicit_refusal(&f.run(&["diff"], &[("GIT_OBJECT_DIRECTORY", "f".into())]), "regular file");
}

/// `$GIT_DIR` names the repository outright, and `setup_explicit_git_dir()`
/// still hands back `nongit` when the probe fails under it.
#[test]
fn explicit_git_dir_with_missing_object_directory_is_nongit() {
    let f = Fixture::new("gitdir");
    let env = [("GIT_DIR", f.path(".git")), ("GIT_OBJECT_DIRECTORY", "nosuch".into())];
    assert_implicit_refusal(&f.run(&["diff", "--quiet"], &env), "GIT_DIR");
}

#[test]
fn common_dir_without_objects_and_refs_makes_diff_the_implicit_no_index() {
    let f = Fixture::new("common");
    let env = [("GIT_COMMON_DIR", f.path("nosuch"))];
    assert_implicit_refusal(&f.run(&["diff", "--stat"], &env), "GIT_COMMON_DIR");
}

/// The same gentle arm reaches `git config`: with no repository, `--local` is
/// refused rather than written into the `.git/config` the probe rejected.
#[test]
fn config_local_is_refused_outside_the_rejected_repository() {
    let f = Fixture::new("config");
    let env = [("GIT_OBJECT_DIRECTORY", "nosuch".into())];
    let out = f.run(&["config", "--local", "foo.bar", "x"], &env);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: --local can only be used inside a git repository\n"
    );
    let config = std::fs::read_to_string(f.work.join(".git/config")).unwrap();
    assert!(!config.contains("[foo]"), "{config}");
}

/// A usable value leaves the repository alone: the ordinary worktree diff.
#[test]
fn usable_object_directory_keeps_the_repository() {
    let f = Fixture::new("usable");
    let env = [("GIT_OBJECT_DIRECTORY", f.path(".git/objects"))];
    let out = f.run(&["diff", "--quiet"], &env);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
}
