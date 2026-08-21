//! The value-taking short options `log`, `show` and `diff` reach through
//! `setup_revisions()`, given no value.
//!
//! These options are not in any of the three verbs' own `parse_options()` tables.
//! They arrive in stage 2, where each verb walks argv itself, and every one of
//! those loops read its value as "the next argument, or the empty string":
//!
//! ```text
//! $ git log -S ; echo $?
//! error: switch `S' requires a value
//! 129
//! $ git log -S            # zvcs, before
//! commit 2199fdee3398f81cec175c9a1c25a2d85979b55c
//! ...
//! 0
//! ```
//!
//! An absent pattern became an empty one, which matches every commit, so the run
//! that git refuses succeeded and printed a whole log. `-O` and `-l` failed the
//! other way, with this port's own `unsupported flag` at exit 1.
//!
//! Three different parsers own these seven names and they do not agree on the
//! wording or the status, which is the whole reason the split is pinned here:
//!
//! | option | refusal | exit | owner |
//! |--------|---------|------|-------|
//! | `-S` `-G` `-I` `-O` `-l` | ``error: switch `<c>' requires a value`` | 129 | `get_arg()`, parse-options.c:59-60 |
//! | `-n` | `error: -n requires an argument` | 128 | `handle_revision_opt()`, revision.c |
//! | `-L` | (not a stage-2 option — see below) | | `builtin_log_options`, builtin/log.c |
//!
//! `-L` is the control. It is `log`'s **own** option, read in stage 1, so it is
//! not subject to the cut that follows and its behaviour must not change.
//!
//! ### `--` is not a value
//!
//! `setup_revisions()` removes the separator and everything behind it before it
//! parses a single option:
//!
//! ```c
//! /* First, search for "--" */
//! ...
//!         for (i = 1; i < argc; i++) {
//!                 const char *arg = argv[i];
//!                 if (strcmp(arg, "--"))
//!                         continue;
//!                 ...
//!                 argv[i] = NULL;
//!                 argc = i;
//! ```
//!
//! (`revision.c`.) So `git log -S --` has no value slot left and is the same
//! refusal as a bare `-S` at the end of the line — while `git log -L --` really
//! does hand `--` to the range parser, because stage 1 ran before the cut. Any
//! *other* token is taken as the value, option-looking or not: `git diff -S -p`
//! searches for the string `-p`.
//!
//! Every expectation below is the observed stderr and exit status of stock git
//! 2.55.0, captured by running it in a one-commit repository with no ambient
//! config. Nothing here needs a network, a stock git binary or the developer's
//! configuration: the built binary runs against a private temp repository with
//! `HOME` and `ZVCS_HOME` pinned inside it.
#![cfg(unix)]

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

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// A two-commit repository: `show` and `log` need something to print, and the
    /// second commit gives the pickaxe options a change to search.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-stage2-value-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a.txt"), "base\nneedle\n").unwrap();
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "second"]);
        f
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        Run {
            code: out.status.code().expect("exited via a signal"),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str]) {
        let r = self.run(args);
        assert_eq!(r.code, 0, "setup `git {args:?}` failed: {}", r.stderr);
    }
}

/// The `PARSE_OPT_ERROR` shape: the one `error:` line, nothing else on stderr,
/// nothing on stdout, and the given status.
///
/// stderr is compared **in full**. That is the assertion that separates a missing
/// value from `PARSE_OPT_UNKNOWN`, which appends a usage block; a `starts_with`
/// would accept either and the two would stop being distinguishable. The empty
/// stdout is the other half of the regression — `git log -S` used to print a
/// whole log before exiting 0.
fn assert_refusal(f: &Fixture, args: &[&str], code: i32, line: &str) {
    let r = f.run(args);
    assert_eq!(r.code, code, "`git {args:?}` exit code (stderr: {})", r.stderr);
    assert_eq!(r.stderr, format!("{line}\n"), "`git {args:?}` stderr");
    assert_eq!(r.stdout, "", "`git {args:?}` must print nothing");
}

