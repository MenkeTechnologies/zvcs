//! `git repo info`'s `path.*` keys, new in git 2.56 (builtin/repo.c:80-122,
//! v2.56.0): the git and common directories through `format_path()` —
//! `PATH_FORMAT_CANONICAL` for `.absolute`, `PATH_FORMAT_RELATIVE` against
//! `repo->prefix` for `.relative`. Measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const KEYS: [&str; 4] = [
    "path.commondir.absolute",
    "path.commondir.relative",
    "path.gitdir.absolute",
    "path.gitdir.relative",
];

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
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = tmp.join(format!("zvcs-repoinfo-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/a/b")).unwrap();
        let f = Fixture { root };
        assert!(f.run("repo", &["init", "-q", "-b", "main"]).status.success());
        f
    }

    fn run(&self, dir: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.root.join(dir))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }

    fn info(&self, dir: &str) -> String {
        let mut args = vec!["repo", "info"];
        args.extend(KEYS);
        let out = self.run(dir, &args);
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

fn expect(common: &Path, common_rel: &str, gitdir: &Path, gitdir_rel: &str) -> String {
    format!(
        "path.commondir.absolute={}\npath.commondir.relative={common_rel}\n\
         path.gitdir.absolute={}\npath.gitdir.relative={gitdir_rel}\n",
        common.display(),
        gitdir.display()
    )
}

#[test]
fn relative_is_measured_from_where_the_command_ran() {
    let f = Fixture::new("plain");
    let git = f.root.join("repo/.git");
    assert_eq!(f.info("repo"), expect(&git, ".git", &git, ".git"));
    assert_eq!(f.info("repo/a/b"), expect(&git, "../../.git", &git, "../../.git"));
    assert_eq!(f.info("repo/.git"), expect(&git, "./", &git, "./"));
    assert_eq!(f.info("repo/.git/refs"), expect(&git, "../", &git, "../"));
}

#[test]
fn a_linked_worktree_has_its_own_gitdir_and_the_shared_commondir() {
    let f = Fixture::new("wt");
    assert!(f.run("repo", &["commit", "-q", "--allow-empty", "-m", "x"]).status.success());
    assert!(f.run("repo", &["worktree", "add", "-q", "../wt"]).status.success());
    let common = f.root.join("repo/.git");
    let gitdir = f.root.join("repo/.git/worktrees/wt");
    assert_eq!(f.info("wt"), expect(&common, "../repo/.git", &gitdir, "../repo/.git/worktrees/wt"));
}

#[test]
fn keys_and_all_list_the_new_keys_in_table_order() {
    let f = Fixture::new("keys");
    let out = f.run("repo", &["repo", "info", "--keys"]);
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "layout.bare\nlayout.shallow\nobject.format\npath.commondir.absolute\n\
         path.commondir.relative\npath.gitdir.absolute\npath.gitdir.relative\nreferences.format\n"
    );
    let out = f.run("repo/a", &["repo", "info", "-z", "path.gitdir.relative"]);
    assert_eq!(out.stdout, b"path.gitdir.relative\n../.git\0");
}
