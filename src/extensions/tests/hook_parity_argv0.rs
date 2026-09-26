//! A hook is exec'd by the name `find_hook()` gives it, so that is its `$0`.
//!
//! `find_hook()` builds the path with `repo_git_path()` / `adjust_git_path()`
//! (hook.c:26-64, path.c:387-431) after `setup_git_directory()` has moved to the
//! top of the work tree, and `run_hooks_opt()` hands it to `start_command()` as
//! `argv[0]` (hook.c:630-650). For a discovered repository that is
//! `.git/hooks/<name>` — or `<core.hooksPath>/<name>`, with a leading `./`
//! dropped by `strbuf_cleanup_path()` (path.c:52-57) — however deep the command
//! was typed. The kernel passes a `#!` script the pathname given to `execve()`,
//! so the hook sees that relative spelling as `$0`.
//!
//! zvcs exec'd the absolute path, so every hook saw `/…/.git/hooks/<name>`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::os::unix::fs::PermissionsExt;
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-hook-argv0-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "1\n2\n3\n").unwrap();
        f.run(&["add", "f"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// A hook in `dir` that reports its name, `$0` and its arguments on stderr.
    fn hook(&self, dir: &str, name: &str) {
        let dir = self.work.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho \"{name} $0 $*\" >&2\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn commit_hooks_see_the_git_dir_relative_name_from_a_subdirectory() {
    let f = Fixture::new("commit");
    for name in ["pre-commit", "post-commit"] {
        f.hook(".git/hooks", name);
    }
    let (out, err, code) = f.run_in(&f.work.join("sub"), &["commit", "-q", "-m", "x"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "pre-commit .git/hooks/pre-commit \n\
             post-commit .git/hooks/post-commit \n",
            0
        )
    );
}

#[test]
fn a_relative_hooks_path_is_spelled_from_the_top_without_its_dot_slash() {
    let f = Fixture::new("hookspath");
    f.hook("hk", "pre-commit");
    for (dir, value) in [(&f.work, "./hk"), (&f.work.join("sub"), "hk")] {
        let key = format!("core.hooksPath={value}");
        let (_, err, code) = f.run_in(dir, &["-c", &key, "commit", "-q", "--allow-empty", "-m", "x"]);
        assert_eq!((err.as_str(), code), ("pre-commit hk/pre-commit \n", 0), "{value}");
    }
}

#[test]
fn am_hooks_see_the_git_dir_relative_name_from_a_subdirectory() {
    let f = Fixture::new("am");
    f.run(&["commit", "-q", "-m", "base"]);
    f.run(&["checkout", "-q", "-b", "side"]);
    std::fs::write(f.work.join("f"), "1\ntwo\n3\n").unwrap();
    f.run(&["commit", "-q", "-am", "change two"]);
    let (patch, _, _) = f.run(&["format-patch", "-1", "--stdout"]);
    let mbox = f.root.join("mbox");
    std::fs::write(&mbox, patch).unwrap();
    f.run(&["checkout", "-q", "main"]);
    for name in ["applypatch-msg", "pre-applypatch", "post-applypatch"] {
        f.hook(".git/hooks", name);
    }
    let (out, err, code) = f.run_in(&f.work.join("sub"), &["am", mbox.to_str().unwrap()]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "Applying: change two\n",
            "applypatch-msg .git/hooks/applypatch-msg .git/rebase-apply/final-commit\n\
             pre-applypatch .git/hooks/pre-applypatch \n\
             post-applypatch .git/hooks/post-applypatch \n",
            0
        )
    );
}
