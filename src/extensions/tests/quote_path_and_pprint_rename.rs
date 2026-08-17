//! `quote.c`'s `quote_c_style()` and `diff.c`'s `pprint_rename()`, over paths that
//! carry the bytes those two functions have an opinion about.
//!
//! Two behaviours are pinned here, both measured from stock git 2.55.0 against the
//! same fixture:
//!
//! * `pprint_rename()` (diff.c:2398-2405) checks `quote_c_style(a, NULL, NULL, 0)`
//!   on *both* names first, and when either would be quoted it abandons the
//!   `pfx{a => b}sfx` factoring entirely and prints `"a" => "b"` — braces cannot be
//!   spliced into a quoted string. A copy that skips that branch prints raw braces
//!   around an unescaped path.
//!
//! * `core.quotePath` (`quote_path_fully`) is one process-wide flag, read once, and
//!   `cq_lookup[]` splits `0x7f` (always octal-escaped) from `0x80..=0xff` (escaped
//!   only while the flag is on). Turning the key off must therefore stop escaping
//!   high bytes and must NOT stop escaping a control byte, a quote or a backslash —
//!   in every verb alike, since git has only the one flag.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use zvcs::quote::{needs_c_quote, quote_two_c_style, quoted_name_bytes};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home_for(dir))
        .env("ZVCS_HOME", home_for(dir))
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn home_for(dir: &Path) -> PathBuf {
    let home = dir.parent().expect("repo has a parent").join("home");
    let _ = std::fs::create_dir_all(&home);
    home
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // stdout is compared as bytes-turned-text: every expectation below is either
    // pure ASCII (git quoted it) or valid UTF-8 (git did not).
    String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned()
}

