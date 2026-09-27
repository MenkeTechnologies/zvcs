//! `git range-diff -z`.
//!
//! `-z` sets `line_termination` to NUL, and the formats `diff_flush()` writes
//! for range-diff's one filepair follow it: `diff_flush_raw()` separates the
//! status from the name with NUL instead of a tab and ends the name with NUL
//! (diff.c:6471-6501), `--name-only` ends `b` with NUL, `show_numstat()` writes
//! the renamed row as `<added>\t<deleted>\t\0a\0b\0`, and the separator before
//! the patch is the line prefix followed by `line_termination`
//! (diff.c:1436-1440). The patch and stat text keep their newlines. zvcs
//! stopped with `fatal: unsupported flag "-z"`.
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
            .join(format!("zvcs-range-diff-z-{tag}-{}", std::process::id()));
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
const RAW: &str = "    :100644 100644 0000000 0000000 M\0a\0";
const NUMSTAT: &str = "    3\t2\t\0a\0b\0";

#[test]
fn name_formats_end_their_fields_with_nul() {
    let f = Fixture::new("names");
    assert_eq!(f.range_diff(&["-z", "--raw"]), format!("{HEADER}{RAW}"));
    assert_eq!(f.range_diff(&["-z", "--name-only"]), format!("{HEADER}    b\0"));
    assert_eq!(f.range_diff(&["-z", "--name-status"]), format!("{HEADER}    M\0a\0"));
    assert_eq!(f.range_diff(&["-z", "--numstat"]), format!("{HEADER}{NUMSTAT}"));
}

#[test]
fn the_patch_keeps_its_newlines_and_the_separator_is_nul() {
    let f = Fixture::new("patch");
    let plain = f.range_diff(&[]);
    assert_eq!(f.range_diff(&["-z"]), plain);
    assert_eq!(f.range_diff(&["-z", "--stat"]), f.range_diff(&["--stat"]));
    let body = plain.strip_prefix(HEADER).expect("the page opens with the pair header");
    assert_eq!(
        f.range_diff(&["-z", "--raw", "--numstat", "-p"]),
        format!("{HEADER}{RAW}{NUMSTAT}    \0{body}")
    );
}
