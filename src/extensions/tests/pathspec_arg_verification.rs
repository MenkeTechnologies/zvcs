//! `looks_like_pathspec()` decides which arguments a verb may accept as pathspecs
//! without stating them, and every verb that splits revisions from paths has to
//! decide it identically. It used to be spelled four different ways, and each
//! spelling was wrong in its own direction:
//!
//! * `diff-tree` and `whatchanged` accepted *any* leading `:`, so `:/nope`,
//!   `:!nope` and `:^nope` were taken as pathspecs and the command ran on an empty
//!   match set instead of dying.
//! * `grep` demanded a closing `)` alongside the `:(`, so an unterminated
//!   `:(icase` was diagnosed as a missing path rather than handed to the pathspec
//!   parser.
//! * `diff-files` had the rule right but no caller-side notion of *which* argument
//!   had failed revision resolution, so a second bad path was reported with the
//!   first one's "ambiguous argument" wording.
//!
//! git's rule (setup.c:232-260) is: an unescaped `*`, `?` or `[` — the
//! `GIT_GLOB_SPECIAL` class of ctype.c:12, minus the backslash, which only escapes
//! — or the two-byte prefix `:(`. Short magic (`:/`, `:!`, `:^`) is *not* in it;
//! `check_filename()` (setup.c:178-186) strips it and stats what is left.
//!
//! Every expectation here was taken from stock git 2.55.0.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