/// A repository whose paths span the `cq_lookup[]` table: a plain name, a name
/// holding a double quote, one holding a backslash, one holding a tab, and one
/// holding two-byte UTF-8. Two renames share `dir/sub/` and `.txt`, so the
/// brace-factoring form is reachable — and one of the two pairs needs quoting.
fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-quote-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("dir/sub")).unwrap();
    let repo = repo.canonicalize().unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    // macOS decomposes unicode file names on the way to the file system; git's
    // own knob for that would otherwise change which bytes reach quote_c_style.
    git_ok(&repo, &["config", "core.precomposeUnicode", "false"]);

    let sub = repo.join("dir/sub");
    for (raw, content) in [
        (&b"plain.txt"[..], "a\n"),
        (&b"qu\"o.txt"[..], "b\n"),
        (&b"uni-\xc3\xa9.txt"[..], "c\n"),
        (&b"back\\slash.txt"[..], "d\n"),
        (&b"tab\tx.txt"[..], "e\n"),
        // `0x7f` is the table's other `1` entry — the one a fused `0x7f..=0xff`
        // arm folds into the high half that `core.quotePath` governs.
        (&b"del\x7fbyte.txt"[..], "g\n"),
    ] {
        std::fs::write(sub.join(OsStr::from_bytes(raw)), content).unwrap();
    }
    git_ok(&repo, &["add", "-A"]);
    git_ok(&repo, &["commit", "-q", "-m", "base"]);
    git_ok(&repo, &["mv", "dir/sub/plain.txt", "dir/sub/plainer.txt"]);
    git_ok(&repo, &["mv", "dir/sub/qu\"o.txt", "dir/sub/qu\"os.txt"]);
    git_ok(&repo, &["mv", "dir/sub/uni-\u{e9}.txt", "dir/sub/uni-\u{f3}.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "renames"]);
    // One untracked high-byte name, so `status` has something to quote.
    std::fs::write(sub.join(OsStr::from_bytes(b"new-\xc3\xb6.txt")), "f\n").unwrap();
    repo
}

/// diff.c:2401 — `if (qlen_a || qlen_b)` prints the two names quoted, in full, with
/// no brace factoring. The plain pair in the same diff still factors, which is what
/// makes this a branch rather than a blanket rule.
#[test]
fn a_rename_needing_quotes_skips_the_brace_factoring() {
    let repo = fixture("rename");

    assert_eq!(
        git_ok(&repo, &["diff", "-M", "--summary", "HEAD~1", "HEAD"]),
        " rename dir/sub/{plain.txt => plainer.txt} (100%)\n \
         rename \"dir/sub/qu\\\"o.txt\" => \"dir/sub/qu\\\"os.txt\" (100%)\n \
         rename \"dir/sub/uni-\\303\\251.txt\" => \"dir/sub/uni-\\303\\263.txt\" (100%)"
    );

    assert_eq!(
        git_ok(&repo, &["diff", "-M", "--numstat", "HEAD~1", "HEAD"]),
        "0\t0\tdir/sub/{plain.txt => plainer.txt}\n\
         0\t0\t\"dir/sub/qu\\\"o.txt\" => \"dir/sub/qu\\\"os.txt\"\n\
         0\t0\t\"dir/sub/uni-\\303\\251.txt\" => \"dir/sub/uni-\\303\\263.txt\""
    );

    // `--stat` measures its name column from the rendered name, so the quoted form
    // has to be the one that sets the width too.
    assert_eq!(
        git_ok(&repo, &["diff", "-M", "--stat", "HEAD~1", "HEAD"]),
        " dir/sub/{plain.txt => plainer.txt}                       | 0\n \
         \"dir/sub/qu\\\"o.txt\" => \"dir/sub/qu\\\"os.txt\"              | 0\n \
         \"dir/sub/uni-\\303\\251.txt\" => \"dir/sub/uni-\\303\\263.txt\" | 0\n \
         3 files changed, 0 insertions(+), 0 deletions(-)"
    );
}

/// `core.quotePath=false` clears `quote_path_fully`, which is table entry `0` —
/// the high half — and nothing else. `"`, `\` and `\t` are table entries the flag
/// cannot reach, so they stay escaped.
#[test]
fn quote_path_false_frees_only_the_high_half() {
    let repo = fixture("half");
    let off = |args: &[&str]| {
        let mut full = vec!["-c", "core.quotePath=false"];
        full.extend_from_slice(args);
        git_ok(&repo, &full)
    };

    assert_eq!(
        off(&["diff", "--name-only", "HEAD~1", "HEAD"]),
        "dir/sub/plainer.txt\n\"dir/sub/qu\\\"os.txt\"\ndir/sub/uni-\u{f3}.txt"
    );

    // `ls-files` reads the key itself; it must reach quote_c_style rather than
    // switch quoting off wholesale — the backslash, tab and DEL names still quote,
    // and only the high-byte one goes out raw.
    assert_eq!(
        off(&["ls-files"]),
        "\"dir/sub/back\\\\slash.txt\"\n\
         \"dir/sub/del\\177byte.txt\"\n\
         dir/sub/plainer.txt\n\
         \"dir/sub/qu\\\"os.txt\"\n\
         \"dir/sub/tab\\tx.txt\"\n\
         dir/sub/uni-\u{f3}.txt"
    );

    assert_eq!(off(&["status", "--porcelain"]), "?? dir/sub/new-\u{f6}.txt");
}

/// One flag, so two verbs rendering the same raw diff must agree. `log --raw` and
/// `whatchanged --raw` used to disagree because only one of them seeded it.
#[test]
fn log_raw_and_whatchanged_agree_on_quote_path() {
    let repo = fixture("agree");
    let expected = ":100644 100644 7898192 7898192 R100\tdir/sub/plain.txt\tdir/sub/plainer.txt\n\
                    :100644 100644 6178079 6178079 R100\t\"dir/sub/qu\\\"o.txt\"\t\"dir/sub/qu\\\"os.txt\"\n\
                    :100644 100644 f2ad6c7 f2ad6c7 R100\tdir/sub/uni-\u{e9}.txt\tdir/sub/uni-\u{f3}.txt";

    let log = git_ok(
        &repo,
        &["-c", "core.quotePath=false", "log", "--raw", "-1", "--format="],
    );
    let whatchanged = git_ok(
        &repo,
        &[
            "-c",
            "core.quotePath=false",
            "whatchanged",
            "--i-still-use-this",
            "-1",
            "--format=",
        ],
    );
    assert_eq!(log.trim_start_matches('\n'), expected);
    assert_eq!(whatchanged.trim_start_matches('\n'), expected);
}

/// The table itself, called directly. `quote_path_fully` defaults to true and this
/// process seeds it from the repository it is run in (`core.quotePath` unset there),
/// so these are the default-configuration rows. Measured from git 2.55.0.
#[test]
fn the_table_matches_cq_lookup() {
    // Table entries `>= ' '` — the named escapes, `"` and `\` — which
    // `cq_must_quote()` cannot switch off whatever `quote_path_fully` is.
    assert_eq!(quoted_name_bytes(b"a\x07b"), b"\"a\\ab\"");
    assert_eq!(quoted_name_bytes(b"a\x08b"), b"\"a\\bb\"");
    assert_eq!(quoted_name_bytes(b"a\tb"), b"\"a\\tb\"");
    assert_eq!(quoted_name_bytes(b"a\nb"), b"\"a\\nb\"");
    assert_eq!(quoted_name_bytes(b"a\x0bb"), b"\"a\\vb\"");
    assert_eq!(quoted_name_bytes(b"a\x0cb"), b"\"a\\fb\"");
    assert_eq!(quoted_name_bytes(b"a\rb"), b"\"a\\rb\"");
    assert_eq!(quoted_name_bytes(b"a\"b"), b"\"a\\\"b\"");
    assert_eq!(quoted_name_bytes(b"a\\b"), b"\"a\\\\b\"");

    // Table entry `-1`: never quoted, space included.
    assert_eq!(quoted_name_bytes(b"plain path.txt"), b"plain path.txt");
    assert!(!needs_c_quote(b"plain path.txt"));

    // Table entry `1`: octal, and `0x7f` is one of them — the row a fused
    // `0x7f..=0xff` arm loses.
    assert_eq!(quoted_name_bytes(b"a\x01b"), b"\"a\\001b\"");
    assert_eq!(quoted_name_bytes(b"a\x7fb"), b"\"a\\177b\"");

    // Table entry `0`: the high half, octal while the flag is on.
    assert_eq!(quoted_name_bytes(b"a\xc3\xa9b"), b"\"a\\303\\251b\"");

    // `quote_two_c_style()`: the prefix is inside the quotes when either half needs
    // them, and concatenated bare when neither does.
    assert_eq!(quote_two_c_style(b"a/", b"plain"), b"a/plain");
    assert_eq!(quote_two_c_style(b"a/", b"t\tb"), b"\"a/t\\tb\"");
}
