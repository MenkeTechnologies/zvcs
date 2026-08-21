//! `git describe` on an unborn `HEAD` reports git's invalid-object-name fatal.
//!
//! `cmd_describe()` ends its no-argument path in `describe("HEAD", 1)`
//! (`builtin/describe.c:784`) — the same function the positional loop calls at
//! `:790` — and `describe()` opens with:
//!
//! ```c
//! if (repo_get_oid(the_repository, arg, &oid))
//!         die(_("Not a valid object name %s"), arg);
//! ```
//!
//! (`builtin/describe.c:570-571`.) So `git describe --always` in a repository
//! whose `HEAD` points at a branch that does not exist yet is
//! `fatal: Not a valid object name HEAD` and exit 128, byte-identical to
//! `git describe --always HEAD`. Reaching `HEAD` by peeling the branch instead
//! leaked gitoxide's `Branch 'refs/heads/main' does not have any commits` with
//! exit 1.
//!
//! `--always` is what makes the case reachable: without it the
//! `if (!hashmap_get_size(&names) && !always) die(...)` gate at
//! `builtin/describe.c:723-724` fires first and a nameless repository never gets
//! as far as resolving anything.
//!
//! Every expectation was captured from stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NOT_VALID_HEAD: &str = "fatal: Not a valid object name HEAD\n";

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
        let root = std::env::temp_dir().join(format!("zvcs-describe-unborn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    /// A repository whose `HEAD` names a branch nothing has been committed to.
    fn unborn(&self, name: &str, bare: bool) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = vec!["init", "-q", "-b", "main"];
        if bare {
            args.push("--bare");
        }
        args.push(".");
        let out = self.run(&dir, &args);
        assert!(out.status.success(), "init failed: {out:?}");
        dir
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn always_reports_the_unresolvable_head_the_way_git_does() {
    let fx = Fixture::new("always");

    // The implicit `HEAD` and the explicit one are the same call, so they are
    // the same bytes. `--long` and `--all` ride along on the same path.
    for (bare, args) in [
        (true, &["describe", "--always"][..]),
        (true, &["describe", "--always", "HEAD"][..]),
        (true, &["describe", "--always", "--long"][..]),
        (true, &["describe", "--always", "--all"][..]),
        (false, &["describe", "--always"][..]),
        (false, &["describe", "--always", "HEAD"][..]),
    ] {
        let dir = fx.unborn(&format!("u{}{}", usize::from(bare), args.join("")), bare);
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(128), "git {args:?} exit: {out:?}");
        assert_eq!(stderr(&out), NOT_VALID_HEAD, "git {args:?} stderr");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "git {args:?} stdout");
    }
}

#[test]
fn without_always_the_no_names_gate_still_wins() {
    let fx = Fixture::new("nonames");

    // `builtin/describe.c:723-724` runs before any revision is resolved, so a
    // repository with no tags never reaches the invalid-object-name fatal —
    // whether or not `HEAD` could have been resolved. This is the guard that
    // keeps the fix from moving the earlier diagnostic.
    for args in [&["describe"][..], &["describe", "HEAD"][..]] {
        let dir = fx.unborn(&format!("n{}", args.join("")), true);
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(128), "git {args:?} exit: {out:?}");
        assert_eq!(
            stderr(&out),
            "fatal: No names found, cannot describe anything.\n",
            "git {args:?} stderr"
        );
    }
}

#[test]
fn a_named_but_absent_revision_names_itself() {
    let fx = Fixture::new("named");
    let dir = fx.unborn("named", true);

    // The `%s` is the argument as written, so the unborn branch reports the
    // spelling the caller used rather than `HEAD`.
    for (arg, expected) in [
        ("main", "fatal: Not a valid object name main\n"),
        ("refs/heads/main", "fatal: Not a valid object name refs/heads/main\n"),
    ] {
        let out = fx.run(&dir, &["describe", "--always", arg]);
        assert_eq!(out.status.code(), Some(128), "git describe --always {arg}: {out:?}");
        assert_eq!(stderr(&out), expected, "git describe --always {arg}");
    }
}

#[test]
fn a_born_head_still_describes() {
    let fx = Fixture::new("born");
    let dir = fx.unborn("work", false);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    for args in [
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "add", "a.txt"][..],
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "commit", "-q", "-m", "one"][..],
        // Annotated: the default selector collects `refs/tags/*` tag objects, so
        // a lightweight tag would leave the repository nameless.
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "tag", "-a", "v1", "-m", "v1"][..],
    ] {
        let out = fx.run(&dir, args);
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
    }

    // Routing the default through the positional path must not change what the
    // default *does* once `HEAD` resolves.
    for args in [&["describe"][..], &["describe", "HEAD"][..], &["describe", "--always"][..]] {
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(0), "git {args:?}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "v1\n", "git {args:?} stdout");
    }
}