fn stderr_of(cwd: &Path, home: &Path, args: &[&str]) -> (Option<i32>, String) {
    let o = run(cwd, home, args);
    (o.status.code(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// git's `die_verify_filename()` texts, verbatim.
const AMBIGUOUS: &str = "unknown revision or path not in the working tree.";
const NO_SUCH_PATH: &str = "no such path in the working tree.";

/// One commit touching `a.txt`, plus an unstaged edit so `diff-files` has output.
/// No file is named `:`-anything, which is the whole point of the short-magic cases.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pathspecarg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::write(repo.join("sub/b.txt"), "b\n").unwrap();
    run(&repo, &home, &["add", "a.txt", "sub/b.txt"]);
    run(&repo, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("a.txt"), "a\na2\n").unwrap();
    run(&repo, &home, &["add", "a.txt"]);
    run(&repo, &home, &["commit", "-q", "-m", "two"]);
    std::fs::write(repo.join("a.txt"), "a\na2\ndirty\n").unwrap();
    (repo, home)
}

/// Short pathspec magic certifies nothing on its own: `check_filename()` strips the
/// prefix and stats the remainder, so `:/nope` is a *missing path*, not a pathspec.
///
/// `diff-tree` and `whatchanged` both short-circuited on a leading `:` and so ran
/// happily — `git diff-tree HEAD :!nope` even printed the full diff, because an
/// exclusion of a path that does not exist excludes nothing.
#[test]
fn short_magic_naming_a_missing_path_is_rejected() {
    let (repo, home) = fixture("shortmagic");

    for spec in [":/nope", ":!nope", ":^nope"] {
        let (code, err) = stderr_of(&repo, &home, &["diff-tree", "HEAD", spec]);
        assert_eq!(code, Some(128), "diff-tree {spec} must die: {err}");
        assert!(
            err.contains(&format!("ambiguous argument '{spec}': {AMBIGUOUS}")),
            "diff-tree {spec}: {err}"
        );

        let (code, err) = stderr_of(&repo, &home, &["whatchanged", spec]);
        assert_eq!(code, Some(128), "whatchanged {spec} must die: {err}");
        assert!(
            err.contains(&format!("ambiguous argument '{spec}': {AMBIGUOUS}")),
            "whatchanged {spec}: {err}"
        );
    }
}

/// The other half of the same rule: short magic in front of a path that *does*
/// exist is stripped and accepted, so the fix must not have turned into a blanket
/// refusal of a leading `:`.
#[test]
fn short_magic_naming_a_present_path_is_accepted() {
    let (repo, home) = fixture("shortmagic-ok");

    // `:/a.txt` is the root-relative spelling of the path that changed in HEAD.
    let o = run(&repo, &home, &["diff-tree", "HEAD", ":/a.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("a.txt"));

    // Excluding a path that exists is legal and selects nothing else here.
    let o = run(&repo, &home, &["diff-tree", "HEAD", ":!a.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // Bare `:/` is the repository root and needs no lookup at all.
    let o = run(&repo, &home, &["diff-tree", "HEAD", ":/"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// Long-form magic is accepted on the two-byte `:(` prefix alone. `grep` also
/// required a `)` somewhere in the argument, which sent an unterminated `:(icase`
/// down the missing-path path instead of to the pathspec parser — git reports it as
/// bad *magic*, never as a bad path.
#[test]
fn unterminated_long_magic_reaches_the_pathspec_parser() {
    let (repo, home) = fixture("longmagic");

    let (_, err) = stderr_of(&repo, &home, &["grep", "a", ":(icase"]);
    assert!(
        !err.contains("ambiguous argument") && !err.contains(NO_SUCH_PATH),
        "`:(icase` must not be diagnosed as a path: {err}"
    );
    assert!(err.contains("Missing ')'"), "expected a pathspec-magic complaint: {err}");

    // The terminated form still works, and `--` must not change the verdict.
    let o = run(&repo, &home, &["grep", "a", ":(icase)a.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (_, dashdash) = stderr_of(&repo, &home, &["grep", "a", "--", ":(icase"]);
    assert!(
        dashdash.contains("Missing ')'"),
        "the same spec behind `--` reaches the same parser: {dashdash}"
    );
}

/// `diagnose_misspelt_rev` is set for the argument that failed revision resolution
/// and cleared for every one after it (`revision.c`: `verify_filename(…, j == i)`).
/// `diff-files` never made that distinction, so it accused a trailing path of being
/// an unknown revision.
#[test]
fn only_the_first_path_is_diagnosed_as_a_misspelt_revision() {
    let (repo, home) = fixture("firstpath");

    // `a.txt` verifies, so `nope` is already known to sit in path position.
    let (code, err) = stderr_of(&repo, &home, &["diff-files", "a.txt", "nope"]);
    assert_eq!(code, Some(128), "{err}");
    assert!(err.contains(&format!("nope: {NO_SUCH_PATH}")), "{err}");
    assert!(!err.contains("ambiguous argument"), "{err}");

    // A glob verifies without existing, and puts the next argument in the same spot.
    let (_, err) = stderr_of(&repo, &home, &["diff-files", "a*", "nope"]);
    assert!(err.contains(&format!("nope: {NO_SUCH_PATH}")), "{err}");

    // The leading argument keeps the ambiguous wording — it could have been a rev.
    let (code, err) = stderr_of(&repo, &home, &["diff-files", "nope", "nope2"]);
    assert_eq!(code, Some(128), "{err}");
    assert!(err.contains(&format!("ambiguous argument 'nope': {AMBIGUOUS}")), "{err}");

    // diff-tree already agreed, and must keep agreeing.
    let (_, err) = stderr_of(&repo, &home, &["diff-tree", "HEAD", "a.txt", "nope"]);
    assert!(err.contains(&format!("nope: {NO_SUCH_PATH}")), "{err}");

    // The same split decides a misplaced option. A lone `-` before any path is
    // never handed to `verify_filename()` at all — parse-options leaves it behind
    // and `cmd_diff_files()` prints its usage (129) — but after one it is an option
    // that came too late (128).
    let (code, err) = stderr_of(&repo, &home, &["diff-files", "-"]);
    assert_eq!(code, Some(129), "a leading `-` is a usage error: {err}");
    let (code, err) = stderr_of(&repo, &home, &["diff-files", "a.txt", "-"]);
    assert_eq!(code, Some(128), "{err}");
    assert!(err.contains("option '-' must come before non-option arguments"), "{err}");
}

/// An unescaped glob metacharacter is enough on its own, in every verb — the
/// argument names a *pattern*, so nothing has to exist for it to be a pathspec.
#[test]
fn an_unescaped_glob_needs_no_file_behind_it() {
    let (repo, home) = fixture("glob");

    for spec in ["zz*", "z?z", "z[ab]"] {
        let o = run(&repo, &home, &["diff-tree", "HEAD", spec]);
        assert!(
            o.status.success(),
            "diff-tree {spec}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = run(&repo, &home, &["diff-files", spec]);
        assert!(
            o.status.success(),
            "diff-files {spec}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    // A backslash-escaped one is a literal, so it has to exist like any other name.
    let (code, err) = stderr_of(&repo, &home, &["diff-tree", "HEAD", r"zz\*"]);
    assert_eq!(code, Some(128), "an escaped glob is a plain missing path: {err}");
}
