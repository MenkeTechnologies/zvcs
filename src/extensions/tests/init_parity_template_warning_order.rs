//! A template directory that cannot be opened is warned about before anything
//! the reference database says.
//!
//! `create_default_files()` (setup.c) starts with `copy_templates()`, whose
//! `warning(_("templates not found in %s"), template_dir)` therefore lands
//! before `create_reference_database()` resolves the initial branch — the
//! `defaultBranchName` hint on a fresh init, the `re-init: ignored
//! --initial-branch` warning on a reinit. zvcs copied the template after the
//! branch was set up, so both came out in the opposite order.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
        let root = std::env::temp_dir()
            .join(format!("zvcs-init-template-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_TEMPLATE_DIR")
            .env_remove("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn the_missing_template_warning_precedes_the_branch_diagnostics() {
    let f = Fixture::new("order");
    let missing = f.root.join("no-such-template");
    let template = format!("--template={}", missing.display());
    let warning = format!("warning: templates not found in {}\n", missing.display());

    let (err, code) = f.run(&["init", &template, "repo"]);
    assert_eq!(code, 0);
    assert!(err.starts_with(&warning), "{err}");
    assert!(err[warning.len()..].starts_with("hint: Using 'master'"), "{err}");

    let (err, code) = f.run(&["init", &template, "-b", "x", "repo"]);
    assert_eq!(code, 0);
    assert_eq!(err, format!("{warning}warning: re-init: ignored --initial-branch=x\n"));
}
