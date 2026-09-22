//! Dates read off an ident line git cannot take at face value.
//!
//! `show_ident_date()` (pretty.c:442-459) does not decode a commit's date into a
//! timestamp and then render it. It reads two *spans* out of the raw header line
//! — the digits `split_ident_line()` found (ident.c:328-342) and the `[-+]HHMM`
//! after them — and renders those, with a fixed set of escape hatches for spans
//! that are absent, non-numeric or too large. The escape hatches are the whole
//! subject of this file, because each one is a different output and none of them
//! is an error:
//!
//!   * **No date span at all** — the field is missing, whitespace, or begins with
//!     a `-` (the scan is `strspn(cp, "0123456789")`, so a sign ends it at zero
//!     length). Every date placeholder renders empty, but the `Date:` header of
//!     the medium format still prints, as the epoch in `+0000`.
//!   * **A span too large for `time_t`** — `parse_timestamp` is `strtoumax`, which
//!     saturates, and `date_overflows()` (date.c:1431-1446) then forces both the
//!     timestamp *and the zone* to zero. `%at` still prints the recorded digits
//!     verbatim, so `%ad` and `%at` disagree on purpose.
//!   * **A span `gmtime_r()` refuses** — in range for `time_t`, out of range for a
//!     calendar. date.c:330-333 falls back to the epoch with a zero zone.
//!
//! And one case where git is *more* faithful than a decode-and-reformat would
//! be: a zone like `+9999` is not a real offset, and a port that normalises it
//! into minutes cannot get it back. git never leaves the `[-+]HHMM` integer, so
//! it prints what was recorded.
//!
//! Every expectation here is stock git 2.55.0's output for the same object, with
//! `TZ` pinned per invocation and fixed timestamps throughout — nothing below
//! reads the wall clock except `%ar`, which is pinned with `GIT_TEST_DATE_NOW`.
//! The corpus is git's own `t/t4212-log-corrupt.sh`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The commit the fixtures are munged from: `1112911993 -0700`, git's own
/// `t4212` timestamp.
const GOOD_DATE: &str = "1112911993 -0700";
/// A fixed "now" for the one relative-date assertion, so `%ar` is a constant.
const NOW: &str = "1790000000";

fn run_in(dir: &Path, tz: &str, args: &[&str]) -> Output {
    run_with_stdin(dir, tz, args, &[])
}

fn run_with_stdin(dir: &Path, tz: &str, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", tz)
        .env("GIT_TEST_DATE_NOW", NOW)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "A U Thor")
        .env("GIT_COMMITTER_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", format!("@{GOOD_DATE}"))
        .env("GIT_COMMITTER_DATE", format!("@{GOOD_DATE}"))
        .env_remove("GIT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zvcs git");
    {
        use std::io::Write;
        child.stdin.as_mut().expect("stdin").write_all(stdin).expect("write stdin");
    }
    child.wait_with_output().expect("run zvcs git")
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-datecorrupt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.canonicalize().expect("canonicalize")
}

/// A repository with one good commit, plus a loose object per entry of `munged`
/// holding that commit with its **author** date field replaced.
///
/// The objects are written with `hash-object --literally` precisely because they
/// are not idents any writer would produce: the point is what the *reader* does
/// with them.
fn corrupt_repo(tag: &str, munged: &[(&str, &str)]) -> (PathBuf, Vec<String>) {
    corrupt_repo_role(tag, "author", munged)
}

