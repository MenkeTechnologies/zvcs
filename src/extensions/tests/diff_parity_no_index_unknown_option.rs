//! `git diff --no-index` with an option its parser does not carry.
//!
//! `diff_no_index()` parses with `no_index_options` (`--no-index` alone)
//! followed by `add_diff_options()` (diff-no-index.c:365-374), so anything
//! outside that table is `parse_options()`'s `unknown option` / `unknown
//! switch` followed by the usage block, exit 129 (parse-options.c:1214-1223).
//! `-i` / `--regexp-ignore-case` and the grep dialect flags are among them:
//! they are `setup_revisions()` options (revision.c:2686-2696), which the
//! no-index path never runs. zvcs answered `unsupported option "-i"`, exit 1.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-unknown-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f"), "a\nB\n").unwrap();
        std::fs::write(root.join("g"), "y\n").unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
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

const USAGE: &str = "usage: git diff --no-index [<options>] <path> <path> [<pathspec>...]\n\n";

#[test]
fn revision_only_options_are_unknown_to_the_no_index_parser() {
    let f = Fixture::new("rev");
    for (opt, first) in [
        ("-i", "error: unknown switch `i'\n"),
        ("-E", "error: unknown switch `E'\n"),
        ("-iw", "error: unknown switch `i'\n"),
        ("--regexp-ignore-case", "error: unknown option `regexp-ignore-case'\n"),
        ("--basic-regexp", "error: unknown option `basic-regexp'\n"),
        ("--bogus=1", "error: unknown option `bogus=1'\n"),
    ] {
        let (out, err, code) = f.run(&["diff", "--no-index", opt, "f", "g"]);
        assert_eq!((out.as_str(), code), ("", 129), "{opt}");
        let usage = err.strip_prefix(first).unwrap_or_else(|| panic!("{opt}: {err}"));
        assert!(usage.starts_with(USAGE), "{opt}: {usage}");
        assert!(usage.contains("\n    --name-only           show only names of changed files\n"), "{opt}");
    }
}

#[test]
fn a_table_option_still_diffs() {
    let f = Fixture::new("ok");
    let (out, err, code) = f.run(&["diff", "--no-index", "--name-only", "f", "g"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("g\n", "", 1));
}
