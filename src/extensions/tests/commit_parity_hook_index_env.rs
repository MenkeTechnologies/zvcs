//! The commit hooks' `GIT_INDEX_FILE` and `GIT_EDITOR` are the ones
//! `run_commit_hook()` exports.
//!
//! `run_commit_hook()` pushes `GIT_INDEX_FILE=<index_file>` and, when no editor
//! will be launched, `GIT_EDITOR=:` for every commit hook (commit.c:1994-2016) —
//! `pre-commit` (builtin/commit.c:780-781), `prepare-commit-msg`, `commit-msg`
//! and `post-commit` (builtin/commit.c:1966-1967) alike. For a plain commit the
//! index is `repo_get_index_file()`, `<gitdir>/index` on the git directory as
//! setup spelled it (repository.c:101-109, :186-189): `.git/index` from any
//! subdirectory. A locked index is named by `lock_file()`, which makes the path
//! absolute from where git stands (abspath.c:293-316): `<top>/.git/index.lock`,
//! `<top>/.git/next-index-<pid>.lock` (builtin/commit.c:541-554).
//!
//! zvcs exported an absolute `GIT_INDEX_FILE` for a plain commit, none at all to
//! `post-commit`, and left the caller's `GIT_EDITOR` in place for `pre-commit`
//! and `post-commit`.
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
            .join(format!("zvcs-commit-hook-index-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // git reports the physical path (`strbuf_getcwd()`), so the expectations
        // are built on the resolved temp directory.
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(work.join("out")).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.work, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "f\n").unwrap();
        std::fs::write(f.work.join("out/f"), "f\n").unwrap();
        f.run_in(&f.work, &[], &["add", "."]);
        f.run_in(&f.work, &[], &["commit", "-q", "-m", "base"]);
        for name in ["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit"] {
            let hook = f.work.join(".git/hooks").join(name);
            std::fs::write(&hook, "#!/bin/sh\necho \"NAME $GIT_INDEX_FILE|$GIT_EDITOR\" >&2\n".replace("NAME", name)).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f
    }

    fn run_in(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
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
            // An editor in the caller's environment is what `GIT_EDITOR=:` overrides.
            .env("GIT_EDITOR", "true")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr)
                .replace(self.work.to_str().unwrap(), "<W>")
                .replace(&format!("next-index-{}", out_pid(&out.stderr)), "next-index-<PID>"),
            out.status.code().expect("no signal"),
        )
    }

    fn commit(&self, dir: &str, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
        self.run_in(&self.work.join(dir), env, args)
    }
}

/// The pid in a `next-index-<pid>.lock` the hooks reported, so it can be masked.
fn out_pid(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.split("next-index-")
        .nth(1)
        .map(|rest| rest.chars().take_while(char::is_ascii_digit).collect())
        .unwrap_or_default()
}

/// Every hook of one commit, reporting the same index and editor.
fn each(value: &str) -> String {
    ["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit"]
        .iter()
        .map(|name| format!("{name} {value}\n"))
        .collect()
}

#[test]
fn the_plain_index_is_spelled_on_the_git_dir_and_post_commit_gets_it_too() {
    let f = Fixture::new("as-is");
    assert_eq!(
        f.commit("sub", &[], &["commit", "-q", "--allow-empty", "-m", "x"]),
        (each(".git/index|:"), 0)
    );
    assert_eq!(
        f.commit("sub", &[("GIT_DIR", "../.git")], &["commit", "-q", "--allow-empty", "-m", "x"]),
        (each("../.git/index|:"), 0)
    );
    // An editor in play leaves the caller's `GIT_EDITOR` alone.
    let (err, code) = f.commit("", &[], &["commit", "-q", "--allow-empty", "-m", "x", "-e"]);
    assert_eq!((err, code), (each(".git/index|true"), 0));
}

#[test]
fn locks_are_named_absolute_from_where_setup_left_git() {
    let f = Fixture::new("locks");
    std::fs::write(f.work.join("sub/f"), "g\n").unwrap();
    let (err, code) = f.commit("sub", &[], &["commit", "-q", "-m", "x", "f"]);
    let lock = "<W>/.git/next-index-<PID>.lock|:";
    assert_eq!(
        (err, code),
        (
            format!(
                "pre-commit {lock}\nprepare-commit-msg {lock}\ncommit-msg {lock}\n\
                 post-commit .git/index|:\n"
            ),
            0
        )
    );
    std::fs::write(f.work.join("out/f"), "g\n").unwrap();
    let (err, code) = f.commit(
        "sub",
        &[],
        &["--git-dir=../.git", "--work-tree=../out", "commit", "-q", "-a", "-m", "x"],
    );
    let lock = "<W>/sub/../.git/index.lock|:";
    assert_eq!(
        (err, code),
        (
            format!(
                "pre-commit {lock}\nprepare-commit-msg {lock}\ncommit-msg {lock}\n\
                 post-commit <W>/sub/../.git/index|:\n"
            ),
            0
        )
    );
}
