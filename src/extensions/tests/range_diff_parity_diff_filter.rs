//! `git range-diff --diff-filter`.
//!
//! `diff_opt_diff_filter()` accumulates the letters across occurrences and
//! rejects an unknown one at parse time (diff.c:5470-5500), and
//! `diffcore_apply_filter()` closes `diffcore_std()` (diff.c:7526). Range-diff's
//! one outer filepair is a plain modification (`M`, no `-B` score), so an
//! uppercase set without `M`, a lowercase `m`, or a bare `*` drops it, and
//! `diff_flush()` then writes nothing in any format (diff.c:7197). zvcs stopped
//! with `fatal: unsupported flag`.
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

/// `seq 1 30` with the named lines replaced.
fn numbered(replace: &[(u32, &str)]) -> String {
    (1..=30)
        .map(|n| match replace.iter().find(|(at, _)| *at == n) {
            Some((_, text)) => format!("{text}\n"),
            None => format!("{n}\n"),
        })
        .collect()
}

impl Fixture {
    /// `a` rewrites lines 3, 20 and 27 of `f` as `3a`, `20a`, `27a`; `b` as
    /// `3b`, `20a` plus a blank line, `27b`. Both rewrite line 2 of `g`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-filter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("f", &numbered(&[]));
        f.write("g", "1\n2\n3\n4\n5\n");
        f.run(&["add", "f", "g"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "a"]);
        f.write("f", &numbered(&[(3, "3a"), (20, "20a"), (27, "27a")]));
        f.write("g", "1\n2a\n3\n4\n5\n");
        f.run(&["commit", "-q", "-am", "c1"]);
        f.run(&["checkout", "-q", "-b", "b", "main"]);
        f.write("f", &numbered(&[(3, "3b"), (20, "20a\n"), (27, "27b")]));
        f.write("g", "1\n2a\n3\n4\n5\n");
        f.run(&["commit", "-q", "-am", "c1"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
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

    /// `range-diff --creation-factor=200 <extra> main..a main..b`, asserting a
    /// clean exit.
    fn range_diff(&self, extra: &[&str]) -> String {
        let mut argv = vec!["range-diff", "--creation-factor=200"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["main..a", "main..b"]);
        let (out, err, code) = self.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "{argv:?}");
        out
    }
}

const HEADER: &str = "1:  7c9b088 ! 1:  14ea31f c1\n";

#[test]
fn the_modified_pair_survives_only_a_filter_that_selects_m() {
    let f = Fixture::new("select");
    let full = f.range_diff(&[]);
    assert!(full.len() > HEADER.len() && full.starts_with(HEADER), "{full}");
    for kept in [&["--diff-filter=M"][..], &["--diff-filter=d"], &["--diff-filter=*M"], &["--diff-filter=A", "--diff-filter=M"]] {
        assert_eq!(f.range_diff(kept), full, "{kept:?}");
    }
    for dropped in [&["--diff-filter=A"][..], &["--diff-filter", "m"], &["--diff-filter=*"], &["--diff-filter=B"], &["--diff-filter=A", "--raw"]] {
        assert_eq!(f.range_diff(dropped), HEADER, "{dropped:?}");
    }
}

#[test]
fn an_unknown_change_class_is_a_parse_error() {
    let f = Fixture::new("bad");
    let (out, err, code) = f.run(&["range-diff", "--diff-filter=a1", "main..a", "main..b"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: unknown change class '1' in --diff-filter=a1\n", 129)
    );
}
