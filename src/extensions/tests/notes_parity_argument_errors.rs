//! `git notes` argument handling, in the order and with the wording git's
//! `parse_options()` produces.
//!
//! Four behaviors are pinned here, each a place where a plausible-looking
//! implementation diverges:
//!
//! * A missing option argument reports one `error:` line and nothing else.
//!   `parse-options.c:60` raises it through `error()` and returns -1, which
//!   `get_value()` turns into `PARSE_OPT_ERROR` (`parse-options.c:606`); the
//!   `PARSE_OPT_ERROR` arm of `parse_options()` (`parse-options.c:1198-1201`)
//!   calls `exit(129)` without ever reaching `usage_with_options()`. Only the
//!   `PARSE_OPT_UNKNOWN` arm (`parse-options.c:1214-1223`) prints a usage block,
//!   so the two refusals are distinguishable by output shape alone.
//!
//! * `git notes copy --stdin <anything>` is "too many arguments"
//!   (`builtin/notes.c:599-606`) — the stdin form takes no operands at all.
//!
//! * `git notes merge` validates its mode and operand count
//!   (`builtin/notes.c:919-932`) *before* it looks at `-s`
//!   (`builtin/notes.c:950`), so `git notes merge -s bogus` is a missing-operand
//!   report and `git notes merge -s ours --abort` is a mode-mixing report.
//!
//! * Short options bundle the way `parse_short_opt()` bundles them, and
//!   `OPT__VERBOSITY`'s counter saturates through zero
//!   (`parse-options-cb.c:65-85`): `-vq` is `-1`, not `0`.
//!
//! Every case is also run against the system `git` and compared on stdout,
//! stderr and exit code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_git");

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A command carrying the deterministic, isolated environment shared by the
/// fixture builder and the run under test.
fn env_cmd(bin: &str, repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000");
    c
}

/// Run a system-`git` command in the fixture, asserting success. Fixture setup
/// only, never the behavior under test.
fn git(repo: &Path, home: &Path, args: &[&str]) {
    let ok = env_cmd("git", repo, home).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} failed");
}

/// A one-commit repo with a note on HEAD and a second notes ref to merge from.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("zvcs-notesargs-{tag}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    git(&repo, &home, &["notes", "--ref=commits", "add", "-m", "AAA", "HEAD"]);
    git(&repo, &home, &["notes", "--ref=other", "add", "-m", "BBB", "HEAD"]);
    (repo, home)
}

