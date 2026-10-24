//! An unparsable `core.bare` refuses `git repo` even with `$GIT_WORK_TREE` set.
//!
//! `read_and_verify_repository_format()` reads `.git/config` through
//! `read_worktree_config()`, whose `git_config_bool()` dies on `core.bare = none`
//! (setup.c:585-602), before the explicit-git-dir arm looks at `$GIT_WORK_TREE`
//! at all (setup.c:1175-1190). `repo` is `RUN_SETUP` (git.c:647), so the die
//! comes ahead of parse-options' "need a subcommand". Measured against stock
//! git 2.56.0: `fatal: bad boolean config value 'none' for 'core.bare'` at 128.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
        let root = std::env::temp_dir().join(format!("zvcs-repobare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        assert!(f.run(&["init", "-q", "-b", "main"], false).status.success());
        let config = f.root.join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[core]\n\tbare = none\n");
        std::fs::write(&config, text).unwrap();
        f
    }

    fn run(&self, args: &[&str], work_tree_env: bool) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("GIT_WORK_TREE");
        if work_tree_env {
            cmd.env("GIT_WORK_TREE", ".");
        }
        cmd.output().unwrap()
    }
}

const FATAL: &str = "fatal: bad boolean config value 'none' for 'core.bare'\n";

#[test]
fn bare_repo_verb_dies_on_the_config_read_with_work_tree_env() {
    let f = Fixture::new("noverb");
    let out = f.run(&["-P", "repo"], true);
    assert_eq!(out.status.code(), Some(128));
    assert!(out.stdout.is_empty());
    assert_eq!(String::from_utf8(out.stderr).unwrap(), FATAL);
}

#[test]
fn repo_info_dies_the_same_way_with_and_without_work_tree_env() {
    let f = Fixture::new("info");
    for env in [true, false] {
        let out = f.run(&["repo", "info", "layout.bare"], env);
        assert_eq!(out.status.code(), Some(128), "GIT_WORK_TREE set: {env}");
        assert!(out.stdout.is_empty());
        assert_eq!(String::from_utf8(out.stderr).unwrap(), FATAL);
    }
}
