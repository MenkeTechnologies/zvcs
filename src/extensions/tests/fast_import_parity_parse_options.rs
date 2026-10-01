//! `git fast-import`'s command line since 2.56, which reads it with
//! `parse_options()` over `fast_import_options[]` (builtin/fast-import.c:
//! 3988-4000, 4130-4196) instead of the hand-written walk 2.55 used.
//!
//! What moved: an unknown option is ``error: unknown option `x'`` plus the
//! usage block at 129 (2.55: `fatal: unknown option --x`, 128); names
//! abbreviate, take their value from the next argument, and refuse a value
//! where they take none; `--` is accepted; a positional no longer stops the
//! sweep. What did not move is *when* the command line is read: at the first
//! stream command that is not `feature`/`option`, or at EOF — so a bad
//! `feature` line still wins over a bad option. Every expectation is stock
//! git 2.56.0's, measured in the same fixture.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `git fast-import -h` on stock git 2.56.0, which `usage_with_options()`
/// also prints on stderr after an unknown option.
const USAGE: &str = "\
usage: git fast-import [<options>]

Common
    --date-format <fmt>   format of the commit/tag dates
    --stats               display some basic statistics (objects, packfiles and memory)
    --quiet               disable the output shown by --stats
    --force               force updating modified existing branches
    --done                require a terminating 'done' command
    --max-pack-size <n>   maximum size of each output pack file
    --big-file-threshold <n>
                          maximum size of a blob that will be deltified
    --depth <n>           maximum delta depth
    --active-branches <n> maximum number of branches to maintain active

Marks
    --import-marks <file> import marks from <file>
    --import-marks-if-exists <file>
                          import marks from <file> if it exists
    --export-marks <file> dump marks to <file>
    --[no-]relative-marks are --(import|export)-marks= paths relative to '.git/info/fast-import'?

Submodule rewrite
    --rewrite-submodules-from <name:filename>
                          rewrite object IDs for submodule <name> from <filename>
    --rewrite-submodules-to <name:filename>
                          rewrite object IDs for submodule <name> to <filename>

Signing
    --signed-commits <mode>
                          how to handle signed commits
    --signed-tags <mode>  how to handle signed tags

";

/// A fresh repository under a directory unique to this test.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fi-parseopt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let status = Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
    root
}

