//! `handle_revision_opt()`'s count-and-age arm, which every history-walking verb
//! inherits from `setup_revisions()` rather than from its own option table.
//!
//! ```c
//! if ((argcount = parse_long_opt("max-count", argv, &optarg))) {
//!         if (revs->max_count_type == 1)
//!                 die_for_incompatible_opt2(1, "--max-count", 1, "--max-count-oldest");
//!         revs->max_count = parse_count(optarg);
//!         revs->no_walk = 0;
//!         revs->max_count_type = 0;
//!         return argcount;
//! } else if ((argcount = parse_long_opt("max-count-oldest", argv, &optarg))) {
//!         ...
//! } else if ((*arg == '-') && isdigit(arg[1])) {
//!         /* accept -<digit>, like traditional "head" */
//!         revs->max_count = parse_count(arg + 1);
//!         revs->no_walk = 0;
//! }
//! ```
//!
//! (`revision.c:2341-2399`, v2.55.0.) Four shapes this file pins, each of which
//! `log`, `rev-list`, `show`, `shortlog` and `whatchanged` had re-derived
//! separately and got wrong in different ways:
//!
//! 1. `-<digits>` is gated on `isdigit(arg[1])` **alone**, so `-1x` enters the
//!    arm and dies in `parse_count()` — it is not an unknown option.
//! 2. every long spelling takes its value attached *or* in the next argv slot
//!    (`parse_long_opt()`, `diff.c:5380-5399`), and a missing one is that
//!    function's `die("Option '--%s' requires a value")`.
//! 3. the value parsers are C's `strtol`/`strtoumax`: leading whitespace and a
//!    sign are consumed, and a value that will not survive the round trip
//!    through `int` is refused.
//! 4. `--max-count-oldest` is the single arm with **no** `return argcount`, so
//!    its separate form consumes only the option word and leaves the value
//!    behind as a revision. It keeps the *last* `max_count` commits of the walk,
//!    still in walk order (`retrieve_oldest_commits()`, `revision.c:4596-4657`).
//!
//! Every expectation below was measured against git 2.55.0 on this exact
//! fixture. Dates are pinned so the walk order and the age cutoffs are fixed.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The committer date of `C1`; each later commit is 100 seconds after it, so
/// `1_700_000_250` falls between `C2` and `C3`.
const BASE: i64 = 1_700_000_100;

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    tick: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Four linear commits `C1..C4`, so the walk is `C4 C3 C2 C1` and every
    /// slice below reads off one end or the other.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-revopt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, tick: BASE };
        f.git(&["init", "-q", "-b", "main", "."]);
        for msg in ["C1", "C2", "C3", "C4"] {
            std::fs::write(f.work.join("f.txt"), format!("{msg}\n")).unwrap();
            f.git(&["add", "f.txt"]);
            f.git(&["commit", "-q", "-m", msg]);
            f.tick += 100;
        }
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", format!("{} +0000", self.tick))
            .env("GIT_COMMITTER_DATE", format!("{} +0000", self.tick))
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .stdin(std::process::Stdio::null());
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// stdout lines of a run that must succeed.
    fn ok(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: rc={:?} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// `(exit code, first stderr line)` of a run that must fail.
    fn err(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        let first = String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        (out.status.code().unwrap_or(-1), first)
    }
}

/// `-<digits>` is entered on the second byte being a digit, so the rest of the
/// word is `parse_count()`'s problem, not the unknown-option path's. Every verb
/// here had spelled the test as "all of the remaining bytes are digits", which
/// made `git log -1x` an unrecognised argument instead of a fatal.
#[test]
fn a_digit_second_byte_enters_the_max_count_arm_whatever_follows_it() {
    let f = Fixture::new("digit");
    for verb in [
        vec!["log", "-1x"],
        vec!["rev-list", "-1x", "HEAD"],
        vec!["show", "-1x"],
        vec!["shortlog", "-1x", "HEAD"],
        vec!["whatchanged", "--i-still-use-this", "-1x"],
    ] {
        let (code, line) = f.err(&verb);
        assert_eq!(
            (code, line.as_str()),
            (128, "fatal: '1x': not an integer"),
            "{verb:?}"
        );
    }
    // The arm itself still works, and `-01` is one commit, not an unknown option.
    assert_eq!(f.ok(&["log", "--format=%s", "-01"]), ["C4"]);
    assert_eq!(f.ok(&["log", "--format=%s", "-0"]), Vec::<String>::new());
}