/// The seven names, with the refusal and status stock git answers each with when
/// the value slot is empty. `-L` is absent: it is stage 1's and has its own test.
const MISSING_VALUE: &[(&str, i32, &str)] = &[
    ("-S", 129, "error: switch `S' requires a value"),
    ("-G", 129, "error: switch `G' requires a value"),
    ("-I", 129, "error: switch `I' requires a value"),
    ("-O", 129, "error: switch `O' requires a value"),
    ("-l", 129, "error: switch `l' requires a value"),
    ("-n", 128, "error: -n requires an argument"),
];

// ---------------------------------------------------------------------------
// the option standing last on the line
// ---------------------------------------------------------------------------

#[test]
fn a_trailing_short_option_with_no_value_is_refused() {
    let f = Fixture::new("trailing");
    for verb in ["log", "show", "diff"] {
        for (opt, code, line) in MISSING_VALUE {
            assert_refusal(&f, &[verb, opt], *code, line);
        }
    }
}

/// The same options with the separator in the value slot. `setup_revisions()` has
/// already cut argv there, so there is nothing to consume and the refusal is
/// identical — `git log -S --` is not a search for the two-character string `--`.
#[test]
fn a_dashdash_in_the_value_slot_is_not_a_value() {
    let f = Fixture::new("dashdash");
    for verb in ["log", "show", "diff"] {
        for (opt, code, line) in MISSING_VALUE {
            assert_refusal(&f, &[verb, opt, "--"], *code, line);
            // With pathspecs behind the separator too: the cut happens at the
            // `--` regardless of what follows it.
            assert_refusal(&f, &[verb, opt, "--", "a.txt"], *code, line);
        }
    }
}

/// `-L` is `builtin_log_options`' own entry, matched by `parse_options()` in
/// stage 1 — before `setup_revisions()` removes the separator — so it *does* take
/// a following `--` and then fails on the range, at 128 rather than 129.
///
/// This is the control for the rule above: a fix that refused every short option
/// standing in front of a `--` would break it.
#[test]
fn dash_l_is_a_stage_one_option_and_still_consumes_the_separator() {
    let f = Fixture::new("dash-l");
    for verb in ["log", "show"] {
        assert_refusal(&f, &[verb, "-L"], 129, "error: switch `L' requires a value");
        let r = f.run(&[verb, "-L", "--"]);
        assert_eq!(r.code, 128, "`git {verb} -L --` exit code (stderr: {})", r.stderr);
        assert_eq!(
            r.stderr.lines().next(),
            Some("fatal: -L argument not 'start,end:file' or ':funcname:file': --"),
            "`git {verb} -L --` must reach the range parser, not the value check"
        );
    }
    // `git diff` has no `-L` at all, so it stays an unknown option with the usage
    // block behind it.
    let r = f.run(&["diff", "-L"]);
    assert_eq!(r.code, 129);
    assert_eq!(r.stderr.lines().next(), Some("error: invalid option: -L"));
}

// ---------------------------------------------------------------------------
// the value forms that must keep working
// ---------------------------------------------------------------------------

/// A refusal that fires when a value *is* present is worse than the bug it
/// replaced, so both spellings of every value are exercised: glued on and in the
/// next argv slot. All twelve exit 0 in stock git 2.55.0.
#[test]
fn a_value_that_is_present_is_still_read() {
    let f = Fixture::new("present");
    for verb in ["log", "show", "diff"] {
        for args in [
            &["-S", "needle"][..],
            &["-Sneedle"],
            &["-G", "needle"],
            &["-Gneedle"],
            &["-I", "needle"],
            &["-Ineedle"],
            &["-n", "1"],
            &["-n1"],
        ] {
            let mut argv = vec![verb];
            argv.extend_from_slice(args);
            let r = f.run(&argv);
            assert_eq!(r.code, 0, "`git {argv:?}` (stderr: {})", r.stderr);
        }
    }
}

