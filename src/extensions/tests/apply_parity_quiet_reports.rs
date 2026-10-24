//! `git apply -q` silences the `--stat`/`--numstat`/`--summary` reports.
//!
//! `apply_patch()` prints each report only when
//! `state->apply_verbosity > verbosity_silent` (apply.c:5007-5014), so `-q`
//! drops them while `--apply` still writes the patch, and a later `--no-quiet`
//! brings them back. zvcs printed the reports regardless of `-q`.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const CREATE: &str = "diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+x\n";

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
            .join(format!("zvcs-apply-quiet-reports-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(root.join("c.diff"), CREATE).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

    fn apply(&self, opts: &[&str]) -> (String, String, i32) {
        let mut args = vec!["apply"];
        args.extend_from_slice(opts);
        args.push("../c.diff");
        self.run(&args)
    }
}

#[test]
fn quiet_drops_every_report_mode() {
    let f = Fixture::new("all");
    assert_eq!(f.apply(&["--stat", "--numstat", "--summary", "-q"]), (String::new(), String::new(), 0));
    assert!(!f.work.join("n").exists());
}

#[test]
fn no_quiet_restores_the_numstat_record() {
    let f = Fixture::new("noquiet");
    assert_eq!(f.apply(&["--numstat", "-q", "--no-quiet"]), ("1\t0\tn\n".to_owned(), String::new(), 0));
}

#[test]
fn quiet_stat_with_apply_still_writes_the_file() {
    let f = Fixture::new("apply");
    assert_eq!(f.apply(&["--stat", "-q", "--apply"]), (String::new(), String::new(), 0));
    assert_eq!(std::fs::read_to_string(f.work.join("n")).unwrap(), "x\n");
}
