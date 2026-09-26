//! Under an explicit `--git-dir` / `--work-tree`, a hook's `$0`, `GIT_DIR`,
//! `GIT_WORK_TREE` and working directory are what git's setup left behind.
//!
//! `setup_explicit_git_dir()` (setup.c:1107-1205) keeps `$GIT_DIR` verbatim when
//! the cwd is the work tree or lies outside it, and `realpath`s it when setup has
//! to `chdir()` up to the work tree from below (setup.c:1191-1198); either way
//! `set_git_dir()` exports it (setup.c:1070-1074, 1091-1105). A discovered
//! repository with `GIT_WORK_TREE` set goes the same way, its `.git` made
//! absolute first when the walk left the starting directory (setup.c:1217-1228).
//! A `NEED_WORK_TREE` command then passes `setup_work_tree()` (git.c:499-500,
//! setup.c:496-513): `chdir_notify()` re-parents a relative git directory through
//! `reparent_relative_path()` (chdir-notify.c:100-115) — `<old-cwd>/<gitdir>`,
//! kept whole when the new cwd does not lead it — and a set `GIT_WORK_TREE`
//! becomes `.`. `find_hook()` spells the hook `git_path("hooks/<name>")` on that
//! git directory (hook.c:26-64) and `run_hooks_opt()` leaves `cp->dir` NULL
//! (hook.c:609), so the hook starts wherever git stands.
//!
//! zvcs exec'd every hook under an explicit setup by its absolute, normalized
//! path from the top of the work tree, exported the absolute git directory, and
//! passed `GIT_WORK_TREE` through as typed (`..`, `../out`).
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
            .join(format!("zvcs-hook-explicit-setup-{tag}-{}", std::process::id()));
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
        let hook = f.work.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho \"$0|$GIT_DIR|$GIT_WORK_TREE|$GIT_PREFIX|$(pwd)\" >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
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
fn a_relative_work_tree_from_below_makes_the_discovered_git_dir_absolute() {
    let f = Fixture::new("wt-below");
    assert_eq!(
        f.commit("sub", &[], &["--work-tree=.."]),
        ("<W>/.git/hooks/pre-commit|<W>/.git|.|sub/|<W>\n".into(), 0)
    );
    assert_eq!(
        f.commit("out", &[], &["--git-dir=../.git", "--work-tree=.."]),
        ("<W>/.git/hooks/pre-commit|<W>/.git|.|out/|<W>\n".into(), 0)
    );
}

#[test]
fn git_dir_at_the_work_tree_is_kept_as_typed_and_exported() {
    let f = Fixture::new("at-top");
    assert_eq!(
        f.commit("", &[], &["--git-dir=.git", "--work-tree=."]),
        (".git/hooks/pre-commit|.git|.||<W>\n".into(), 0)
    );
    assert_eq!(
        f.commit("", &[], &["--git-dir=.git"]),
        (".git/hooks/pre-commit|.git|||<W>\n".into(), 0)
    );
    // Without a work tree, `GIT_DIR` alone makes the cwd the work tree.
    assert_eq!(
        f.commit("sub", &[("GIT_DIR", "../.git")], &[]),
        ("../.git/hooks/pre-commit|../.git|||<W>/sub\n".into(), 0)
    );
}

#[test]
fn a_work_tree_elsewhere_reparents_the_relative_git_dir_unnormalized() {
    let f = Fixture::new("elsewhere");
    assert_eq!(
        f.commit("sub", &[], &["--git-dir=../.git", "--work-tree=../out"]),
        ("<W>/sub/../.git/hooks/pre-commit|<W>/sub/../.git|.||<W>/out\n".into(), 0)
    );
}