/// `parse_long_opt()` takes the value attached or in the next argv slot. Only
/// `log` accepted the separate form; `rev-list`, `show` and `shortlog` refused
/// it outright, and `shortlog` had no `--min-age`/`--max-age` at all.
#[test]
fn the_separate_value_form_reaches_every_walking_verb() {
    let f = Fixture::new("sep");
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count", "2"]), ["C4", "C3"]);
    assert_eq!(
        f.ok(&["log", "--format=%s", "--skip", "1", "--max-count", "2"]),
        ["C3", "C2"]
    );
    assert_eq!(
        f.ok(&["rev-list", "--format=%s", "--max-count", "2", "HEAD"])
            .iter()
            .filter(|l| !l.starts_with("commit "))
            .cloned()
            .collect::<Vec<_>>(),
        ["C4", "C3"]
    );
    assert_eq!(f.ok(&["show", "-s", "--format=%s", "--max-count", "2"]), ["C4", "C3"]);
    // `--min-age`/`--max-age` are `--until`/`--since` read as a raw epoch by
    // `parse_age()` (revision.c:2379-2393), and they set the same two fields.
    assert_eq!(
        f.ok(&["shortlog", "--max-age", "1700000250", "HEAD"]),
        ["A U Thor (2):", "      C3", "      C4", ""]
    );
    assert_eq!(
        f.ok(&["shortlog", "--min-age", "1700000250", "HEAD"]),
        ["A U Thor (2):", "      C1", "      C2", ""]
    );
    // `--since-as-filter` must not be swallowed by `--since`'s prefix: the
    // `*arg != '\0'` guard in `parse_long_opt` is what keeps them apart.
    assert_eq!(
        f.ok(&["shortlog", "--since-as-filter", "1700000250", "HEAD"]),
        ["A U Thor (2):", "      C3", "      C4", ""]
    );
    assert_eq!(f.ok(&["show", "-s", "--format=%s", "--max-age", "1700000250"]), ["C4"]);
}

/// The separate form running off the end of argv is `parse_long_opt()`'s
/// `die()`, exit 128 — not this port's own "requires a value" at exit 1, and not
/// silence. `git log --since` with nothing after it used to read the value as
/// the empty string and list the whole history.
#[test]
fn a_missing_separate_value_is_the_parse_long_opt_die() {
    let f = Fixture::new("missing");
    for (name, verb) in [
        ("--max-count", "log"),
        ("--skip", "log"),
        ("--since", "log"),
        ("--until", "log"),
        ("--after", "log"),
        ("--before", "log"),
        ("--min-age", "log"),
        ("--max-age", "log"),
        ("--max-count", "rev-list"),
        ("--skip", "shortlog"),
        ("--since", "show"),
    ] {
        let (code, line) = f.err(&[verb, name]);
        assert_eq!(
            (code, line.as_str()),
            (128, format!("fatal: Option '{name}' requires a value").as_str()),
            "{verb} {name}"
        );
    }
    // `-n` alone is `error()` inside `handle_revision_opt()`, a different
    // wording that still exits 128 (revision.c:2370-2372).
    assert_eq!(
        f.err(&["log", "-n"]),
        (128, "error: -n requires an argument".to_string())
    );
}

/// `strtol`'s own lexing, which the port had replaced with Rust's `str::parse`:
/// leading whitespace and a `+` are consumed, and a value too wide for `int` is
/// refused even though it fits a `long`.
#[test]
fn counts_are_lexed_by_strtol_not_by_rust() {
    let f = Fixture::new("strtol");
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count= 2"]), ["C4", "C3"]);
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count=+2"]), ["C4", "C3"]);
    assert_eq!(f.ok(&["log", "--format=%s", "--skip= 2"]), ["C2", "C1"]);
    // `(int) ul != ul` (git-compat-util.h:985).
    for bad in ["3000000000", "-3000000000", "0x2", "2x", ""] {
        let (code, line) = f.err(&["log", &format!("--max-count={bad}")]);
        assert_eq!(
            (code, line.as_str()),
            (128, format!("fatal: '{bad}': not an integer").as_str()),
            "--max-count={bad}"
        );
    }
    // A negative count is git's "no limit" and a negative skip skips nothing,
    // because the walk only ever tests `max_count`/`skip_count` against zero.
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count=-1"]).len(), 4);
    assert_eq!(f.ok(&["log", "--format=%s", "--skip=-1"]).len(), 4);
}