/// Run `<bin> <args>` in a fresh fixture with empty stdin, then tear it down.
fn run(bin: &str, tag: &str, args: &[&str]) -> Output {
    let (repo, home) = fixture(tag);
    let out = env_cmd(bin, &repo, &home)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    out
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Run one argument list against both binaries and assert full agreement.
fn both(tag: &str, args: &[&str]) -> Output {
    let z = run(BIN, tag, args);
    let g = run("git", &format!("{tag}-g"), args);
    assert_eq!(z.status.code(), g.status.code(), "{args:?}: exit code");
    assert_eq!(stdout(&z), stdout(&g), "{args:?}: stdout");
    assert_eq!(stderr(&z), stderr(&g), "{args:?}: stderr");
    z
}

#[test]
fn missing_option_argument_prints_no_usage_block() {
    // Every spelling of "this switch needs a value" across the notes
    // subcommands: one `error:` line, exit 129, and no usage block — which is
    // what separates PARSE_OPT_ERROR from PARSE_OPT_UNKNOWN.
    let cases: &[(&[&str], &str)] = &[
        (&["notes", "add", "-m"], "error: switch `m' requires a value\n"),
        (&["notes", "add", "-F"], "error: switch `F' requires a value\n"),
        (&["notes", "add", "-C"], "error: switch `C' requires a value\n"),
        (&["notes", "add", "-c"], "error: switch `c' requires a value\n"),
        (&["notes", "append", "-m"], "error: switch `m' requires a value\n"),
        (&["notes", "edit", "-F"], "error: switch `F' requires a value\n"),
        (&["notes", "--ref"], "error: option `ref' requires a value\n"),
        (
            &["notes", "copy", "--for-rewrite"],
            "error: option `for-rewrite' requires a value\n",
        ),
        (&["notes", "merge", "-s"], "error: switch `s' requires a value\n"),
        (
            &["notes", "merge", "--strategy"],
            "error: option `strategy' requires a value\n",
        ),
    ];
    for (args, want) in cases {
        let z = both(&format!("mv{}", args.join("-").replace('-', "")), args);
        assert_eq!(z.status.code(), Some(129), "{args:?}: exit code");
        assert_eq!(stderr(&z), *want, "{args:?}: the error line, and only it");
        assert_eq!(stdout(&z), "", "{args:?}: nothing on stdout");
    }
}

#[test]
fn unknown_switch_still_prints_the_usage_block() {
    // The contrast case for the test above: PARSE_OPT_UNKNOWN does reach
    // `usage_with_options()`, so an unknown switch keeps its usage block. If the
    // two paths were collapsed into one helper, this test and the previous one
    // cannot both pass.
    let z = both("unk", &["notes", "add", "-Z"]);
    assert_eq!(z.status.code(), Some(129), "unknown switch exits 129");
    let err = stderr(&z);
    assert!(err.starts_with("error: unknown switch `Z'\n"), "got: {err:?}");
    assert!(
        err.contains("usage: git notes add [<options>] [<object>]"),
        "the usage block must follow an unknown switch: {err:?}"
    );
}

#[test]
fn copy_stdin_rejects_positional_arguments() {
    // `builtin/notes.c:599-606`: with --stdin (or --for-rewrite) any operand is
    // "too many arguments", not a silently ignored extra.
    for args in [
        &["notes", "copy", "--stdin", "HEAD"][..],
        &["notes", "copy", "--stdin", "HEAD", "HEAD"][..],
        &["notes", "copy", "--for-rewrite=amend", "HEAD"][..],
    ] {
        let z = both("copystdin", args);
        assert_eq!(z.status.code(), Some(129), "{args:?}: exit code");
        assert!(
            stderr(&z).starts_with("error: too many arguments\n"),
            "{args:?}: {:?}",
            stderr(&z)
        );
    }
    // The bare stdin form, with nothing on stdin, is still fine.
    let ok = both("copystdin-ok", &["notes", "copy", "--stdin"]);
    assert_eq!(ok.status.code(), Some(0), "an operand-less --stdin still works");
}

#[test]
fn merge_checks_mode_and_operands_before_strategy() {
    // `builtin/notes.c:919-932` runs before `:950`, so an invalid strategy is
    // never what gets reported while the operand count or the mode is wrong.
    let cases: &[(&[&str], &str)] = &[
        (&["notes", "merge", "-s", "bogus"], "error: must specify a notes ref to merge\n"),
        (&["notes", "merge", "-s", "bogus", "a", "b"], "error: must specify a notes ref to merge\n"),
        (&["notes", "merge", "-s", "ours"], "error: must specify a notes ref to merge\n"),
        (
            &["notes", "merge", "-s", "bogus", "--abort"],
            "error: cannot mix --commit, --abort or -s/--strategy\n",
        ),
        (
            &["notes", "merge", "-s", "ours", "--abort"],
            "error: cannot mix --commit, --abort or -s/--strategy\n",
        ),
        (
            &["notes", "merge", "--abort", "--commit"],
            "error: cannot mix --commit, --abort or -s/--strategy\n",
        ),
        (&["notes", "merge", "--abort", "extra"], "error: too many arguments\n"),
        (&["notes", "merge", "--commit", "extra"], "error: too many arguments\n"),
    ];
    for (args, want) in cases {
        let z = both("mergeorder", args);
        assert_eq!(z.status.code(), Some(129), "{args:?}: exit code");
        assert!(stderr(&z).starts_with(want), "{args:?}: {:?}", stderr(&z));
    }
    // And the strategy *is* validated once the mode and operands are right.
    let bad = both("mergebadstrat", &["notes", "merge", "-s", "bogus", "other"]);
    assert!(
        stderr(&bad).starts_with("error: unknown -s/--strategy: bogus\n"),
        "{:?}",
        stderr(&bad)
    );
}

#[test]
fn merge_bundles_short_options_and_saturates_verbosity() {
    // `-vsours` is `-v -s ours`, and `-vv` is two `-v` — a per-word match on
    // "-v"/"-s" rejects both as an unknown switch.
    let bundled = both("bundle", &["notes", "merge", "-vsours", "other"]);
    assert_eq!(bundled.status.code(), Some(0), "-vsours must merge: {:?}", stderr(&bundled));
    assert_eq!(
        stdout(&bundled),
        "Using local notes for f6ff480b95036f304c76fb00fc918ce8204cb626\n",
        "-v selects the per-note strategy line, -sours the strategy"
    );

    // `parse_opt_verbosity_cb()` saturates through zero, so `-vq` is quiet
    // (`-1`), not the neutral `0` a naive `+1/-1` counter would give. At `-1`
    // the "Using local notes" line above is suppressed; at `0` it is not.
    let vq = both("vq", &["notes", "merge", "-vq", "-s", "ours", "other"]);
    assert_eq!(vq.status.code(), Some(0), "-vq must still merge: {:?}", stderr(&vq));
    assert_eq!(stdout(&vq), "", "-vq saturates to -1, which is silent");

    // `-qv` saturates the other way, back to a talkative `1`.
    let qv = both("qv", &["notes", "merge", "-qv", "-s", "ours", "other"]);
    assert_eq!(
        stdout(&qv),
        "Using local notes for f6ff480b95036f304c76fb00fc918ce8204cb626\n",
        "-qv saturates to 1"
    );

    // `--no-verbose` and `--no-quiet` both reset to 0 rather than stepping.
    let reset = both("noverb", &["notes", "merge", "-vv", "--no-verbose", "-s", "ours", "other"]);
    assert_eq!(
        stdout(&reset),
        "Using local notes for f6ff480b95036f304c76fb00fc918ce8204cb626\n",
        "--no-verbose drops back to 0, which still prints the per-note line but no trace"
    );
}
