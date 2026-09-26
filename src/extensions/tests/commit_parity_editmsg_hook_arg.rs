//! `prepare-commit-msg` and `commit-msg` are handed `COMMIT_EDITMSG` as git
//! spells it, not as an absolute path.
//!
//! `run_commit_hook()` passes `git_path_commit_editmsg()` (builtin/commit.c:
//! 1116-1117, :1133-1134; sequencer.c `run_prepare_commit_msg_hook()`), which
//! `git_path()` builds on `repo->gitdir` as setup left it (path.c:387-431): the
//! discovered `.git` once setup has moved to the top of the work tree
//! (setup.c:1237-1249), a `$GIT_DIR` kept as typed at or outside the work tree
//! (setup.c:1184-1204), and `<old-cwd>/../.git` once `setup_work_tree()` has
//! re-parented it (setup.c:496-513, chdir-notify.c:100-115).
//!
//! zvcs passed `<abs>/sub/../.git/COMMIT_EDITMSG` from a subdirectory.
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
            .join(format!("zvcs-commit-editmsg-arg-{tag}-{}", std::process::id()));
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
        for name in ["prepare-commit-msg", "commit-msg"] {
            let hook = f.work.join(".git/hooks").join(name);
            std::fs::write(&hook, format!("#!/bin/sh\necho \"{name} $*\" >&2\n")).unwrap();
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
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr).replace(self.work.to_str().unwrap(), "<W>"),
            out.status.code().expect("no signal"),
        )
    }

    fn commit(&self, dir: &str, env: &[(&str, &str)], global: &[&str]) -> (String, i32) {
        let mut args = global.to_vec();
        args.extend(["commit", "-q", "--allow-empty", "-m", "x"]);
        self.run_in(&self.work.join(dir), env, &args)
    }
}

#[test]
fn a_subdirectory_commit_hands_the_hooks_dot_git_commit_editmsg() {
    let f = Fixture::new("subdir");
    assert_eq!(
        f.commit("sub", &[], &[]),
        (
            "prepare-commit-msg .git/COMMIT_EDITMSG message\n\
             commit-msg .git/COMMIT_EDITMSG\n"
                .into(),
            0
        )
    );
}

#[test]
fn an_explicit_git_dir_is_spelled_as_typed_or_as_reparented() {
    let f = Fixture::new("explicit");
    assert_eq!(
        f.commit("sub", &[("GIT_DIR", "../.git")], &[]),
        (
            "prepare-commit-msg ../.git/COMMIT_EDITMSG message\n\
             commit-msg ../.git/COMMIT_EDITMSG\n"
                .into(),
            0
        )
    );
    assert_eq!(
        f.commit("sub", &[], &["--work-tree=.."]),
        (
            "prepare-commit-msg <W>/.git/COMMIT_EDITMSG message\n\
             commit-msg <W>/.git/COMMIT_EDITMSG\n"
                .into(),
            0
        )
    );
    assert_eq!(
        f.commit("sub", &[], &["--git-dir=../.git", "--work-tree=../out"]),
        (
            "prepare-commit-msg <W>/sub/../.git/COMMIT_EDITMSG message\n\
             commit-msg <W>/sub/../.git/COMMIT_EDITMSG\n"
                .into(),
            0
        )
    );
}

#[test]
fn a_cherry_pick_from_a_subdirectory_hands_prepare_commit_msg_dot_git() {
    let f = Fixture::new("pick");
    std::fs::write(f.work.join("sub/f"), "f\n").unwrap();
    f.run_in(&f.work, &[], &["add", "sub/f"]);
    f.run_in(&f.work, &[], &["commit", "-q", "--no-verify", "--allow-empty", "-m", "base"]);
    f.run_in(&f.work, &[], &["checkout", "-q", "-b", "side"]);
    std::fs::write(f.work.join("sub/g"), "g\n").unwrap();
    f.run_in(&f.work, &[], &["add", "sub/g"]);
    f.run_in(&f.work, &[], &["commit", "-q", "--no-verify", "-m", "side"]);
    f.run_in(&f.work, &[], &["checkout", "-q", "main"]);
    let (err, code) = f.run_in(&f.work.join("sub"), &[], &["cherry-pick", "side"]);
    assert_eq!((err.as_str(), code), ("prepare-commit-msg .git/COMMIT_EDITMSG message\n", 0));
}
