//! `git commit --trailer` weighs its arguments before it writes anything.
//!
//! The trailer engine is shared with `git tag`, and so is the check in front of
//! it:
//!
//! ```c
//! if (validate_trailer_args(trailer_args)) {
//!         ret = -1;
//!         goto out;
//! }
//! ```
//!
//! (`amend_file_with_trailers()`, trailer.c, whose `validate_trailer_args()`
//! rejects an empty argument with `error(_("empty --trailer argument"))` and one
//! whose separator sits at offset 0 — nothing in front of the `:` or `=` — with
//! `error(_("invalid trailer '%s': missing key before separator"), txt)`.)
//! `cmd_commit()` answers any failure of the helper with
//!
//! ```c
//! if (trailer_args.nr) {
//!         if (amend_file_with_trailers(git_path_commit_editmsg(), &trailer_args))
//!                 die(_("unable to pass trailers to --trailers"));
//! ```
//!
//! (builtin/commit.c:1070-1072.) Two lines, exit 128, and **no commit**: the check
//! runs before the message file is amended, so a rejected `--trailer` must not
//! leave a recorded commit behind. Letting the engine take the argument instead
//! produced a different diagnostic *and* a commit, which is the failure this file
//! pins shut.
//!
//! `trailer.separators` widens the separator set, so what counts as "missing key
//! before separator" is configurable; the default set is `=` plus `:`.
//!
//! Expectations are literal. No gpg, no network, no editor.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn thread_slug() -> String {
    format!("{:?}", std::thread::current().id())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_REFLOG_ACTION")
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let out = run(cwd, args);
    assert!(
        out.status.success(),
        "`git {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn repo(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-committrailer-{tag}-{}-{}",
        std::process::id(),
        thread_slug()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    ok(&p, &["init", "-q", "--initial-branch=main", "."]);
    ok(&p, &["commit", "-q", "--allow-empty", "-m", "base"]);
    p
}

/// `git rev-list --count HEAD`, so "no commit was recorded" is checked rather
/// than assumed from an exit code.
fn commits(repo: &Path) -> String {
    ok(repo, &["rev-list", "--count", "HEAD"])
}

/// An empty `--trailer` argument.
#[test]
fn an_empty_trailer_argument_is_two_lines_and_no_commit() {
    let r = repo("empty");
    let out = run(&r, &["commit", "--allow-empty", "--trailer", "", "-m", "x"]);
    assert_eq!(
        stderr(&out),
        "error: empty --trailer argument\n\
         fatal: unable to pass trailers to --trailers\n"
    );
    assert_eq!(code(&out), 128);
    assert_eq!(commits(&r), "1", "a rejected --trailer recorded a commit");
}

/// A separator with no key in front of it, in both of the default spellings.
#[test]
fn a_trailer_with_no_key_before_the_separator_is_two_lines_and_no_commit() {
    let r = repo("nokey");
    for arg in [":x", "=x", ":", "="] {
        let out = run(&r, &["commit", "--allow-empty", "--trailer", arg, "-m", "x"]);
        assert_eq!(
            stderr(&out),
            format!(
                "error: invalid trailer '{arg}': missing key before separator\n\
                 fatal: unable to pass trailers to --trailers\n"
            ),
            "{arg}"
        );
        assert_eq!(code(&out), 128, "{arg}");
    }
    assert_eq!(commits(&r), "1", "a rejected --trailer recorded a commit");
}

/// The first bad argument decides, even when a good one precedes it, and the
/// good one is not applied on the way past.
#[test]
fn a_later_bad_trailer_still_rejects_the_whole_commit() {
    let r = repo("mixed");
    let out = run(
        &r,
        &[
            "commit",
            "--allow-empty",
            "--trailer",
            "Acked-by: X <x@example.com>",
            "--trailer",
            ":bad",
            "-m",
            "x",
        ],
    );
    assert_eq!(
        stderr(&out),
        "error: invalid trailer ':bad': missing key before separator\n\
         fatal: unable to pass trailers to --trailers\n"
    );
    assert_eq!(code(&out), 128);
    assert_eq!(commits(&r), "1");
}

/// The check is not a blanket ban on punctuation: a well-formed trailer still
/// lands, so the guard above cannot be satisfied by refusing everything.
#[test]
fn a_well_formed_trailer_is_still_applied() {
    let r = repo("good");
    let out = run(
        &r,
        &[
            "commit",
            "--allow-empty",
            "--trailer",
            "Acked-by: X <x@example.com>",
            "-m",
            "subject",
        ],
    );
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        ok(&r, &["log", "-1", "--format=%B"]),
        "subject\n\nAcked-by: X <x@example.com>"
    );
    assert_eq!(commits(&r), "2");
}

/// `trailer.separators` is what the check consults, so a configured separator
/// makes a previously acceptable argument keyless.
#[test]
fn a_configured_separator_widens_what_counts_as_keyless() {
    let r = repo("sep");
    ok(&r, &["config", "trailer.separators", ":#"]);
    let out = run(&r, &["commit", "--allow-empty", "--trailer", "#x", "-m", "x"]);
    assert_eq!(
        stderr(&out),
        "error: invalid trailer '#x': missing key before separator\n\
         fatal: unable to pass trailers to --trailers\n"
    );
    assert_eq!(code(&out), 128);
    assert_eq!(commits(&r), "1");
}
