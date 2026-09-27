//! A `safe.bareRepository` value that is neither `explicit` nor `all`.
//!
//! `allowed_bare_repo_cb()` (setup.c:1458-1476) returns -1 for any other value,
//! and `git_protected_config()` turns that into `git_die_config_linenr()`:
//! `unable to parse 'safe.barerepository' from command-line config`, or
//! `bad config variable … in file … at line <n>`. `get_allowed_bare_repo()` is
//! only asked once the discovery walk stands in a git directory
//! (setup.c:1673-1677), so a bare repository or the inside of a `.git` refuses —
//! gentle commands included — while a work tree, a named `--git-dir` and a
//! command that never sets up do not. Every protected value is read in order,
//! so a bad one dies even when a later one is good. zvcs ignored the value.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// A bare `b.git` with `x.y = bare` and a work-tree repository `r` with
    /// `x.y = work`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-safe-bare-bogus-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "--bare", "b.git"]);
        f.git(&["-C", "b.git", "config", "x.y", "bare"]);
        f.git(&["init", "-q", "-b", "main", "r"]);
        f.git(&["-C", "r", "config", "x.y", "work"]);
        f
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn a_walk_into_a_git_directory_dies_on_it() {
    let f = Fixture::new("dies");
    let refused = (String::new(), "fatal: unable to parse 'safe.barerepository' from command-line config\n".to_string(), 128);
    for args in [
        &["-c", "safe.bareRepository=bogus", "-C", "b.git", "rev-parse", "--git-dir"][..],
        &["-c", "safe.bareRepository=bogus", "-C", "b.git", "config", "x.y"],
        &["-c", "safe.bareRepository=bogus", "-C", "r/.git", "rev-parse", "--git-dir"],
        &["-c", "safe.bareRepository=bogus", "-c", "safe.bareRepository=all", "-C", "b.git", "rev-parse", "--git-dir"],
        &["-c", "safe.bareRepository=all", "-c", "safe.bareRepository=bogus", "-C", "b.git", "rev-parse", "--git-dir"],
    ] {
        assert_eq!(f.git(args), refused, "{args:?}");
    }
}

#[test]
fn nothing_else_asks() {
    let f = Fixture::new("quiet");
    assert_eq!(f.git(&["-c", "safe.bareRepository=bogus", "-C", "r", "rev-parse", "--git-dir"]), (".git\n".into(), String::new(), 0));
    let (_, err, code) = f.git(&["-c", "safe.bareRepository=bogus", "-C", "b.git", "version"]);
    assert_eq!((err.as_str(), code), ("", 0));
}
