//! `grep.patternType` decides which dialect `--grep`/`--author`/`--committer`
//! compile in.
//!
//! ```c
//! if (!strcmp(var, "grep.patterntype")) {
//!         opt->pattern_type_option = parse_pattern_type_arg(var, value);
//!         return 0;
//! }
//! ```
//! (`grep_config()`, grep.c:73-76, v2.55.0)
//!
//! It seeds the *same* `pattern_type_option` field the command-line dialect flags
//! assign (`--basic-regexp`, `-E`, `-F`, `-P`, revision.c:2596-2611), so the
//! config value is simply what that field starts as and any flag overwrites it —
//! whichever order they are written in, because there is only one field.
//!
//! An unset key, and the explicit `default`, fall back to the older
//! `grep.extendedRegexp` boolean, which `compile_regexp()` resolves late:
//!
//! ```c
//! if (opt->pattern_type_option == GREP_PATTERN_TYPE_UNSPECIFIED)
//!         opt->pattern_type_option = (opt->extended_regexp_option
//!                                     ? GREP_PATTERN_TYPE_ERE
//!                                     : GREP_PATTERN_TYPE_BRE);
//! ```
//! (grep.c:497-500)
//!
//! The port read the key in `git grep` and nowhere else, so every history walk
//! compiled its patterns as POSIX basic whatever the repository said.
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

impl Fixture {
    /// Two commits: `1`, then one whose subject is the literal `(1|2)`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-patterntype-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "A\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "1"]);
        std::fs::write(f.work.join("file"), "B\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "(1|2)"]);
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
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `fixed` makes the pattern a literal, so a regexp metacharacter stops matching;
/// `basic` still reads `.` as "any character".
#[test]
fn fixed_and_basic_come_from_the_config_key() {
    let f = Fixture::new("fixedbasic");
    // `rev-list --format=` prefixes each record with a `commit <oid>` header of
    // its own, which `--no-commit-header` suppresses; `log` never writes one.
    for verb in [vec!["log", "--format=%s"], vec!["rev-list", "--no-commit-header", "--format=%s"]] {
        let run = |extra: &[&str]| {
            let mut args: Vec<&str> = vec!["-c", "grep.patterntype=fixed"];
            args.extend(verb.iter().copied());
            args.push("--all");
            args.extend(extra);
            f.stdout(&args)
        };
        assert_eq!(run(&["-1", "--grep=s.c.nd"]), "", "{verb:?}");
        // The literal *does* match when it is really there.
        assert_eq!(run(&["--grep=(1|2)"]), "(1|2)\n", "{verb:?}");

        // POSIX basic reads `(`, `|` and `)` literally, so `(.|.)` matches it.
        let mut args: Vec<&str> = vec!["-c", "grep.patterntype=basic"];
        args.extend(verb.iter().copied());
        args.extend(["--all", "--grep=(.|.)"]);
        assert_eq!(f.stdout(&args), "(1|2)\n", "{verb:?}");
    }
}

/// POSIX extended needs `|` escaped to match it literally, which is how the
/// dialect is told apart from basic.
#[test]
fn extended_differs_from_basic_on_the_same_pattern() {
    let f = Fixture::new("extended");
    assert_eq!(
        f.stdout(&["-c", "grep.patterntype=extended", "log", "--format=%s", "--grep=\\|2"]),
        "(1|2)\n"
    );
    // Under basic the same pattern is `\|` the alternation operator, so it also
    // matches the commit whose subject is just `1`.
    assert_eq!(
        f.stdout(&["-c", "grep.patterntype=basic", "log", "--format=%s", "--grep=\\|2"]).lines().count(),
        2
    );
}

/// One field, so the last writer wins and a flag always beats the key — whichever
/// side of the pattern it is written on.
#[test]
fn a_dialect_flag_overrides_the_config() {
    let f = Fixture::new("override");
    for args in [
        vec!["-c", "grep.patterntype=fixed", "log", "-1", "--format=%s", "--basic-regexp", "--grep=s.c.nd"],
        vec!["-c", "grep.patterntype=fixed", "log", "-1", "--format=%s", "--grep=s.c.nd", "--basic-regexp"],
    ] {
        assert_eq!(f.stdout(&args), "", "{args:?}");
    }
    // `--grep=(.|.)` is a literal under `-F` even with `basic` configured.
    assert_eq!(
        f.stdout(&["-c", "grep.patterntype=basic", "log", "--format=%s", "-F", "--grep=(.|.)"]),
        ""
    );
}

/// `default` is not a dialect: it falls back to the legacy `grep.extendedRegexp`
/// boolean, which `compile_regexp()` turns into ERE or BRE.
#[test]
fn default_falls_back_to_grep_extended_regexp() {
    let f = Fixture::new("legacy");
    // BRE: `\|` is alternation, so both commits match.
    assert_eq!(
        f.stdout(&["-c", "grep.patterntype=default", "log", "--format=%s", "--grep=\\|2"]).lines().count(),
        2
    );
    // ERE through the legacy boolean: `\|` is a literal pipe, so only one matches.
    assert_eq!(
        f.stdout(&["-c", "grep.extendedRegexp=true", "log", "--format=%s", "--grep=\\|2"]),
        "(1|2)\n"
    );
    // An explicit type beats the legacy boolean.
    assert_eq!(
        f.stdout(&[
            "-c",
            "grep.extendedRegexp=true",
            "-c",
            "grep.patterntype=basic",
            "log",
            "--format=%s",
            "--grep=\\|2"
        ])
        .lines()
        .count(),
        2
    );
}

/// `--author` and `--committer` compile in the same dialect, and so does
/// `whatchanged`, which reaches `setup_revisions()` the same way.
#[test]
fn the_other_header_greps_and_verbs_read_it_too() {
    let f = Fixture::new("others");
    assert_eq!(f.stdout(&["-c", "grep.patterntype=fixed", "log", "--format=%s", "--author=A.U.Thor"]), "");
    assert_eq!(
        f.stdout(&["-c", "grep.patterntype=basic", "log", "--format=%s", "--author=A.U.Thor"]).lines().count(),
        2
    );
    assert_eq!(f.stdout(&["-c", "grep.patterntype=fixed", "log", "--format=%s", "--committer=C.O.Mitter"]), "");
    assert_eq!(
        f.stdout(&[
            "-c",
            "grep.patterntype=fixed",
            "whatchanged",
            "--i-still-use-this",
            "--format=%s",
            "--grep=s.c.nd"
        ]),
        ""
    );
}