/// An option-looking token in the value slot is still the value: `get_arg()` takes
/// the next argv entry whatever it is, and only `--` is special.
#[test]
fn an_option_looking_token_is_taken_as_the_value() {
    let f = Fixture::new("optlike");
    for verb in ["log", "show", "diff"] {
        for args in [&["-S", "-p"][..], &["-G", "--stat"]] {
            let mut argv = vec![verb];
            argv.extend_from_slice(args);
            let r = f.run(&argv);
            assert_eq!(r.code, 0, "`git {argv:?}` (stderr: {})", r.stderr);
            assert_eq!(r.stdout, "", "no commit adds the literal string {:?}", args[1]);
        }
    }
}

/// `git diff`'s `-l` and `-n` take a separate value, and each has its own value
/// parser with its own diagnostic. `-l` is `OPT_INTEGER('l', ...)` — parse-options'
/// integer wording at 129 — and `-n` is `parse_count()` — a `die()` at 128. Both
/// spellings must reach the same one.
#[test]
fn diff_reads_a_separated_l_and_n_value_and_rejects_a_bad_one() {
    let f = Fixture::new("diff-ln");
    for args in [&["-l", "5"][..], &["-l5"], &["-n", "1"], &["-n1"]] {
        let mut argv = vec!["diff"];
        argv.extend_from_slice(args);
        let r = f.run(&argv);
        assert_eq!(r.code, 0, "`git {argv:?}` (stderr: {})", r.stderr);
    }
    for args in [&["-l", "foo"][..], &["-lfoo"]] {
        let mut argv = vec!["diff"];
        argv.extend_from_slice(args);
        assert_refusal(
            &f,
            &argv,
            129,
            "error: switch `l' expects an integer value with an optional k/m/g suffix",
        );
    }
    for args in [&["-n", "foo"][..], &["-nfoo"]] {
        let mut argv = vec!["diff"];
        argv.extend_from_slice(args);
        assert_refusal(&f, &argv, 128, "fatal: 'foo': not an integer");
    }
}

/// The mirror image: *behind* the separator the same names are pathspecs, not
/// options, because `setup_revisions()` has already moved them into `prune_data`.
/// `git diff -- -S` limits the diff to a path called `-S` and exits 0.
///
/// `git diff` claimed some of them anyway — `-S`, `-G`, `--diff-algorithm`,
/// `--find-object`, `--anchored`, `--color-moved-ws`, `--word-diff-regex` — and
/// answered a pathspec with `error: switch `S' requires a value` at 129, because
/// the check that consumes a separate value ran before the pathspec test.
#[test]
fn behind_the_separator_the_same_names_are_pathspecs() {
    let f = Fixture::new("behind");
    for name in [
        "-S",
        "-G",
        "-I",
        "-O",
        "-l",
        "-n",
        "--diff-algorithm",
        "--find-object",
        "--anchored",
        "--color-moved-ws",
        "--word-diff-regex",
        "--output-indicator-new",
    ] {
        for verb in ["log", "show", "diff"] {
            let r = f.run(&[verb, "--", name]);
            assert_eq!(
                r.code, 0,
                "`git {verb} -- {name}` must be a pathspec, not an option (stderr: {})",
                r.stderr
            );
            assert_eq!(r.stderr, "", "`git {verb} -- {name}`");
        }
    }
}

/// `git diff`'s long value-taking options in front of a `--`, which the same cut
/// reaches. Before this, `--output --` created a file called `--` and
/// `--find-object --` reported `unable to resolve '--'`.
#[test]
fn diff_long_options_also_lose_their_value_to_the_separator() {
    let f = Fixture::new("diff-long");
    for name in [
        "find-object",
        "diff-algorithm",
        "output",
        "anchored",
        "skip-to",
        "rotate-to",
        "ws-error-highlight",
        "color-moved-ws",
        "inter-hunk-context",
        "stat-width",
        "word-diff-regex",
    ] {
        let flag = format!("--{name}");
        assert_refusal(
            &f,
            &["diff", &flag, "--"],
            129,
            &format!("error: option `{name}' requires a value"),
        );
    }
    assert!(
        !f.work.join("--").exists(),
        "`git diff --output --` must not have opened a file named `--`"
    );
}
