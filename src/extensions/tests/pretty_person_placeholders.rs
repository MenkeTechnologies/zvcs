//! `format_person_part()` (pretty.c:788-867) in full, plus the two rules around
//! it: `%e` is the commit's `encoding` header, and anything that is not a
//! placeholder at all is printed as typed.
//!
//! The dates are pinned by fixing the author and committer timestamps, so each
//! assertion is about the format rather than about the clock.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One second past the epoch's 1.7 billionth: `2023-11-14 22:13:20 +0000`.
const WHEN: &str = "1700000000 +0000";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Alias Name")
        .env("GIT_AUTHOR_EMAIL", "alias@example.invalid")
        .env("GIT_COMMITTER_NAME", "Committer Name")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_AUTHOR_DATE", WHEN)
        .env("GIT_COMMITTER_DATE", WHEN)
        .env("TZ", "UTC")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn fmt(dir: &Path, spec: &str) -> String {
    String::from_utf8_lossy(&ok(dir, &["log", "-1", &format!("--format={spec}")]).stdout)
        .trim_end_matches('\n')
        .to_string()
}

/// A repository with one commit and a `.mailmap` that rewrites the author, so
/// the mailmap-resolved placeholders differ from the recorded ones.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-person-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    ok(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(
        dir.join(".mailmap"),
        "Proper Name <proper@example.invalid> Alias Name <alias@example.invalid>\n",
    )
    .expect("write .mailmap");
    std::fs::write(dir.join("f.txt"), "x\n").expect("write f.txt");
    ok(&dir, &["add", "-A"]);
    ok(&dir, &["commit", "-qm", "add two"]);
    dir
}

#[test]
fn the_person_placeholders_cover_name_address_local_part_and_seven_dates() {
    let dir = fixture("parts");

    // Name and address, recorded and mailmap-resolved. `format_person_part()`
    // looks the mailmap up for the capitalized letters whether or not
    // `--use-mailmap` is on.
    assert_eq!(fmt(&dir, "%an|%aN"), "Alias Name|Proper Name");
    assert_eq!(fmt(&dir, "%ae|%aE"), "alias@example.invalid|proper@example.invalid");
    // The local part is everything before the first `@` of whichever address the
    // mailmap left.
    assert_eq!(fmt(&dir, "%al|%aL"), "alias|proper");

    // The seven dates, each pinning its own mode, plus the raw timestamp.
    assert_eq!(fmt(&dir, "%ad"), "Tue Nov 14 22:13:20 2023 +0000");
    assert_eq!(fmt(&dir, "%aD"), "Tue, 14 Nov 2023 22:13:20 +0000");
    assert_eq!(fmt(&dir, "%ai"), "2023-11-14 22:13:20 +0000");
    assert_eq!(fmt(&dir, "%aI"), "2023-11-14T22:13:20Z");
    assert_eq!(fmt(&dir, "%as"), "2023-11-14");
    assert_eq!(fmt(&dir, "%ah"), "Nov 14 2023");
    assert_eq!(fmt(&dir, "%at"), "1700000000");

    // The committer letters are the same function over the other identity.
    assert_eq!(fmt(&dir, "%cn|%cl|%cs"), "Committer Name|committer|2023-11-14");
    // …and the mailmap has nothing to say about this one, so `%cN` is `%cn`.
    assert_eq!(fmt(&dir, "%cN|%cE"), "Committer Name|committer@example.invalid");

    // `--date=` steers `%ad`/`%cd` and nothing else.
    let dated = ok(&dir, &["log", "-1", "--date=short", "--format=%ad|%ai"]);
    assert_eq!(
        String::from_utf8_lossy(&dated.stdout).trim_end(),
        "2023-11-14|2023-11-14 22:13:20 +0000"
    );
}

