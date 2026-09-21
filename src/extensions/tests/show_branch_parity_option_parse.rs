//! `show-branch`'s option parsing: the two refusal *shapes* parse-options has,
//! and the `strtoul()` grammar behind `--reflog=<n>[,<base>]`.
//!
//! Both halves are easy to get wrong in a way that looks right.
//!
//! **The shape.** `parse_options()` ends a rejection in one of two ways
//! (parse-options.c:1198-1201):
//!
//! ```c
//! switch (parse_options_step(&ctx, options, usagestr)) {
//! case PARSE_OPT_HELP:
//! case PARSE_OPT_ERROR:
//!         exit(129);
//! ```
//!
//! `PARSE_OPT_ERROR` is what `error()` returns through `get_value()` and through
//! an option callback, and it exits **bare** — one `error:` line and nothing
//! else. Only `PARSE_OPT_UNKNOWN`, three lines further down, reaches
//! `usage_with_options()`. So `--reflog=bogus` writes 41 bytes to stderr under
//! stock 2.55.0 while `-Z` writes 1472, and a port that prints the usage block
//! for both has the right text and the wrong shape. Every case below is paired
//! with that `-Z` control, because a fix that dropped the block everywhere would
//! be just as wrong.
//!
//! **The name.** `optname()` (parse-options.c:30-45) reports the *table's*
//! spelling, with a `no-` glued on for the unset sense:
//!
//! ```c
//! else if (flags & OPT_UNSET)
//!         strbuf_addf(&sb, "option `no-%s'", opt->long_name);
//! else if (flags == OPT_LONG)
//!         strbuf_addf(&sb, "option `%s'", opt->long_name);
//! ```
//!
//! which is why `--al=x` is reported as `all` (the abbreviation is resolved
//! first) and `--name=x` as `no-no-name` (the table entry is `no-name`, reached
//! in the unset sense). Echoing the token as typed passes none of those.
//!
//! **The grammar.** `parse_reflog_param()` (builtin/show-branch.c:622-640) reads
//! `<n>` with `strtoul(arg, &ep, 10)` and then
//!
//! ```c
//! if (reflog <= 0)
//!         reflog = DEFAULT_REFLOG;
//! ```
//!
//! so a leading sign, leading whitespace and `int` wraparound are all legal
//! spellings rather than refusals. A digit-run scanner rejects `-1` and `+2`,
//! reads `' 3'` as nothing, and turns `4294967298` into the default instead of
//! 2. The `<base>` half (builtin/show-branch.c:791-800) is the same `strtoul`,
//! so `2,+1` is the index 1 and `2,-1` is the index -1 that `read_ref_at()`
//! refuses outright.
//!
//! Every expectation here was captured from stock git 2.55.0 in an identical
//! fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 +0000")
        .env("GIT_COMMITTER_DATE", "1112911993 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) -> String {
    let (stdout, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
    stdout.trim_end().to_string()
}

/// Five commits on `main`, so its reflog has five entries and a `<n>` of 1
/// through 5 each picks out a different run.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-sb-opt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    for name in ["A", "B", "C", "D", "E"] {
        std::fs::write(repo.join(name), format!("{name}\n")).unwrap();
        must(&repo, &home, &["add", name]);
        must(&repo, &home, &["commit", "-qm", name]);
    }
    (root, repo, home)
}

/// The usage block belongs to `PARSE_OPT_UNKNOWN` alone. Each `error()` that
/// reaches `PARSE_OPT_ERROR` — an option callback (`--reflog`, `--color`) and
/// `get_value()`'s integer parse (`--more`) — writes its one line and stops.
#[test]
fn parse_opt_error_refusals_print_no_usage_block() {
    let (root, repo, home) = fixture("bare");

    for (args, line) in [
        (
            vec!["show-branch", "--reflog=bogus", "main"],
            "error: unrecognized reflog param 'bogus'\n",
        ),
        (
            vec!["show-branch", "-gx", "main"],
            "error: unrecognized reflog param 'x'\n",
        ),
        (
            vec!["show-branch", "--reflog=3 ", "main"],
            "error: unrecognized reflog param '3 '\n",
        ),
        (
            vec!["show-branch", "--reflog=0x10", "main"],
            "error: unrecognized reflog param '0x10'\n",
        ),
        (
            // `-` consumes the sign but converts no digit, so `strtoul` leaves
            // `ep` at the start of the string and the `,` test fails.
            vec!["show-branch", "--reflog=-", "main"],
            "error: unrecognized reflog param '-'\n",
        ),
        (
            vec!["show-branch", "--color=bogus", "main"],
            "error: option `color' expects \"always\", \"auto\", or \"never\"\n",
        ),
        (
            vec!["show-branch", "--more=bogus", "main"],
            "error: option `more' expects an integer value with an optional k/m/g suffix\n",
        ),
        (
            vec!["show-branch", "--more=", "main"],
            "error: option `more' expects a numerical value\n",
        ),
    ] {
        let (out, err, code) = git(&repo, &home, &args);
        assert_eq!(err, line, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 129, "{args:?}");
    }

    // The control: `PARSE_OPT_UNKNOWN` *does* print the block, so a port that
    // simply stopped printing it everywhere fails here.
    let (_, err, code) = git(&repo, &home, &["show-branch", "-Z", "main"]);
    assert!(err.starts_with("error: unknown switch `Z'\n"), "{err}");
    assert!(err.contains("usage: git show-branch"), "{err}");
    assert_eq!(code, 129);

    let (_, err, code) = git(&repo, &home, &["show-branch", "--nosuchopt", "main"]);
    assert!(err.starts_with("error: unknown option `nosuchopt'\n"), "{err}");
    assert!(err.contains("usage: git show-branch"), "{err}");
    assert_eq!(code, 129);

    let _ = std::fs::remove_dir_all(&root);
}

