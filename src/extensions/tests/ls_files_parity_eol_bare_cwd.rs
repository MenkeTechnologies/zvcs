//! `ls-files --eol` reads the worktree column from the current directory in a bare repository.
//!
//! git's `lstat(fullname)` is relative to the cwd, and a repository declared bare (`core.bare`
//! in `config`, or in `config.worktree` under `extensions.worktreeConfig`) never changes into a
//! work tree, so files sitting next to `.git` still report their `w/` column. zvcs joined the
//! path onto a work tree that does not exist and printed an empty column. Expectations measured
//! from stock git 2.56.0.

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
        let root = std::env::temp_dir().join(format!("zvcs-eol-bare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f
    }

    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

const WITH_WORKTREE_COLUMN: &str = "i/lf    w/lf    attr/                 \ta\n";

#[test]
fn core_bare_in_the_common_config() {
    let f = Fixture::new("common");
    assert_eq!(f.run(&["ls-files", "--eol"]), WITH_WORKTREE_COLUMN);
    f.run(&["config", "core.bare", "true"]);
    assert_eq!(f.run(&["ls-files", "--eol"]), WITH_WORKTREE_COLUMN);
}

#[test]
fn core_bare_in_the_worktree_config() {
    let f = Fixture::new("worktree");
    f.run(&["config", "extensions.worktreeConfig", "true"]);
    std::fs::write(f.root.join(".git/config.worktree"), "[core]\n\tbare = 1\n").unwrap();
    assert_eq!(f.run(&["ls-files", "--eol"]), WITH_WORKTREE_COLUMN);
}