/// As [`corrupt_repo`], but choosing which ident line is broken. The revision
/// walk's clock is the *committer* date (`parse_commit_date()` requires the
/// `committer` header, commit.c:141-142), so a test about walking has to break
/// that one; the pretty placeholders read whichever they are asked for.
fn corrupt_repo_role(tag: &str, role: &str, munged: &[(&str, &str)]) -> (PathBuf, Vec<String>) {
    let dir = tmp(tag);
    assert!(run_in(&dir, "UTC", &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(dir.join("foo"), "foo\n").expect("write");
    assert!(run_in(&dir, "UTC", &["add", "foo"]).status.success(), "add");
    let o = run_in(&dir, "UTC", &["commit", "-q", "-m", "foo"]);
    assert_eq!(code(&o), 0, "commit: {}", err(&o));

    let base = text(&run_in(&dir, "UTC", &["cat-file", "commit", "HEAD"]));
    let ident_line = format!("{role} A U Thor <author@example.com> {GOOD_DATE}\n");
    assert!(base.contains(&ident_line), "fixture base commit is not as expected: {base}");

    let mut ids = Vec::new();
    for (_, field) in munged {
        let replaced = base.replace(
            &ident_line,
            &format!("{role} A U Thor <author@example.com> {field}\n"),
        );
        assert_ne!(replaced, base, "munge for {field:?} changed nothing");
        let o = run_with_stdin(
            &dir,
            "UTC",
            &["hash-object", "-t", "commit", "-w", "--stdin", "--literally"],
            replaced.as_bytes(),
        );
        assert_eq!(code(&o), 0, "hash-object {field:?}: {}", err(&o));
        ids.push(text(&o).trim_end().to_string());
    }
    (dir, ids)
}

/// One `log -1 --format=<fmt>` reading, with `TZ` and "now" pinned.
fn fmt_of(dir: &Path, tz: &str, id: &str, fmt: &str, date_mode: &str) -> String {
    let o = run_in(dir, tz, &["log", "-1", &format!("--format={fmt}"), &format!("--date={date_mode}"), id]);
    assert_eq!(code(&o), 0, "log {fmt} {date_mode}: {}", err(&o));
    text(&o).trim_end_matches('\n').to_string()
}

/// A timestamp `strtoumax` saturates on (2^64 + 1) and one that fits in
/// `uintmax_t` but not in a signed `time_t` (2^64 - 2) are both
/// `date_overflows()`, and git's answer is the epoch in `+0000` — *not* the
/// recorded `-0700`, because pretty.c:452-457 only reads the zone in the
/// non-overflowing branch. `%at` is unaffected: it copies the digit span.
#[test]
fn an_overflowing_timestamp_renders_as_the_epoch_in_utc_but_prints_back_verbatim() {
    let (dir, ids) = corrupt_repo(
        "overflow",
        &[("two_to_the_64_plus_1", "18446744073709551617 -0700"), ("time_t_overflow", "18446744073709551614 -0700")],
    );
    for id in &ids {
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "default"), "Thu Jan 1 00:00:00 1970 +0000");
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso"), "1970-01-01 00:00:00 +0000");
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso-strict"), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "rfc"), "Thu, 1 Jan 1970 00:00:00 +0000");
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "short"), "1970-01-01");
        // The zone is forced to `+0000` even though the ident recorded `-0700`.
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "raw"), "0 +0000");
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "unix"), "0");
        // `GIT_TEST_DATE_NOW` is 1790000000, so "now minus the epoch" is fixed.
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "relative"), "57 years ago");
    }
    // `%at` is the recorded digits, so it and `%ad` deliberately disagree.
    assert_eq!(fmt_of(&dir, "UTC", &ids[0], "%at", "iso"), "18446744073709551617");
    assert_eq!(fmt_of(&dir, "UTC", &ids[1], "%at", "iso"), "18446744073709551614");
    // The committer line is untouched, which is what proves only the broken half
    // took the sentinel.
    assert_eq!(fmt_of(&dir, "UTC", &ids[0], "%ad|%cd", "iso"), "1970-01-01 00:00:00 +0000|2005-04-07 15:13:13 -0700");
}

/// `--pretty=raw` reproduces the object's own header, so a timestamp no integer
/// type can hold has to survive the round trip byte-for-byte.
#[test]
fn pretty_raw_round_trips_an_overflowing_date_field() {
    let (dir, ids) = corrupt_repo("rawroundtrip", &[("overflow", "18446744073709551617 -0700")]);
    let o = run_in(&dir, "UTC", &["log", "-1", "--pretty=raw", &ids[0]]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        text(&o).contains("author A U Thor <author@example.com> 18446744073709551617 -0700\n"),
        "raw must echo the stored header: {}",
        text(&o)
    );
}