/// `git fast-import <args>` with `stream` on stdin: (stdout, stderr, status).
fn fast_import(repo: &Path, args: &[&str], stream: &str) -> (String, String, i32) {
    let mut child = Command::new(BIN)
        .arg("fast-import")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

const BLOB: &str = "blob\nmark :1\ndata 1\nx\n";

/// The parity case: an unknown option, a stream that stores a blob. The
/// command line is read at `blob`, refused there, and nothing is stored.
#[test]
fn unknown_option_is_refused_with_the_usage_block() {
    let repo = fixture("unknown");
    let (stdout, stderr, code) = fast_import(&repo, &["--no-such-flag"], BLOB);
    assert_eq!(code, 129, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(stderr, format!("error: unknown option `no-such-flag'\n{USAGE}"));

    let (_, stderr, code) = fast_import(&repo, &["--quiet", "-xh"], "");
    assert_eq!(code, 129, "{stderr}");
    assert_eq!(stderr, format!("error: unknown switch `x'\n{USAGE}"));

    let (_, stderr, code) = fast_import(&repo, &["--quiet", "--d=1"], "");
    assert_eq!(code, 129, "{stderr}");
    assert_eq!(
        stderr,
        format!("error: ambiguous option: d=1 (could be --done or --depth)\n{USAGE}")
    );
}

/// `PARSE_OPT_ERROR` and `check_typos()`: one `error:` line, no block, 129.
#[test]
fn value_and_typo_refusals_print_one_line() {
    let repo = fixture("one-line");
    for (args, want) in [
        (&["--quiet", "--depth"][..], "error: option `depth' requires a value\n"),
        (&["--quiet", "--stats=1"], "error: option `stats' takes no value\n"),
        (&["--quiet", "--no-relative-marks=1"], "error: option `no-relative-marks' takes no value\n"),
        (&["--quiet", "-dep"], "error: did you mean `--dep` (with two dashes)?\n"),
    ] {
        let (stdout, stderr, code) = fast_import(&repo, args, "");
        assert_eq!((code, stdout.as_str(), stderr.as_str()), (129, "", want), "{args:?}");
    }
}

/// Accepted spellings 2.55 refused: `--` and a separate value.
#[test]
fn dashdash_and_detached_values_are_accepted() {
    let repo = fixture("accepted");
    for args in [&["--quiet", "--"][..], &["--quiet", "--depth", "5"], &["--quiet", "--forc"]] {
        let (_, stderr, code) = fast_import(&repo, args, BLOB);
        assert_eq!((code, stderr.as_str()), (0, ""), "{args:?}");
    }
}

/// Callbacks run as the sweep reaches them, so an option after a positional
/// still dies before `usage_with_options()` refuses the positional; and the
/// value checks are `die()`s, in the form the callbacks word them.
#[test]
fn callbacks_die_in_argv_order() {
    let repo = fixture("callbacks");
    for (args, stream, want) in [
        (&["--quiet", "foo", "--depth=x"][..], "", "fatal: --depth: argument must be a non-negative integer\n"),
        (&["--quiet", "--max-pack-size=abc"], "", "fatal: --max-pack-size: argument must be a non-negative integer\n"),
        (&["--quiet"], "option git max-pack-size=zz\n", "fatal: --max-pack-size: argument must be a non-negative integer\n"),
        (&["--quiet"], "option git depth=99999\n", "fatal: --depth cannot exceed 8191\n"),
    ] {
        let (_, stderr, code) = fast_import(&repo, args, stream);
        assert_eq!((code, stderr.as_str()), (128, want), "{args:?} {stream:?}");
    }
}

/// `feature` and `option` lines are read before the command line, matched
/// exactly, and refused once a data command has been seen.
#[test]
fn stream_features_come_first_and_match_exactly() {
    let repo = fixture("features");
    for (args, stream, want) in [
        (
            &["--quiet", "--no-such-flag"][..],
            "feature foo\nblob\ndata 1\nx\n",
            "fatal: this version of fast-import does not support feature foo.\n",
        ),
        (&["--quiet"], "feature date-format\n", "fatal: this version of fast-import does not support feature date-format.\n"),
        (&["--quiet"], "feature force=1\n", "fatal: this version of fast-import does not support feature force=1.\n"),
        (&["--quiet"], "blob\ndata 1\nx\noption git quiet\n", "fatal: got option command 'quiet' after data command\n"),
        (&["--quiet"], "blob\ndata 1\nx\nfeature force\n", "fatal: got feature command 'force' after data command\n"),
        // The early scan only knows the exact spelling, so the abbreviation
        // `parse_options()` would accept arrives too late for the stream.
        (
            &["--quiet", "--allow-unsafe"],
            "feature export-marks=e1\n",
            "fatal: feature 'export-marks=e1' forbidden in input without --allow-unsafe-features\n",
        ),
        (
            &["--quiet", "--allow-unsafe-features"],
            "feature import-marks=a\nfeature import-marks=b\n",
            "fatal: only one import-marks command allowed per stream\n",
        ),
        (
            &["--quiet", "--allow-unsafe-features"],
            "feature import-marks=nope\n",
            "fatal: cannot read 'nope': No such file or directory\n",
        ),
    ] {
        let (_, stderr, code) = fast_import(&repo, args, stream);
        assert_eq!((code, stderr.as_str()), (128, want), "{args:?} {stream:?}");
    }

    // The command line's marks file replaces the stream's unread.
    let (_, stderr, code) = fast_import(
        &repo,
        &["--quiet", "--allow-unsafe-features", "--import-marks-if-exists=nope2"],
        "feature import-marks=nope\n",
    );
    assert_eq!((code, stderr.as_str()), (0, ""));
}
