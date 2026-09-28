//! A `RUN_SETUP` builtin outside any repository.
//!
//! `run_builtin()` (git.c:474-481) runs `setup_git_directory()` before the
//! builtin sees its arguments, so with no repository every `RUN_SETUP` entry of
//! `commands[]` dies `not a git repository` at 128 — an unknown option, a missing
//! operand and a `-h` next to anything else included. Only a lone `-h` (or
//! `--help-all`) demotes the setup to gentle and gets the usage at 129. zvcs let
//! `update-ref`, `symbolic-ref`, `replace`, `notes`, `mv`, `rm`, `clean` and
//! `status -h …` parse their options first.
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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-run-setup-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nowhere")).unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(self.root.join("nowhere"))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn setup_dies_before_the_options_are_read() {
    let f = Fixture::new();
    let fatal = ("fatal: not a git repository (or any of the parent directories): .git\n".to_owned(), 128);
    for args in [
        &["update-ref", "--bogus"][..],
        &["symbolic-ref"][..],
        &["symbolic-ref", "-m", ""][..],
        &["replace", "--bogus"][..],
        &["notes", "--bogus"][..],
        &["mv", "--bogus"][..],
        &["rm"][..],
        &["clean", "-n"][..],
        &["status", "-h", "--bogus"][..],
        &["update-ref", "--help-all", "x"][..],
    ] {
        assert_eq!(f.run(args), fatal, "{args:?}");
    }
    // A lone `-h` is the one exemption.
    assert_eq!(f.run(&["update-ref", "-h"]).1, 129);
    assert_eq!(f.run(&["symbolic-ref", "--help-all"]).1, 129);
}