/// A date field that is not digits — a word, or a leading `-` — leaves
/// `split_ident_line()` in its `person_only` state. Every *consumed* date
/// placeholder (`%at %ad %aD %ar %ai`) is then empty, and the `Date:` header of
/// the medium format falls back to the epoch sentinel instead.
#[test]
fn a_non_numeric_date_field_empties_the_placeholders_but_not_the_date_header() {
    let (dir, ids) = corrupt_repo("nodate", &[("negative", "-1 -0700")]);
    let id = &ids[0];
    for mode in ["default", "iso", "raw", "unix", "relative", "human", "short", "iso-strict", "rfc"] {
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", mode), "", "%ad under --date={mode}");
        assert_eq!(fmt_of(&dir, "UTC", id, "%at", mode), "", "%at under --date={mode}");
    }
    // The committer half is intact, so the empties above are the author's alone.
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad|%cd", "iso"), "|2005-04-07 15:13:13 -0700");
    assert_eq!(fmt_of(&dir, "UTC", id, "%at:%ct", "iso"), ":1112911993");

    // `show_ident_date()` is called unconditionally for the header (pretty.c:607),
    // so with no date span it renders `date = 0, tz = 0`.
    let o = run_in(&dir, "UTC", &["log", "-1", "--pretty=medium", id]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        text(&o).contains("Date:   Thu Jan 1 00:00:00 1970 +0000\n"),
        "medium keeps a sentinel Date: header: {}",
        text(&o)
    );
}

/// The `skip:` label splits the date placeholders in two (pretty.c:862-866): it
/// returns `placeholder_len` for `t d D r i`, consuming them and printing
/// nothing, but `return 0` for `I h s`, which is "unknown placeholder" and
/// leaves `%aI`, `%ah`, `%as` on the line *as typed*. Only a dateless ident can
/// tell the two groups apart, which is why this is easy to flatten into "all
/// empty" and why the difference is pinned here.
#[test]
fn a_dateless_ident_consumes_five_date_placeholders_and_prints_three_literally() {
    let (dir, ids) = corrupt_repo("skiparm", &[("negative", "-1 -0700")]);
    assert_eq!(
        fmt_of(&dir, "UTC", &ids[0], "%aD|%aI|%ai|%ah|%as|%ar|%at|%ad", "iso"),
        "|%aI||%ah|%as|||"
    );
    // A good ident renders all eight, so the literals above are the dateless
    // path and not a missing implementation.
    let good = fmt_of(&dir, "UTC", "HEAD", "%aI|%ah|%as", "iso");
    assert_eq!(good, "2005-04-07T15:13:13-07:00|Apr 7 2005|2005-04-07");
}

/// A date field of nothing but whitespace is the other `person_only` route, and
/// the vertical tab is in it on purpose: `isspace()` in C matches `\013` and
/// Rust's `u8::is_ascii_whitespace` does not, so a port that reaches for the
/// Rust predicate reads the tab as the start of a timestamp and gets a date
/// where git gets none.
///
/// The *committer* line is the broken one here because that is the header
/// `parse_commit_date()` reads (commit.c:141-142), and its every failure arm is
/// `return 0` — which is why `--until=1980-01-01` still lists the commit
/// instead of aborting the walk. The author line is left intact so the empty
/// `%ct` below cannot be a blanket failure.
#[test]
fn a_whitespace_only_committer_date_is_no_date_and_still_walks() {
    let (dir, ids) = corrupt_repo_role(
        "ws",
        "committer",
        &[("spaces", "   "), ("vertical_tab", "  \u{b}")],
    );
    for id in &ids {
        assert_eq!(fmt_of(&dir, "UTC", id, "%at:%ct", "iso"), "1112911993:");
        assert_eq!(fmt_of(&dir, "UTC", id, "%cd", "iso"), "");
        assert_eq!(fmt_of(&dir, "UTC", id, "%cr", "iso"), "");
        // The author half is untouched, so the empties above are the committer's.
        assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso"), "2005-04-07 15:13:13 -0700");
        let o = run_in(&dir, "UTC", &["rev-list", "--until=1980-01-01", id]);
        assert_eq!(code(&o), 0, "rev-list: {}", err(&o));
        assert_eq!(text(&o).trim_end(), *id, "a sentinel date is ancient, not an error");
    }
}

