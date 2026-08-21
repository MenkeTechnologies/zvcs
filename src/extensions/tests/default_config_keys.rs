//! `git_default_config()` (environment.c:659-716) — the keys git validates while
//! it *reads* the configuration, pinned to what git 2.55.0 was observed to do.
//!
//! Every expectation here is a literal captured from a differential run against
//! stock git in a one-commit fixture, so these run headless with nothing on
//! `PATH` but the binary under test. The command is `status --short` unless a key
//! needs a different one: the point is that these refusals happen *before* the
//! command does anything, so the verb hardly matters.
//!
//! What is covered, and why each group is here rather than folded together:
//!
//!   * the `git_config_bool` keys, which die with one `fatal:` line and no origin
//!     clause in every scope;
//!   * the `return error(...)` keys, which print `error:` first and then a
//!     `fatal:` that *does* name its origin — `-c` and the environment say
//!     "command-line config", a file says `in file '<path>' at line <n>`;
//!   * the numeric keys, whose `die_bad_number` line carries ` in file <path>`
//!     with no line number, a third and deliberately different shape;
//!   * the arms with an extra word or a closed set (`core.autocrlf=input`,
//!     `push.default=tracking`, `branch.autoSetupMerge=always`), where the
//!     case-sensitivity of the C comparison is the whole behaviour;
//!   * the keys that look validatable and are not (`core.eol`, `core.whitespace`,
//!     an `advice.*` slot outside git's table), because accepting them is as much
//!     of a parity requirement as refusing the others.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's `die()` exit status.
const FATAL: i32 = 128;

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// Same, but with `GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0` set — the third scope a
/// value can arrive through, which git reports as command-line config just like
/// `-c`.
fn run_env(repo: &Path, home: &Path, key: &str, value: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", key)
        .env("GIT_CONFIG_VALUE_0", value)
        .output()
        .unwrap()
}

fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A one-commit repository plus an isolated empty `HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-defaultcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("f"), "a\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// Append `block` to the repository config and return the 1-based line number the
/// last line of it landed on — which is what git's
/// `bad config variable … at line <n>` names.
fn append_config(repo: &Path, block: &str) -> usize {
    let cfg = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(block);
    std::fs::write(&cfg, &text).unwrap();
    text.lines().count()
}

// ---------------------------------------------------------------------------
// git_config_bool: one fatal line, no origin clause, in every scope
// ---------------------------------------------------------------------------

/// The plain booleans of `git_default_core_config()` (environment.c:299-546) and
/// the three that live outside it. Every one dies through `git_config_bool`
/// (config.c:1292-1298), whose message has no `kvi` to build an origin from — so
/// the bytes are identical whether the value came from `-c`, a file, or the
/// environment. That invariance is the point of the second half of this test.
#[test]
fn the_boolean_keys_die_with_gits_boolean_message() {
    let (repo, home) = fixture("bools");

    for key in [
        "core.filemode",
        "core.trustctime",
        "core.quotepath",
        "core.symlinks",
        "core.ignorecase",
        "core.bare",
        "core.ignorestat",
        "core.lockfilepid",
        "core.sparseCheckout",
        "core.sparseCheckoutCone",
        "core.precomposeUnicode",
        "core.protectHFS",
        "core.protectNTFS",
        "user.useConfigOnly",
        "color.pager",
        "pager.color",
        "color.advice",
        "advice.detachedHead",
        "advice.statusHints",
        "sparse.expectFilesOutsideOfPatterns",
    ] {
        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "status", "--short"]);
        assert_eq!(
            stderr(&out),
            format!(
                "fatal: bad boolean config value 'bogus' for '{}'\n",
                key.to_lowercase()
            ),
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // The same key through the other two scopes says the same thing, byte for
    // byte — no ` in file …`, no ` at line …`.
    let expected = "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n";
    append_config(&repo, "[core]\n\tignorecase = bogus\n");
    let out = run(&repo, &home, &["status", "--short"]);
    assert_eq!(stderr(&out), expected, "from the config file");
    assert_eq!(code(&out), FATAL);

    let (repo, home) = fixture("bools-env");
    let out = run_env(&repo, &home, "core.ignorecase", "bogus", &["status", "--short"]);
    assert_eq!(stderr(&out), expected, "from GIT_CONFIG_KEY_0");
    assert_eq!(code(&out), FATAL);
}

/// `git_config_bool`'s two edge values, which are not the same thing:
/// `git_parse_maybe_bool_text(NULL)` is 1 (parse.c:168-169) while `""` is 0. So
/// `[core]\n\tbare\n` is a *true* boolean and `bare =` is a false one, and
/// neither is an error.
#[test]
fn a_valueless_boolean_is_true_and_an_empty_one_is_false() {
    let (repo, home) = fixture("bool-edges");
    append_config(&repo, "[core]\n\tignorecase\n\tprecomposeUnicode =\n");
    let out = run(&repo, &home, &["status", "--short"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

// ---------------------------------------------------------------------------
// return error(...): the origin-naming fatal, all three scopes
// ---------------------------------------------------------------------------

/// The arms that `return error(...)` rather than dying: the `error:` text first,
/// then `git_die_config_linenr()` (config.c:2552-2559) naming where the value
/// came from.
///
/// The pairs below are exactly what git 2.55.0 prints, `push.default` included —
/// the one arm in `git_default_config` that emits two `error:` lines
/// (environment.c:637-640).
#[test]
fn the_reporting_keys_print_error_then_a_fatal_naming_the_origin() {
    let (repo, home) = fixture("reported");

    let cases: &[(&str, &str, &[&str])] = &[
        (
            "core.checkStat=bogus",
            "core.checkstat",
            &["error: invalid value for 'core.checkstat': 'bogus'"],
        ),
        (
            "core.disambiguate=bogus",
            "core.disambiguate",
            &["error: unknown hint type for 'core.disambiguate': bogus"],
        ),
        (
            "core.commentChar=",
            "core.commentchar",
            &["error: core.commentchar must have at least one character"],
        ),
        (
            "core.commentString=",
            "core.commentstring",
            &["error: core.commentstring must have at least one character"],
        ),
        (
            "branch.autoSetupRebase=bogus",
            "branch.autosetuprebase",
            &["error: malformed value for branch.autosetuprebase"],
        ),
        (
            "push.default=bogus",
            "push.default",
            &[
                "error: malformed value for push.default: bogus",
                "error: must be one of nothing, matching, simple, upstream or current",
            ],
        ),
        (
            "color.advice.hint=nosuchcolor",
            "color.advice.hint",
            &["error: invalid color value: nosuchcolor"],
        ),
    ];

    for (assignment, key, errors) in cases {
        let mut want = errors.join("\n");
        want.push('\n');
        want.push_str(&format!(
            "fatal: unable to parse '{key}' from command-line config\n"
        ));
        let out = run(&repo, &home, &["-c", assignment, "status", "--short"]);
        assert_eq!(stderr(&out), want, "for -c {assignment}");
        assert_eq!(code(&out), FATAL, "for -c {assignment}");

        // The environment scope is the same "command-line config" to git.
        let (k, v) = assignment.split_once('=').unwrap();
        let out = run_env(&repo, &home, k, v, &["status", "--short"]);
        assert_eq!(stderr(&out), want, "for GIT_CONFIG_KEY_0={k}");
        assert_eq!(code(&out), FATAL, "for GIT_CONFIG_KEY_0={k}");
    }
}

/// The same arms from a *file*, where the fatal names the path and the line.
///
/// The line number is computed from the fixture rather than hardcoded, and the
/// block is padded with unrelated variables first so a port that simply reported
/// the section's first line — or the file's last — would fail here.
#[test]
fn a_file_sourced_refusal_names_the_path_and_the_line() {
    let (repo, home) = fixture("file-origin");
    let line = append_config(
        &repo,
        "[core]\n\tlogAllRefUpdates = true\n\tfilemode = true\n\tcheckStat = bogus\n",
    );

    let out = run(&repo, &home, &["status", "--short"]);
    assert_eq!(
        stderr(&out),
        format!(
            "error: invalid value for 'core.checkstat': 'bogus'\n\
             fatal: bad config variable 'core.checkstat' in file '.git/config' at line {line}\n"
        )
    );
    assert_eq!(code(&out), FATAL);
}

/// `config_error_nonbool()` (config.c:3552-3555) for a `git_config_string` key
/// written with no `=` at all, in both origin shapes.
#[test]
fn a_string_key_without_a_value_is_config_error_nonbool() {
    let (repo, home) = fixture("nonbool");

    for key in [
        "core.editor",
        "core.askpass",
        "core.checkRoundTripEncoding",
        "core.excludesFile",
        "core.attributesFile",
        "i18n.commitEncoding",
        "i18n.logOutputEncoding",
        "attr.tree",
        "user.name",
        "user.email",
        "author.name",
        "committer.email",
        "push.default",
        "branch.autoSetupRebase",
    ] {
        let lower = key.to_lowercase();
        let (section, name) = key.split_once('.').unwrap();
        let (repo, home) = fixture(&format!("nonbool-{}", lower.replace('.', "-")));
        let line = append_config(&repo, &format!("[{section}]\n\t{name}\n"));
        let out = run(&repo, &home, &["status", "--short"]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: missing value for '{lower}'\n\
                 fatal: bad config variable '{lower}' in file '.git/config' at line {line}\n"
            ),
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // An *empty* value is a different thing and is not `config_error_nonbool`:
    // `git_config_string` copies the empty string and returns 0, so the run is
    // clean. This is the pair the C distinguishes with `if (!value)`.
    let out = run(&repo, &home, &["-c", "core.editor=", "status", "--short"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

// NOTE: the valueless spelling on the *command line* — `git -c core.editor` with
// no `=` — is not covered above, because this port cannot see it. git carries a
// `-c` override to its config reader through `GIT_CONFIG_PARAMETERS`, whose
// grammar has a no-`=` form; zvcs carries it through the
// `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` sequence
// (`crate::push_config_override`), which is the channel `gix-config` reads and
// which has no way to spell "no value" — a valueless `-c` arrives as an empty
// string. So `git -c core.editor status` runs clean here where stock reports
// `missing value for 'core.editor'`. The file scope, which is where the
// valueless form is actually written by hand, is covered exactly.

// ---------------------------------------------------------------------------
// die_bad_number: the third shape, with ` in file <path>` and no line
// ---------------------------------------------------------------------------

/// The numeric keys. `die_bad_number()` (config.c:1188-1223) names the file but
/// not the line, and does not quote the path — a third distinct shape that must
/// not be confused with `git_die_config_linenr`'s.
#[test]
fn the_numeric_keys_die_with_the_bad_number_shape() {
    let (repo, home) = fixture("numeric");

    for key in [
        "core.abbrev",
        "core.looseCompression",
        "core.compression",
        "pack.packSizeLimit",
        "pack.compression",
    ] {
        let out = run(&repo, &home, &["-c", &format!("{key}=bogus"), "status", "--short"]);
        assert_eq!(
            stderr(&out),
            format!(
                "fatal: bad numeric config value 'bogus' for '{}': invalid unit\n",
                key.to_lowercase()
            ),
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // From a file the same key grows ` in file .git/config` — unquoted, and with
    // no line number.
    let (repo, home) = fixture("numeric-file");
    append_config(&repo, "[pack]\n\tpackSizeLimit = bogus\n");
    let out = run(&repo, &home, &["status", "--short"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'pack.packsizelimit' in file .git/config: \
         invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);
}

/// The zlib range checks (environment.c:371-374, 382-385, 700-703). `-1` is
/// `Z_DEFAULT_COMPRESSION` and exempt; the message differs between the two
/// `core.*` keys and `pack.compression`.
#[test]
fn the_compression_levels_are_range_checked_after_they_parse() {
    let (repo, home) = fixture("zlib");

    for key in ["core.looseCompression", "core.compression"] {
        for bad in ["99", "-2"] {
            let out = run(&repo, &home, &["-c", &format!("{key}={bad}"), "status", "--short"]);
            assert_eq!(
                stderr(&out),
                format!("fatal: bad zlib compression level {bad}\n"),
                "for {key}={bad}"
            );
            assert_eq!(code(&out), FATAL, "for {key}={bad}");
        }
    }

    let out = run(&repo, &home, &["-c", "pack.compression=99", "status", "--short"]);
    assert_eq!(stderr(&out), "fatal: bad pack compression level 99\n");
    assert_eq!(code(&out), FATAL);

    for good in ["-1", "0", "9"] {
        for key in ["core.compression", "core.looseCompression", "pack.compression"] {
            let out = run(&repo, &home, &["-c", &format!("{key}={good}"), "status", "--short"]);
            assert_eq!(stderr(&out), "", "for {key}={good}");
            assert!(out.status.success(), "for {key}={good}");
        }
    }
}

// ---------------------------------------------------------------------------
// core.abbrev: three branches, and the range check on the third
// ---------------------------------------------------------------------------

/// `core.abbrev` (environment.c:349-363) is `auto`, then the *false half* of the
/// text boolean, then an integer with a floor of `minimum_abbrev`.
///
/// The middle branch is `!git_parse_maybe_bool_text(value)`, and `!1` is 0 — so
/// `true` is **not** a word here and falls through to `git_config_int`, which
/// cannot read it. That asymmetry with `no` is the whole of this test's first
/// half, and it is what a naive "parse as boolean first" port gets wrong.
#[test]
fn core_abbrev_reads_auto_then_a_false_word_then_a_number() {
    let (repo, home) = fixture("abbrev");

    // The false words mean "the whole hash" and are accepted.
    for word in ["no", "NO", "false", "off", ""] {
        let out = run(&repo, &home, &["-c", &format!("core.abbrev={word}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {word:?}");
        assert!(out.status.success(), "for {word:?}");
    }

    // The true words are not words at all here.
    for word in ["true", "yes", "on"] {
        let out = run(&repo, &home, &["-c", &format!("core.abbrev={word}"), "status", "--short"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: bad numeric config value '{word}' for 'core.abbrev': invalid unit\n"),
            "for {word}"
        );
        assert_eq!(code(&out), FATAL, "for {word}");
    }

    // Below `minimum_abbrev` is an `error()`, so it carries an origin clause —
    // unlike the unreadable value just above, which dies inside the number parser.
    for short in ["0", "3", "-1"] {
        let out = run(&repo, &home, &["-c", &format!("core.abbrev={short}"), "status", "--short"]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: abbrev length out of range: {short}\n\
                 fatal: unable to parse 'core.abbrev' from command-line config\n"
            ),
            "for {short}"
        );
        assert_eq!(code(&out), FATAL, "for {short}");
    }

    // `auto` and any length from 4 up are fine, including one past the hash width.
    for good in ["auto", "AUTO", "4", "40", "99"] {
        let out = run(&repo, &home, &["-c", &format!("core.abbrev={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
}

/// The range bug that lives in the same arm: a `core.abbrev` past the hash width
/// is not an error, and it does not fall back to `auto` either — git caps what it
/// prints at `hexsz` and prints the *whole* name.
///
/// Measured against git 2.55.0 in a 40-hex repository: `-c core.abbrev=99 log
/// --oneline` and `-c core.abbrev=41 …` both print 40 characters, while `auto`
/// on the same object prints 7. Before the fix in
/// `gix::config::tree::core::Abbrev::try_into_abbreviation`, an out-of-range
/// length was rejected and the lenient fallback landed on `auto` — seven
/// characters, i.e. *shorter* than asked for rather than longer.
#[test]
fn core_abbrev_past_the_hash_width_prints_the_whole_name() {
    let (repo, home) = fixture("abbrev-range");
    // A second commit so the ids differ and `--oneline` has something to shorten.
    std::fs::write(repo.join("f"), "b\n").unwrap();
    ok(&repo, &home, &["commit", "-qam", "c1"]);

    let full = String::from_utf8_lossy(&ok(&repo, &home, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(full.len(), 40, "fixture is a sha1 repository");

    let oneline = |args: &[&str]| -> String {
        let mut argv = args.to_vec();
        argv.extend_from_slice(&["log", "--oneline", "-1"]);
        let out = ok(&repo, &home, &argv);
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_owned()
    };

    for len in ["41", "99", "40"] {
        assert_eq!(
            oneline(&["-c", &format!("core.abbrev={len}")]),
            full,
            "core.abbrev={len} must print the whole name, not a fallback"
        );
    }
    // A width inside the range is still honoured exactly.
    assert_eq!(oneline(&["-c", "core.abbrev=12"]), full[..12]);
}

// ---------------------------------------------------------------------------
// The arms with an extra word, and the case-sensitivity of each comparison
// ---------------------------------------------------------------------------

/// Three arms take a word *before* falling back to a boolean, and the comparison
/// is not the same in each: `core.autocrlf`/`core.safecrlf` use `strcasecmp`
/// (environment.c:392-411) while `branch.autoSetupMerge` uses `strcmp`
/// (environment.c:582-594). So `INPUT` is a mode and `Always` is a bad boolean.
#[test]
fn the_word_arms_match_case_the_way_their_c_comparison_does() {
    let (repo, home) = fixture("words");

    for good in ["input", "INPUT", "Input"] {
        let out = run(&repo, &home, &["-c", &format!("core.autocrlf={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for core.autocrlf={good}");
        assert!(out.status.success(), "for core.autocrlf={good}");
    }
    for good in ["warn", "WARN"] {
        let out = run(&repo, &home, &["-c", &format!("core.safecrlf={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for core.safecrlf={good}");
        assert!(out.status.success(), "for core.safecrlf={good}");
    }

    for good in ["always", "inherit", "simple"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("branch.autoSetupMerge={good}"), "status", "--short"],
        );
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
    // `strcmp`, so the capitalised spelling is a boolean and fails as one.
    let out = run(&repo, &home, &["-c", "branch.autoSetupMerge=Always", "status", "--short"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'Always' for 'branch.autosetupmerge'\n"
    );
    assert_eq!(code(&out), FATAL);

    // `branch.autoSetupRebase` has no boolean fallback and is also `strcmp`.
    for good in ["never", "local", "remote", "always"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("branch.autoSetupRebase={good}"), "status", "--short"],
        );
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
    let out = run(&repo, &home, &["-c", "branch.autoSetupRebase=Never", "status", "--short"]);
    assert_eq!(
        stderr(&out),
        "error: malformed value for branch.autosetuprebase\n\
         fatal: unable to parse 'branch.autosetuprebase' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);

    // `core.disambiguate` is `strcasecmp` against its six hints (object-name.c:222-227).
    for good in ["none", "COMMIT", "committish", "tree", "treeish", "blob"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("core.disambiguate={good}"), "status", "--short"],
        );
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }

    // `core.checkStat` is `strcasecmp` against two.
    for good in ["default", "DEFAULT", "minimal", "Minimal"] {
        let out = run(&repo, &home, &["-c", &format!("core.checkStat={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }

    // `push.default` is `strcmp`, and `tracking` is the accepted deprecated
    // spelling of `upstream` (environment.c:628-629).
    for good in ["nothing", "matching", "simple", "upstream", "tracking", "current"] {
        let out = run(&repo, &home, &["-c", &format!("push.default={good}"), "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
    let out = run(&repo, &home, &["-c", "push.default=Simple", "status", "--short"]);
    assert_eq!(code(&out), FATAL);
}

// ---------------------------------------------------------------------------
// The keys that are deliberately NOT validated
// ---------------------------------------------------------------------------

/// Accepting a value git accepts is as much of a parity requirement as refusing
/// one it refuses, and each of these looks like it ought to be checked:
///
///   * `core.eol` has a closed set of three words, but the `else` branch is
///     `EOL_UNSET` rather than an error (environment.c:413-423) — so a typo runs.
///     This one used to make zvcs *fail* where git succeeded, because gitoxide
///     rejected the value while resolving the filter pipeline.
///   * `core.whitespace` skips a rule name it does not know (ws.c:54-63).
///   * an `advice.*` key outside `advice_setting[]` (advice.c:47-94) is ignored,
///     and so is a `color.advice.<slot>` outside the two-entry slot table.
///   * `core.commentChar` takes a multi-character string.
#[test]
fn the_permissive_arms_stay_permissive() {
    let (repo, home) = fixture("permissive");

    for assignment in [
        "core.eol=bogus",
        "core.eol=",
        "core.whitespace=bogus",
        "core.whitespace=-nosuchrule",
        "advice.nosuchAdvice=bogus",
        "color.advice.nosuchslot=nosuchcolor",
        "core.commentChar=ab",
        "core.commentChar=auto",
        "core.checkRoundTripEncoding=",
        "attr.tree=",
        "i18n.commitEncoding=",
    ] {
        let out = run(&repo, &home, &["-c", assignment, "status", "--short"]);
        assert_eq!(stderr(&out), "", "for {assignment}");
        assert!(out.status.success(), "for {assignment}");
    }

    // A newline inside `core.commentChar` *is* refused, which is the only shape
    // of that key git rejects besides the empty one.
    let out = run(&repo, &home, &["-c", "core.commentChar=a\nb", "status", "--short"]);
    assert_eq!(
        stderr(&out),
        "error: core.commentchar cannot contain newline\n\
         fatal: unable to parse 'core.commentchar' from command-line config\n"
    );
    assert_eq!(code(&out), FATAL);
}

/// `parse_whitespace_rule()`'s two diagnostics (ws.c:64-79): an out-of-range
/// `tabwidth=` warns and the run continues, and the one contradictory pair is
/// fatal with no origin clause.
#[test]
fn core_whitespace_warns_on_tabwidth_and_dies_on_the_contradiction() {
    let (repo, home) = fixture("whitespace");

    let out = run(&repo, &home, &["-c", "core.whitespace=tabwidth=99", "status", "--short"]);
    assert_eq!(stderr(&out), "warning: tabwidth 99 out of range\n");
    assert!(out.status.success());

    let out = run(&repo, &home, &["-c", "core.whitespace=tabwidth=8", "status", "--short"]);
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());

    let out = run(
        &repo,
        &home,
        &["-c", "core.whitespace=tab-in-indent,indent-with-non-tab", "status", "--short"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: cannot enforce both tab-in-indent and indent-with-non-tab\n"
    );
    assert_eq!(code(&out), FATAL);

    // Negating one of the pair resolves the contradiction, as it does in ws.c.
    let out = run(
        &repo,
        &home,
        &["-c", "core.whitespace=tab-in-indent,-indent-with-non-tab", "status", "--short"],
    );
    assert_eq!(stderr(&out), "");
    assert!(out.status.success());
}

/// `git_config_pathname()` → `interpolate_path()`: a `~user` that `getpwnam(3)`
/// cannot resolve is fatal, and the message quotes the path *after* the
/// `:(optional)` prefix is stripped (config.c:1308-1327).
#[test]
fn a_path_key_dies_when_the_user_directory_cannot_be_expanded() {
    let (repo, home) = fixture("pathname");

    for key in ["core.excludesFile", "core.attributesFile"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("{key}=~zvcs-no-such-user/ignore"), "status", "--short"],
        );
        assert_eq!(
            stderr(&out),
            "fatal: failed to expand user dir in: '~zvcs-no-such-user/ignore'\n",
            "for {key}"
        );
        assert_eq!(code(&out), FATAL, "for {key}");
    }

    // A plain path, a `~/` path and a `%(prefix)/` path all expand.
    for good in ["/tmp/ignore", "~/ignore", "%(prefix)/etc/ignore"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("core.excludesFile={good}"), "status", "--short"],
        );
        assert_eq!(stderr(&out), "", "for {good}");
        assert!(out.status.success(), "for {good}");
    }
}

// ---------------------------------------------------------------------------
// Callback semantics: every occurrence, first one wins the refusal
// ---------------------------------------------------------------------------

/// `configset_iter()` (config.c:1654-1673) calls the callback once per *value*,
/// not once per key, so an occurrence that a later line overrides is still
/// validated — and the first bad one is where git stops.
///
/// This is the behaviour that separates `git_default_config`'s keys from the
/// targeted `repo_config_get_*` readers, which keep the last value and never see
/// the earlier ones; `core_settings_config.rs` pins the other side of that
/// contrast with `core.packedGitLimit`.
#[test]
fn every_occurrence_is_validated_and_the_first_bad_one_stops_the_run() {
    let (repo, home) = fixture("occurrences");

    for pair in [
        ["core.ignorecase=bogus", "core.ignorecase=true"],
        ["core.ignorecase=true", "core.ignorecase=bogus"],
    ] {
        let out = run(&repo, &home, &["-c", pair[0], "-c", pair[1], "status", "--short"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n",
            "for {pair:?}"
        );
        assert_eq!(code(&out), FATAL, "for {pair:?}");
    }

    // Two different bad keys: the one parsed first reports, which for `-c` is the
    // one written first.
    let out = run(
        &repo,
        &home,
        &["-c", "core.ignorecase=bogus", "-c", "core.filemode=bogus", "status", "--short"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n"
    );

    // And a file's values are parsed before the command line's, so a bad value in
    // the file wins over a bad `-c`.
    let (repo, home) = fixture("occurrences-file");
    append_config(&repo, "[core]\n\tfilemode = bogus\n");
    let out = run(&repo, &home, &["-c", "core.ignorecase=bogus", "status", "--short"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'core.filemode'\n"
    );
    assert_eq!(code(&out), FATAL);
}

/// Whether a lone `-h` outruns the config callback is decided **per verb**, and
/// the two gates this dispatcher runs disagree about it.
///
/// git.c:474-477 demotes `-h` to a gentle setup, but that only decides whether a
/// repository is required; each builtin still chooses whether
/// `show_usage_with_options_if_asked()` comes before or after its
/// `repo_config()`. Measured under git 2.55.0 for every verb this dispatcher
/// gates: `status`, `commit`, `gc`, `branch`, `ls-files`, `rev-parse`,
/// `diff-tree` and a dozen more answer 129 with their usage block, while `diff`,
/// `log`, `grep`, `blame`, `tag`, `push` and the rest answer 128 with the config
/// diagnostic. `prepare_repo_settings()` splits the other way: only `diff`,
/// `show`, `pull`, `fetch`, `checkout`, `restore` and `switch` let it beat `-h`.
#[test]
fn whether_h_outruns_the_config_gate_is_decided_per_verb() {
    let (repo, home) = fixture("help-order");

    // The config callback wins: usage is never printed.
    for verb in ["diff", "log", "grep", "blame", "tag", "push", "clean", "stash"] {
        let out = run(&repo, &home, &["-c", "core.ignorecase=bogus", verb, "-h"]);
        assert_eq!(
            stderr(&out),
            "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n",
            "for {verb} -h"
        );
        assert_eq!(code(&out), FATAL, "for {verb} -h");
        assert!(out.stdout.is_empty(), "for {verb} -h");
    }

    // `-h` wins: the usage block prints and the bad value is never looked at.
    for verb in ["status", "commit", "gc", "branch", "ls-files", "rev-parse", "diff-tree"] {
        let out = run(&repo, &home, &["-c", "core.ignorecase=bogus", verb, "-h"]);
        assert_eq!(stderr(&out), "", "for {verb} -h");
        assert_eq!(code(&out), 129, "for {verb} -h");
        assert!(
            out.stdout.starts_with(b"usage: git "),
            "for {verb} -h: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // The settings block splits differently: `diff` lets it beat `-h`, `log`
    // does not — even though `log` lets the *config callback* beat `-h`.
    let out = run(&repo, &home, &["-c", "core.packedGitLimit=bogus", "diff", "-h"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'bogus' for 'core.packedgitlimit': invalid unit\n"
    );
    assert_eq!(code(&out), FATAL);
    let out = run(&repo, &home, &["-c", "core.packedGitLimit=bogus", "log", "-h"]);
    assert_eq!(stderr(&out), "");
    assert_eq!(code(&out), 129);
}

/// The gate runs before the command parses its own options, so a bad value is
/// fatal even on a command line that would otherwise have failed for another
/// reason — and even for a verb that would never have read the key.
#[test]
fn the_gate_fires_before_the_command_looks_at_its_arguments() {
    let (repo, home) = fixture("ordering");

    // A nonexistent pathspec would normally be the complaint.
    let out = run(
        &repo,
        &home,
        &["-c", "core.ignorecase=bogus", "status", "--short", "--", "no-such-path"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'core.ignorecase'\n"
    );
    assert_eq!(code(&out), FATAL);

    // `push.default` is refused by verbs that have nothing to do with pushing.
    for verb in ["branch", "tag", "count-objects", "symbolic-ref"] {
        let out = run(&repo, &home, &["-c", "push.default=bogus", verb]);
        assert_eq!(
            stderr(&out),
            "error: malformed value for push.default: bogus\n\
             error: must be one of nothing, matching, simple, upstream or current\n\
             fatal: unable to parse 'push.default' from command-line config\n",
            "for {verb}"
        );
        assert_eq!(code(&out), FATAL, "for {verb}");
    }
}
