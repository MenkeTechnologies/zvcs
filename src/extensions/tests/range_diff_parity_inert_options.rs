//! `git range-diff` options that cannot change the one outer filepair.
//!
//! `patch_diff()` queues a single *modified* pair, `a` → `b`, both valid, both
//! mode 0100644, built by `get_filespec()` (range-diff.c:477-489) and queued with
//! `diff_queue()` (range-diff.c:494-495). Against that pair these options leave
//! every format byte-identical to the flagless run in git 2.55.0:
//!
//! * `-M`/`--find-renames`, `-C`/`--find-copies`, `--find-copies-harder`,
//!   `--no-renames`, `--[no-]rename-empty`, `-l<n>` (diff.c:6167-6190):
//!   `diffcore_rename()` pairs deletions with creations, and there are none;
//! * `-D`/`--irreversible-delete`: read for a deleted pair only;
//! * `-R`: `diff_change()`/`diff_addremove()` swap the sides (diff.c:7625,
//!   7667), which `diff_queue()` bypasses, and the swapped prefixes only reach
//!   headers range-diff suppresses;
//! * `-a`/`--text`: a patch text has no NUL for the binary test to find;
//! * `--no-ext-diff`, `--rotate-to`, `--skip-to` (non-strict rotation,
//!   diffcore-rotate.c:20-32).
//!
//! The `-M`/`-C` score and the `-l` limit are still validated at parse time.
//! zvcs stopped every one of them with `fatal: unsupported flag`.
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
            .join(format!("zvcs-range-diff-inert-{tag}-{}", std::process::id()));
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

#[test]
fn rename_reverse_text_and_rotation_options_leave_every_format_alone() {
    let f = Fixture::new("noop");
    for format in [&[][..], &["--raw"], &["--name-status"], &["--stat"], &["--numstat"], &["--color"]] {
        let plain = f.range_diff(format);
        for option in [
            &["-M"][..],
            &["-M50%"],
            &["--find-renames=40"],
            &["--no-renames"],
            &["-C"],
            &["-C30"],
            &["--find-copies-harder"],
            &["--rename-empty"],
            &["--no-rename-empty"],
            &["-l5"],
            &["-l", "1k"],
            &["-D"],
            &["--irreversible-delete"],
            &["-R"],
            &["-a"],
            &["--text"],
            &["--no-ext-diff"],
            &["--rotate-to=a"],
            &["--skip-to", "zzz"],
        ] {
            let mut argv = format.to_vec();
            argv.extend_from_slice(option);
            assert_eq!(f.range_diff(&argv), plain, "{argv:?}");
        }
    }
}

#[test]
fn scores_and_limits_are_validated_before_the_ranges() {
    let f = Fixture::new("values");
    for (option, message) in [
        ("-Mfoo", "error: invalid argument to find-renames\n"),
        ("--find-renames=x", "error: invalid argument to find-renames\n"),
        ("-C1x", "error: invalid argument to find-copies\n"),
        ("-lfoo", "error: switch `l' expects an integer value with an optional k/m/g suffix\n"),
        (
            "-l99999999999",
            "error: value 99999999999 for switch `l' not in range [-2147483648,2147483647]\n",
        ),
    ] {
        let (out, err, code) = f.run(&["range-diff", option, "main..a", "main..b"]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", message, 129), "{option}");
    }
    let (out, err, code) = f.run(&["range-diff", "main..a", "main..b", "-l"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: switch `l' requires a value\n", 129)
    );
}
