//! `git range-diff -W`, `--inter-hunk-context`, `--ignore-blank-lines` and `-I`.
//!
//! All four configure the *outer* diff — `add_diff_options()` binds the whole
//! `git diff` table to range-diff's `diffopt` (builtin/range-diff.c:83) — and
//! all four reach `xdl_emit_diff()` rather than the comparison:
//!
//! * `-W` is `flags.funccontext` (diff.c:6054), which `builtin_diff()` turns into
//!   `XDL_EMIT_FUNCCONTEXT` (diff.c:4061-4062). The function lines are the ones
//!   the `section_headers` driver matches (range-diff.c:470-475), so an outer hunk
//!   grows back to the ` ## <path> ##` or inner `@@` line above it and forward to
//!   the next one.
//! * `--inter-hunk-context=<n>` is `xecfg.interhunkctxlen`, merging outer hunks
//!   whose gap is at most `<n>` lines.
//! * `--ignore-blank-lines` (diff.c:6208-6210) and `-I<regex>` (diff.c:5859-5877)
//!   mark a change whose every record is blank, or matches, as ignorable
//!   (`xdl_mark_ignorable_lines()` / `xdl_mark_ignorable_regex()`), and
//!   `xdl_get_hunk()` opens no hunk for it. `builtin_diffstat()` passes the same
//!   `xpp` (diff.c:4241-4250), so `--numstat` drops the ignored lines too. A bad
//!   pattern is the 129 `error: invalid regex given to -I` at parse time.
//!
//! zvcs deferred all four and stopped with `fatal: unsupported flag` once a pair
//! had a body.
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
    /// `a` and `b` each rewrite lines 3, 20 and 27 of `f` and line 2 of `g`;
    /// `b` differs from `a` at lines 3 and 27 and adds a blank line after 20.
    /// `c` and `d` carry the same tree and messages that differ only by a blank
    /// line.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-geometry-{tag}-{}", std::process::id()));
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

        for (branch, message) in [("c", "c1\n\nbody\n\nmore\n"), ("d", "c1\n\nbody\n\n\nmore\n")] {
            f.run(&["checkout", "-q", "-b", branch, "main"]);
            f.write("f", &numbered(&[(3, "3a"), (20, "20a"), (27, "27a")]));
            f.write("g", "1\n2a\n3\n4\n5\n");
            f.run(&["commit", "-q", "-a", "--cleanup=verbatim", "-m", message]);
        }
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

/// The outer lines from `17` through `30`, shared by the `-W` and
/// `--inter-hunk-context` pages.
const TAIL: &str = "      17\n      18\n      19\n     -20\n     +20a\n    ++\n      21\n      22\n      23\n      24\n      25\n      26\n     -27\n    -+27a\n    ++27b\n";

#[test]
fn function_context_grows_to_the_section_header_lines() {
    let f = Fixture::new("w");
    let want = format!(
        "{HEADER}    @@ Commit message\n      ## f ##\n     @@\n      1\n      2\n     -3\n    -+3a\n    ++3b\n      4\n      5\n      6\n     @@\n{TAIL}      28\n      29\n      30\n"
    );
    assert_eq!(f.range_diff(&["-W"]), want);
    assert_eq!(f.range_diff(&["--function-context", "-U1"]), want);
    // `OPT_BOOL`: the negation restores the three ordinary hunks.
    let plain = f.range_diff(&[]);
    assert_eq!(f.range_diff(&["-W", "--no-function-context"]), plain);
    assert_eq!(plain.matches("    @@ f\n").count(), 3, "{plain}");
}

#[test]
fn inter_hunk_context_merges_nearby_hunks() {
    let f = Fixture::new("ihc");
    assert_eq!(
        f.range_diff(&["-U0", "--inter-hunk-context=20"]),
        format!("{HEADER}    @@ f\n    -+3a\n    ++3b\n      4\n      5\n      6\n     @@\n{TAIL}")
    );
}

#[test]
fn ignore_matching_lines_drops_the_matching_hunks_and_their_counts() {
    let f = Fixture::new("i");
    let only_20 = format!(
        "{HEADER}    @@ f\n      19\n     -20\n     +20a\n    ++\n      21\n      22\n      23\n"
    );
    assert_eq!(f.range_diff(&["-I3", "-I27"]), only_20);
    assert_eq!(f.range_diff(&["--ignore-matching-lines=3", "-I", "27"]), only_20);
    assert_eq!(f.range_diff(&["--numstat", "-I3"]), format!("{HEADER}    2\t1\ta => b\n"));

    let (out, err, code) = f.run(&["range-diff", "-I(", "main..a", "main..b"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: invalid regex given to -I: '('\n", 129)
    );
    let (out, err, code) = f.run(&["range-diff", "main..a", "main..b", "-I"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: switch `I' requires a value\n", 129)
    );
}

#[test]
fn ignore_blank_lines_hides_a_message_that_gained_a_blank_line() {
    let f = Fixture::new("blank");
    let run = |extra: &[&str]| {
        let mut argv = vec!["range-diff"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["main..c", "main..d"]);
        let (out, err, code) = f.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "{argv:?}");
        out
    };
    let header = "1:  e38ef3a ! 1:  5b17444 c1\n";
    assert_eq!(
        run(&[]),
        format!("{header}    @@ Commit message\n     \n         body\n     \n    +\n         more\n     \n      ## f ##\n")
    );
    assert_eq!(run(&["--ignore-blank-lines"]), header);
    assert_eq!(run(&["--ignore-blank-lines", "-U0"]), header);
    // `builtin_diffstat()` drops a modified pair with nothing counted
    // (diff.c:4256-4273), so no numstat row is written at all.
    assert_eq!(run(&["--numstat", "--ignore-blank-lines"]), header);
    assert_eq!(run(&["--stat", "--ignore-blank-lines"]), header);
}
