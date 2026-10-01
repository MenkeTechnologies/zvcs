//! `git add` with a filter driver whose keys arrive in more than one config
//! section — every `-c filter.<name>.<key>=<v>` is a section of its own.
//!
//! `read_convert_config()` (convert.c:1024-1078) looks the driver up by name for
//! each `filter.<name>.<key>` and updates it in place, so `clean`, `required` and
//! the rest accumulate on one driver, each key keeping its last value. A bare
//! `required` is `git_config_bool()` of a NULL value: true.
//!
//! zvcs built one driver per section and matched the first, so `-c
//! filter.x.clean=false -c filter.x.required=true` ran a non-required `false`
//! and staged the raw content (exit 0), and with the order swapped it died for a
//! driver it believed had no `clean` command without ever running one.
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
            .join(format!("zvcs-add-filter-sections-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join(".gitattributes"), "*.txt filter=up\n").unwrap();
        std::fs::write(f.work.join("a.txt"), "hi\n").unwrap();
        f
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

const FAILED: &str = "error: external filter 'false' failed 1\n\
                      error: external filter 'false' failed\n\
                      fatal: a.txt: clean filter 'up' failed\n";

#[test]
fn required_in_a_later_section_makes_the_failing_clean_fatal() {
    let f = Fixture::new("later");
    let (out, err, code) =
        f.git(&["-c", "filter.up.clean=false", "-c", "filter.up.required=true", "add", "a.txt"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", FAILED, 128));
    assert_eq!(f.git(&["ls-files", "-s"]).0, "");
}

#[test]
fn clean_in_a_later_section_is_run_before_the_required_driver_dies() {
    let f = Fixture::new("earlier");
    // A bare `required` (no `=`) is true.
    let (out, err, code) = f.git(&["-c", "filter.up.required", "-c", "filter.up.clean=false", "add", "a.txt"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", FAILED, 128));
    let (out, err, code) = f.git(&[
        "-c",
        "filter.up.clean=false",
        "-c",
        "filter.up.required=true",
        "hash-object",
        "--path",
        "a.txt",
        "a.txt",
    ]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", FAILED, 128));
}

#[test]
fn a_repository_clean_command_is_required_by_a_command_line_section() {
    let f = Fixture::new("repo-config");
    let mut cfg = std::fs::read_to_string(f.work.join(".git/config")).unwrap();
    cfg.push_str("[filter \"up\"]\n\tclean = tr a-z A-Z\n");
    std::fs::write(f.work.join(".git/config"), cfg).unwrap();
    let (out, err, code) = f.git(&["-c", "filter.up.required=true", "add", "a.txt"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    // `tr a-z A-Z` of "hi\n": the blob of "HI\n".
    assert_eq!(
        f.git(&["ls-files", "-s", "a.txt"]).0,
        "100644 c1e3b52e700b18a2b8e1d2616e9277ab447bddd0 0\ta.txt\n"
    );
}
