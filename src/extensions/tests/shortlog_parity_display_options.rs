//! `git shortlog` with `handle_revision_opt()`'s display switches.
//!
//! `--log-size`, `--abbrev-commit`, `--show-signature`, the notes family,
//! `--expand-tabs`, `--show-linear-break`, `--full-diff`, `--always`,
//! `--no-commit-id`, `--root` and the email-header pair only set `rev_info`
//! fields `show_log()` and `log_tree_diff()` read (revision.c:2575-2668), and
//! `cmd_shortlog()` takes nothing from `rev` but the format kind, the
//! abbreviation, the output file and the date mode (builtin/shortlog.c:460-463),
//! so stock accepts each and prints the same tally. `--expand-tabs=<n>` still
//! dies on a value `strtol_i()` refuses (revision.c:2579-2583), and
//! `--relative-date` is the date mode `%ad` reads (revision.c:2660-2662). zvcs
//! answered every one with `unknown option`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `base` adds `a` and `b`; `both` changes the two.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-shortlog-display-options-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a"), "a2\n").unwrap();
        std::fs::write(f.work.join("b"), "b2\n").unwrap();
        f.run(&["commit", "-q", "-am", "both"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
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
fn the_display_switches_leave_the_tally_alone() {
    let f = Fixture::new("inert");
    let (out, err, code) = f.run(&[
        "shortlog",
        "--log-size",
        "--abbrev-commit",
        "--show-signature",
        "--notes",
        "--notes=x",
        "--expand-tabs=4",
        "--show-linear-break",
        "--full-diff",
        "--always",
        "--no-commit-id",
        "--root",
        "HEAD",
    ]);
    assert_eq!((out.as_str(), err.as_str(), code), ("A U Thor (2):\n      base\n      both\n\n", "", 0));
}

#[test]
fn expand_tabs_still_checks_its_value() {
    let f = Fixture::new("tabs");
    let (out, err, code) = f.run(&["shortlog", "--expand-tabs=x", "HEAD"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: 'x': not a non-negative integer\n", 128));
}

#[test]
fn relative_date_reaches_the_format() {
    let f = Fixture::new("reldate");
    let (out, err, code) = f.run(&["shortlog", "--format=%ad|%s", "--relative-date", "HEAD"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "A U Thor (2):");
    assert!(lines[1].ends_with(" ago|base") && lines[2].ends_with(" ago|both"), "{out}");
}
