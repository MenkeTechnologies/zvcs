//! `git range-diff -S`, `-G` and `--find-object`.
//!
//! `add_diff_options()` binds the pickaxe to range-diff's `diffopt`
//! (builtin/range-diff.c:83), and `patch_diff()` runs `diffcore_std()` over the
//! one filepair it queues (range-diff.c:491-498), which calls
//! `diffcore_pickaxe()` before `diff_flush()` (diff.c:7517-7518). A pair the
//! filter drops leaves an empty queue, and `diff_flush()` returns without a byte
//! in any format (diff.c:7197), so only the pair header is left. `-S` compares
//! occurrence counts in the two patch texts (`has_changes()`), `-G` greps the
//! outer diff's changed lines (`diff_grep()`), and `--find-object` tests the two
//! filespecs' ids, which `get_filespec()` sets to the null id
//! (range-diff.c:477-489, diffcore-pickaxe.c:140-145). zvcs stopped with
//! `fatal: unsupported flag`.
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
            .join(format!("zvcs-range-diff-pickaxe-{tag}-{}", std::process::id()));
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
fn s_keeps_the_pair_only_when_the_counts_differ() {
    let f = Fixture::new("s");
    let full = f.range_diff(&[]);
    assert!(full.len() > HEADER.len() && full.starts_with(HEADER), "{full}");
    // `3a` is in the old patch only.
    assert_eq!(f.range_diff(&["-S3a"]), full);
    assert_eq!(f.range_diff(&["-S", "3a", "--pickaxe-all"]), full);
    // `27` appears twice on each side (`-27` and `+27a` / `+27b`).
    assert_eq!(f.range_diff(&["-S27"]), HEADER);
    assert_eq!(f.range_diff(&["-S2", "--pickaxe-regex"]), HEADER);
    // A dropped pair writes nothing in any format; a kept one is listed.
    assert_eq!(f.range_diff(&["-Szz", "--raw"]), HEADER);
    assert_eq!(
        f.range_diff(&["-S3a", "--raw"]),
        format!("{HEADER}    :100644 100644 0000000 0000000 M\ta\n")
    );
}

#[test]
fn g_greps_only_the_changed_outer_lines() {
    let f = Fixture::new("g");
    let full = f.range_diff(&[]);
    // `20a` is outer context; `3b` is an added outer line.
    assert_eq!(f.range_diff(&["-G20a"]), HEADER);
    assert_eq!(f.range_diff(&["-G", "3[ab]"]), full);
}

#[test]
fn find_object_matches_only_the_null_id() {
    let f = Fixture::new("objfind");
    let full = f.range_diff(&[]);
    assert_eq!(
        f.range_diff(&["--find-object=0000000000000000000000000000000000000000"]),
        full
    );
    assert_eq!(f.range_diff(&["--find-object=HEAD"]), HEADER);
}