#[test]
fn an_unknown_placeholder_is_printed_as_typed() {
    let dir = fixture("unknown");

    // `format_commit_item()` returns 0 for anything it does not recognize, and
    // the driver then prints the `%` and rescans from the character after it.
    assert_eq!(fmt(&dir, "%zz|%(nope)|%"), "%zz|%(nope)|%");
    assert_eq!(fmt(&dir, "%q"), "%q");
    // `%%` is still an escaped percent, handled before any of that.
    assert_eq!(fmt(&dir, "%%zz"), "%zz");
    // And a real placeholder next to one keeps working.
    assert_eq!(fmt(&dir, "%s|%q|%s"), "add two|%q|add two");

    // `%e` is the `encoding` header, which a commit git wrote itself does not
    // carry — it re-encodes to UTF-8 and drops it.
    assert_eq!(fmt(&dir, "[%e]"), "[]");
}

/// `%C(<spec>)` goes through the same `color_parse_mem()` the `color.*` config
/// slots do, so a spec is spelled identically wherever it appears — and one it
/// refuses is fatal rather than rendered as something plausible.
#[test]
fn a_color_spec_is_parsed_the_way_config_colors_are() {
    let dir = fixture("color");

    let colored = |spec: &str| -> String {
        let out = ok(&dir, &["log", "-1", "--color=always", &format!("--format=%C({spec})x")]);
        String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string()
    };

    // The forms the port used to drop on the floor: a bright name, 24-bit hex,
    // a palette index, and `normal` (which names no code at all).
    assert_eq!(colored("brightred"), "\u{1b}[91mx");
    assert_eq!(colored("#ff0000"), "\u{1b}[38;2;255;0;0mx");
    assert_eq!(colored("255"), "\u{1b}[38;5;255mx");
    assert_eq!(colored("normal"), "x");
    // …beside the ones it already handled.
    assert_eq!(colored("bold red"), "\u{1b}[1;31mx");
    assert_eq!(colored("red blue"), "\u{1b}[31;44mx");
    assert_eq!(colored("reset"), "\u{1b}[mx");

    // A spec the parser refuses names the value and then gives up on the format.
    let bad = run(&dir, &["log", "-1", "--color=always", "--format=%C(nosuchcolor)%h"]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        stderr_of(&bad),
        "error: invalid color value: nosuchcolor\nfatal: unable to parse --pretty format\n"
    );

    // With color off the spec is never parsed at all, so the same format is fine.
    let uncolored = ok(&dir, &["log", "-1", "--no-color", "--format=%C(nosuchcolor)%s"]);
    assert_eq!(String::from_utf8_lossy(&uncolored.stdout).trim_end(), "add two");
}

/// `compile_regexp_failed()` (grep.c) words a bad pattern by where it came from:
/// `--grep` is `command line`, `--author`/`--committer` are `header`, and the
/// pickaxe is neither — it compiles its own regex and dies without an origin.
#[test]
fn a_bad_pattern_is_reported_by_where_it_came_from() {
    let dir = fixture("regex");

    let fails = |args: &[&str]| -> String {
        let out = run(&dir, args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        stderr_of(&out)
    };

    assert_eq!(
        fails(&["log", "--grep=[bad"]),
        "fatal: command line, '[bad': brackets ([ ]) not balanced\n"
    );
    assert_eq!(
        fails(&["log", "--author=[bad"]),
        "fatal: header, '[bad': brackets ([ ]) not balanced\n"
    );
    assert_eq!(
        fails(&["log", "-G[bad"]),
        "fatal: invalid regex: brackets ([ ]) not balanced\n"
    );
    // An unbalanced `(` is an error only where it is an operator: in an extended
    // regular expression, not in the default basic one.
    assert_eq!(
        fails(&["log", "-E", "--grep=(unclosed"]),
        "fatal: command line, '(unclosed': parentheses not balanced\n"
    );
    assert!(ok(&dir, &["log", "--grep=(unclosed"]).stdout.is_empty(), "a BRE `(` is a literal");
}
