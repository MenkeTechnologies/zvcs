//! `git add` runs a path's `clean` filter once, and only after the pathspec
//! checks have passed.
//!
//! `cmd_add()` dies on a pathspec that matched nothing (builtin/add.c:568)
//! before `add_files_to_cache()` and `add_files()` (:588-599) reach
//! `index_path()`, which is the one place `convert_to_git()` runs — once per
//! path, writing the blob unless `-n`.
//!
//! zvcs converted every path during the directory walk to compute its id, then
//! converted it a second time to write the blob, so a clean driver ran twice per
//! file (and a failing one printed its `error:` lines twice), and ran even for an
//! `add` that went on to die on an unmatched pathspec.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
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
            .join(format!("zvcs-add-clean-once-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join(".gitattributes"), "*.txt filter=count\n").unwrap();
        std::fs::write(f.work.join("a.txt"), "hi\n").unwrap();
        std::fs::write(f.work.join("b.txt"), "yo\n").unwrap();
        f
    }

    /// `-c filter.count.clean=…`: a clean driver that logs `%f` to `runs` and
    /// passes its input through.
    fn counting(&self) -> String {
        format!("filter.count.clean=echo %f >>{}; cat", self.root.join("runs").display())
    }

    fn runs(&self) -> String {
        std::fs::read_to_string(self.root.join("runs")).unwrap_or_default()
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
fn an_unmatched_pathspec_dies_before_any_filter_runs() {
    let f = Fixture::new("unmatched");
    let c = f.counting();
    let (out, err, code) = f.git(&["-c", &c, "add", "a.txt", "nosuch"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: pathspec 'nosuch' did not match any files\n", 128)
    );
    assert_eq!(f.runs(), "");
    // A failing driver is never started either, so it reports nothing.
    let (_, err, code) = f.git(&["-c", "filter.count.clean=false", "add", "a.txt", "nosuch"]);
    assert_eq!((err.as_str(), code), ("fatal: pathspec 'nosuch' did not match any files\n", 128));
}

#[test]
fn each_added_path_is_filtered_once() {
    let f = Fixture::new("once");
    let c = f.counting();
    let (out, err, code) = f.git(&["-c", &c, "add", "a.txt", "b.txt"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.runs(), "a.txt\nb.txt\n");
    assert_eq!(
        f.git(&["ls-files", "-s"]).0,
        "100644 45b983be36b73c0788dc9cbcb76cbb80fc7bb057 0\ta.txt\n\
         100644 092bfb9bdf74dd8cfd22e812151281ee9aa6f01a 0\tb.txt\n"
    );
}

#[test]
fn a_dry_run_filters_each_path_once_and_a_failing_driver_reports_once() {
    let f = Fixture::new("dry-run");
    let c = f.counting();
    let (out, _, code) = f.git(&["-c", &c, "add", "-n", "a.txt"]);
    assert_eq!((out.as_str(), code), ("add 'a.txt'\n", 0));
    assert_eq!(f.runs(), "a.txt\n");
    let (out, err, code) = f.git(&["-c", "filter.count.clean=false", "add", "a.txt"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: external filter 'false' failed 1\nerror: external filter 'false' failed\n", 0)
    );
}