/// In range for `time_t`, out of range for `gmtime_r()`. date.c:330-333 is the
/// only thing standing between this and a year-31688740476 date, and it is a
/// fallback a hand-rolled calendar does not have — which is exactly how the
/// wrong answer looks plausible.
#[test]
fn a_date_gmtime_refuses_falls_back_to_the_epoch() {
    let (dir, ids) = corrupt_repo("farfuture", &[("absurd", "999999999999999999 -0700")]);
    let id = &ids[0];
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "default"), "Thu Jan 1 00:00:00 1970 +0000");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso"), "1970-01-01 00:00:00 +0000");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "short"), "1970-01-01");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "rfc"), "Thu, 1 Jan 1970 00:00:00 +0000");
    // This one does *not* overflow, so `raw`/`unix` — which return before the
    // calendar conversion (date.c:296-318) — still print the recorded value, and
    // the recorded zone with it. That is the line between this case and the
    // `date_overflows()` one above.
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "raw"), "999999999999999999 -0700");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "unix"), "999999999999999999");
}

/// git carries the zone as the `[-+]HHMM` integer it read, and never as an
/// offset, so a zone outside any real one prints back as recorded: `%+05d` of
/// `9999`, and `tz / 100` / `tz % 100` in iso-strict. Normalising through
/// seconds turns `+9999` into `+10039` — arithmetically the same 6039 minutes,
/// and not what git writes.
#[test]
fn an_out_of_range_timezone_prints_back_exactly_as_recorded() {
    let (dir, ids) = corrupt_repo("badtz", &[("plus9999", "1700000000 +9999")]);
    let id = &ids[0];
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "raw"), "1700000000 +9999");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso"), "2023-11-19 02:52:20 +9999");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "default"), "Sun Nov 19 02:52:20 2023 +9999");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso-strict"), "2023-11-19T02:52:20+99:99");
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "rfc"), "Sun, 19 Nov 2023 02:52:20 +9999");
    // `format:%z` is the same integer through `strbuf_addftime()`.
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "format:%z"), "+9999");
    // `-local` ignores the recorded zone entirely, so it is unaffected — which
    // confirms the value above came from the header and not from a calculation.
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso-local"), "2023-11-14 22:13:20 +0000");
}

/// A zone is only read when the timestamp did not overflow, and the two halves
/// of that `if` are easy to write as independent reads. `+0530` on a good
/// timestamp and `+0530` on an overflowing one must therefore differ.
#[test]
fn an_overflowing_timestamp_discards_the_zone_a_good_one_keeps() {
    let (dir, ids) = corrupt_repo(
        "zonedrop",
        &[("good", "1700000000 +0530"), ("overflow", "18446744073709551617 +0530")],
    );
    assert_eq!(fmt_of(&dir, "UTC", &ids[0], "%ad", "raw"), "1700000000 +0530");
    assert_eq!(fmt_of(&dir, "UTC", &ids[1], "%ad", "raw"), "0 +0000");
}

/// The recorded zone, not the process zone, is what the non-`local` modes use —
/// so the same object under three `TZ` values is three identical readings, and
/// the `-local` spelling of the same mode is three different ones. Pinning both
/// halves keeps a "just use localtime" regression from passing under one zone.
#[test]
fn the_recorded_zone_wins_until_local_is_asked_for() {
    let (dir, ids) = corrupt_repo("tzpin", &[("kolkata", "1700000000 +0530")]);
    let id = &ids[0];
    for tz in ["UTC", "America/New_York", "Asia/Kolkata", "Australia/Lord_Howe"] {
        assert_eq!(
            fmt_of(&dir, tz, id, "%ad", "iso"),
            "2023-11-15 03:43:20 +0530",
            "the recorded zone must not follow TZ={tz}"
        );
    }
    assert_eq!(fmt_of(&dir, "UTC", id, "%ad", "iso-local"), "2023-11-14 22:13:20 +0000");
    assert_eq!(fmt_of(&dir, "America/New_York", id, "%ad", "iso-local"), "2023-11-14 17:13:20 -0500");
    assert_eq!(fmt_of(&dir, "Asia/Kolkata", id, "%ad", "iso-local"), "2023-11-15 03:43:20 +0530");
    // `%s` is computed by git rather than `strftime`, and has to undo the zone
    // shift in both spellings.
    assert_eq!(fmt_of(&dir, "Asia/Kolkata", id, "%ad", "format:%s"), "1700000000");
    assert_eq!(fmt_of(&dir, "Asia/Kolkata", id, "%ad", "format-local:%s"), "1700000000");
}