/// `do_get_value()`'s two `takes no value` refusals (parse-options.c:138-143),
/// each naming the option the way `optname()` does rather than the way it was
/// typed. Without them these tokens fell through to `unknown option`, which
/// quotes the value as part of the name *and* prints the usage block.
#[test]
fn a_glued_value_on_a_no_arg_option_is_named_by_the_table() {
    let (root, repo, home) = fixture("noarg");

    for (tok, shown) in [
        ("--all=x", "all"),
        ("--remotes=1", "remotes"),
        ("--list=3", "list"),
        ("--current=1", "current"),
        ("--sparse=2", "sparse"),
        ("--topo-order=x", "topo-order"),
        // An abbreviation is resolved before `optname()` sees the entry.
        ("--al=x", "all"),
        // The unset sense glues `no-` onto the *table's* name, so an entry that
        // already spells its own `no-` is reported doubled.
        ("--name=x", "no-no-name"),
        ("--no-all=x", "no-all"),
        ("--no-name=x", "no-name"),
        ("--no-sparse=1", "no-sparse"),
        // `--color` and `--more` take an optional value in the *set* sense, so
        // only their unset spelling is refused here.
        ("--no-color=x", "no-color"),
        ("--no-more=2", "no-more"),
        // `p->opt` is "there was an `=`", not "there was a value": an empty one
        // is refused just the same.
        ("--all=", "all"),
    ] {
        let (out, err, code) = git(&repo, &home, &["show-branch", tok, "main"]);
        assert_eq!(err, format!("error: option `{shown}' takes no value\n"), "{tok}");
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 129, "{tok}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `<n>` is `strtoul`, not a digit run: a sign is consumed, leading whitespace
/// is skipped, and the result wraps into the `int` it is assigned to. Each case
/// is pinned against the plain spelling it has to agree with, so the test states
/// an equivalence rather than a transcript.
#[test]
fn reflog_count_is_read_with_strtoul() {
    let (root, repo, home) = fixture("count");

    let four = must(&repo, &home, &["show-branch", "--no-color", "--reflog=4", "main"]);
    let two = must(&repo, &home, &["show-branch", "--no-color", "--reflog=2", "main"]);
    let three = must(&repo, &home, &["show-branch", "--no-color", "--reflog=3", "main"]);
    // The premise: the three runs really are distinguishable.
    assert_ne!(four, two);
    assert_ne!(three, two);

    for spec in ["--reflog=-1", "--reflog=-3", "--reflog=0", "-g0", "-g-2"] {
        // `reflog <= 0` falls back to DEFAULT_REFLOG, which is 4.
        let got = must(&repo, &home, &["show-branch", "--no-color", spec, "main"]);
        assert_eq!(got, four, "{spec}");
    }
    for spec in ["--reflog=+2", "--reflog=02", "-g+2"] {
        let got = must(&repo, &home, &["show-branch", "--no-color", spec, "main"]);
        assert_eq!(got, two, "{spec}");
    }
    // `strtoul` skips leading whitespace; the digit run does not, and read the
    // whole argument as the refused remainder.
    let got = must(&repo, &home, &["show-branch", "--no-color", "--reflog= 3", "main"]);
    assert_eq!(got, three);

    // 4294967298 == 2 + 2^32: `unsigned long` holds it, `int reflog` does not.
    let got = must(
        &repo,
        &home,
        &["show-branch", "--no-color", "--reflog=4294967298", "main"],
    );
    assert_eq!(got, two);

    let _ = std::fs::remove_dir_all(&root);
}

/// The `<base>` half of `--reflog=<n>,<base>` is the same `strtoul`, and a
/// negative index is the one that proves it: `read_ref_at()` turns it away on
/// the very first entry, so `reflog` is cut to 0 and nothing is left to show.
#[test]
fn reflog_base_is_read_with_strtoul() {
    let (root, repo, home) = fixture("base");

    let at_one = must(&repo, &home, &["show-branch", "--no-color", "--reflog=2,1", "main"]);
    let at_zero = must(&repo, &home, &["show-branch", "--no-color", "--reflog=2,0", "main"]);
    assert_ne!(at_one, at_zero);
    assert!(at_one.contains("[main@{1}]"), "{at_one}");

    for spec in ["--reflog=2,+1", "--reflog=2, 1", "--reflog=2,01"] {
        let got = must(&repo, &home, &["show-branch", "--no-color", spec, "main"]);
        assert_eq!(got, at_one, "{spec}");
    }

    // A negative `<n>` in front of a `<base>` still falls back to the default
    // rather than being refused, so the `,` arm is reached at all.
    let at_one_default = must(&repo, &home, &["show-branch", "--no-color", "--reflog=4,1", "main"]);
    for spec in ["--reflog=-2,1", "--reflog=0,1"] {
        let got = must(&repo, &home, &["show-branch", "--no-color", spec, "main"]);
        assert_eq!(got, at_one_default, "{spec}");
    }

    let (out, err, code) = git(&repo, &home, &["show-branch", "--no-color", "--reflog=2,-1", "main"]);
    assert_eq!(out, "");
    assert_eq!(err, "No revs to be shown.\n");
    assert_eq!(code, 0);

    let _ = std::fs::remove_dir_all(&root);
}
