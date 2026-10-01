//! `git diff --no-index -h` / `--help-all`.
//!
//! `diff_no_index()` hands its argv to `parse_options()` (diff-no-index.c:373-374),
//! so a `-h` anywhere among the options is parse-options' own help: the usage
//! block on stdout, nothing on stderr, exit 0 since 2.56 (`PARSE_OPT_HELP`,
//! parse-options.c:1207-1208). `--help-all` renders `USAGE_FULL`, which adds the
//! table's one `PARSE_OPT_HIDDEN` entry, `--no-index` (diff-no-index.c:366-367),
//! ahead of the diff options. zvcs answered `unsupported option "-h"`, exit 1.
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
            .join(format!("zvcs-no-index-help-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f"), "a\n").unwrap();
        std::fs::write(root.join("g"), "b\n").unwrap();
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
fn dash_h_anywhere_is_the_usage_block_on_stdout_at_zero() {
    let f = Fixture::new("h");
    for args in [
        &["diff", "--no-index", "-h"][..],
        &["diff", "--no-index", "--stat", "-h"],
        &["diff", "--no-index", "-h", "f", "g"],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        let body = out.strip_prefix(USAGE).unwrap_or_else(|| panic!("{args:?}: {out}"));
        assert!(body.starts_with("Diff output format options\n    -p, --patch           generate patch\n"), "{args:?}: {body}");
        assert!(!body.contains("--no-index"), "{args:?}: the hidden entry is USAGE_FULL only");
        assert!(body.ends_with("\n\n"), "{args:?}");
    }
}

#[test]
fn help_all_adds_the_hidden_no_index_entry() {
    let f = Fixture::new("all");
    let (out, err, code) = f.run(&["diff", "--no-index", "--help-all"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let body = out.strip_prefix(USAGE).unwrap_or_else(|| panic!("{out}"));
    assert!(body.starts_with("    --no-index\n\nDiff output format options\n"), "{body}");
}
