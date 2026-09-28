//! `git hook` outside a repository.
//!
//! `hook` is `RUN_SETUP_GENTLY` (git.c:591), so it runs without a repository:
//! `find_hook()` answers NULL when there is no gitdir (hook.c:32-33) and
//! `get_hook_config_cache()` builds a throwaway map from whatever configuration
//! there is (hook.c:453-477) — system, global and command line. A missing hook
//! is then the ordinary `cannot find a hook named <event>` (exit 1), and a
//! hook configured globally runs in the directory the command was typed in.
//! zvcs demanded a repository and died with `not a git repository` (128).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// A plain directory with no repository above it (the ceiling stops
    /// discovery at `root`).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-hook-outside-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Fixture { root, work }
    }

    fn run(&self, global: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
fn a_missing_hook_is_reported_as_missing_not_as_a_missing_repository() {
    let f = Fixture::new("missing");
    let none = Path::new("/dev/null");
    assert_eq!(
        f.run(none, &["hook", "run", "pre-commit"]),
        (String::new(), "error: cannot find a hook named pre-commit\n".into(), 1)
    );
    assert_eq!(
        f.run(none, &["hook", "run", "--ignore-missing", "pre-commit"]),
        (String::new(), String::new(), 0)
    );
    assert_eq!(
        f.run(none, &["hook", "list", "pre-commit"]),
        (String::new(), "warning: no hooks found for event 'pre-commit'\n".into(), 1)
    );
}

#[test]
fn a_globally_configured_hook_runs_where_the_command_was_typed() {
    let f = Fixture::new("global");
    let global = f.root.join("gitconfig");
    std::fs::write(
        &global,
        "[hook \"h\"]\n\tcommand = pwd\n\tevent = pre-commit\n",
    )
    .unwrap();
    let (out, err, code) = f.run(&global, &["hook", "run", "pre-commit"]);
    // The hook's stdout is sent to stderr.
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(
        std::fs::canonicalize(err.trim_end()).unwrap(),
        std::fs::canonicalize(&f.work).unwrap()
    );
    assert_eq!(
        f.run(&global, &["hook", "list", "--show-scope", "pre-commit"]),
        ("global\th\n".into(), String::new(), 0)
    );
}