/// `--max-count-oldest` keeps the last `max_count` commits of the walk, in walk
/// order — and its separate form leaves the value behind as a revision, because
/// its arm is the one with no `return argcount`.
#[test]
fn max_count_oldest_slices_the_far_end_and_eats_only_its_own_word() {
    let f = Fixture::new("oldest");
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count-oldest=2"]), ["C2", "C1"]);
    assert_eq!(
        f.ok(&["rev-list", "--format=%s", "--max-count-oldest=2", "HEAD"])
            .iter()
            .filter(|l| !l.starts_with("commit "))
            .cloned()
            .collect::<Vec<_>>(),
        ["C2", "C1"]
    );
    assert_eq!(
        f.ok(&["show", "-s", "--format=%s", "--max-count-oldest=2"]),
        ["C2", "C1"]
    );
    assert_eq!(
        f.ok(&["shortlog", "--max-count-oldest=2", "HEAD"]),
        ["A U Thor (2):", "      C1", "      C2", ""]
    );
    // A count larger than the history is the whole history, from the same end.
    assert_eq!(f.ok(&["log", "--format=%s", "--max-count-oldest=9"]).len(), 4);
    // No `return argcount`: the `2` is left in argv and read as a revision.
    let (code, line) = f.err(&["log", "--max-count-oldest", "2"]);
    assert_eq!(code, 128);
    assert!(
        line.starts_with("fatal: ambiguous argument '2'"),
        "separate form must leave its value behind, got {line:?}"
    );
}

/// The two `die_for_incompatible_opt2()` calls, whose named order is fixed by
/// the call sites and so never depends on which option was typed first.
#[test]
fn max_count_oldest_conflicts_with_max_count_and_with_skip() {
    let f = Fixture::new("conflict");
    const MC: &str =
        "fatal: options '--max-count' and '--max-count-oldest' cannot be used together";
    const SK: &str = "fatal: options '--skip' and '--max-count-oldest' cannot be used together";
    for verb in ["log", "rev-list", "shortlog"] {
        assert_eq!(
            f.err(&[verb, "--max-count=1", "--max-count-oldest=2"]),
            (128, MC.to_string()),
            "{verb}"
        );
        assert_eq!(
            f.err(&[verb, "--max-count-oldest=2", "--max-count=1"]),
            (128, MC.to_string()),
            "{verb}"
        );
        assert_eq!(
            f.err(&[verb, "--max-count-oldest=2", "--skip=1"]),
            (128, SK.to_string()),
            "{verb}"
        );
        assert_eq!(
            f.err(&[verb, "--skip=1", "--max-count-oldest=2"]),
            (128, SK.to_string()),
            "{verb}"
        );
    }
    // `--max-count=-1` leaves the field at its `-1` sentinel, so it is not
    // "already set" for the conflict test (revision.c:2350).
    assert_eq!(
        f.ok(&["log", "--format=%s", "--max-count=-1", "--max-count-oldest=2"]),
        ["C2", "C1"]
    );
}

/// `handle_revision_opt()`'s last arm hands what it does not claim to
/// `diff_opt_parse()` (revision.c:2720-2724), which is how the colour family
/// reaches `shortlog` — a command that renders no diff at all
/// (builtin/shortlog.c:444). It refused every spelling before.
#[test]
fn shortlog_forwards_the_colour_family_to_diff_opt_parse() {
    let f = Fixture::new("color");
    let expected = ["A U Thor (4):", "      C1", "      C2", "      C3", "      C4", ""];
    for flag in [
        "--color",
        "--no-color",
        "--color=always",
        "--color=never",
        "--color=auto",
        "--color-moved",
        "--no-color-moved",
        "--color-moved=zebra",
        "--color-words",
    ] {
        assert_eq!(f.ok(&["shortlog", flag, "HEAD"]), expected, "{flag}");
    }
    // The values are still checked, by the same callbacks, before anything is
    // resolved — accepting the option must not mean accepting any value.
    assert_eq!(
        f.err(&["shortlog", "--color=bogus", "HEAD"]),
        (
            129,
            "error: option `color' expects \"always\", \"auto\", or \"never\"".to_string()
        )
    );
    let (code, line) = f.err(&["shortlog", "--color-moved=bogus", "HEAD"]);
    assert_eq!(code, 129);
    assert!(
        line.starts_with("error: color moved setting must be one of"),
        "got {line:?}"
    );
}

/// `parse_options_step()` rewrites `ctx->argv[0]` to `-<rest of the cluster>`
/// before returning PARSE_OPT_UNKNOWN, so the digits left over from a short
/// option shortlog *does* own still reach the revision parser: `-n2` is
/// `--numbered` and `--max-count=2` at once.
#[test]
fn shortlog_hands_the_tail_of_a_short_cluster_to_the_revision_parser() {
    let f = Fixture::new("cluster");
    assert_eq!(
        f.ok(&["shortlog", "-n2", "HEAD"]),
        ["A U Thor (2):", "      C3", "      C4", ""]
    );
    assert_eq!(
        f.ok(&["shortlog", "-2", "HEAD"]),
        ["A U Thor (2):", "      C3", "      C4", ""]
    );
    // And the leftover is still `parse_count()`'s to reject.
    assert_eq!(
        f.err(&["shortlog", "-n2x", "HEAD"]),
        (128, "fatal: '2x': not an integer".to_string())
    );
}
