//! A well-formed object name the repository does not have, fed to the three
//! revision walkers that are not `git log`: `whatchanged`, `shortlog` and
//! `fast-export`.
//!
//! `get_oid_basic()`'s first branch (`object-name.c`) is
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) { … return 0; }
//! ```
//!
//! — a name of exactly `hexsz` hex digits *is* the object id, decoded without the
//! object database ever being consulted. So resolution **succeeds** for an id
//! that is simply absent, and the command dies further along, at a different
//! place, with a different message:
//!
//! * a bare name (or one behind `^`) reaches `get_reference()`, which is
//!   `die("bad object %s", name)` with the `^` already stripped;
//! * a range reaches `handle_dotdot_1()`, whose `parse_object()` on the endpoints
//!   fails into `dotdot_missing()` — `Invalid revision range %s` for `A..B`,
//!   `Invalid symmetric difference expression %s` for `A...B`, both naming the
//!   whole token rather than the endpoint that failed;
//! * and if a file of that name is sitting in the working tree,
//!   `verify_non_filename()` gets there first with "both revision and filename".
//!
//! A resolver that asks the database instead — gitoxide's `rev_parse_single()` —
//! collapses all of that into "this argument did not resolve", which these three
//! commands used to report as `verify_filename()`'s three-line "ambiguous
//! argument", or (whatchanged) swallow as a pathspec and exit 0.
//!
//! Every expectation below was captured from stock git 2.55.0 in the same
//! fixture before being written down. The controls next to them are the reason
//! the fix cannot be "print `bad object` whenever resolution fails": a name that
//! is *not* full-length hex still has to produce the old messages.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Forty hex digits that decode fine and name nothing.
const ABSENT: &str = "0123456789012345678901234567890123456789";
/// A second one, so a range can have both endpoints missing.
const ABSENT_2: &str = "fedcba9876543210fedcba9876543210fedcba98";
/// `hexval()` is case-insensitive, so this decodes too — and git echoes the name
/// back exactly as written rather than folding it.
const ABSENT_UPPER: &str = "ABCDEF0123456789012345678901234567890123";

/// The three commands under test, each with whatever it needs before its
/// revision arguments. `whatchanged` refuses to run without the opt-in flag.
const WALKERS: [&[&str]; 3] = [
    &["whatchanged", "--i-still-use-this"],
    &["shortlog"],
    &["fast-export"],
];

fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — fixture construction only,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

/// Two commits on `main` plus a `side` branch, so the controls have a real range
/// to walk and a real revision to resolve.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-walkers-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    must(&repo, &home, &["add", "a.txt"]);
    must(&repo, &home, &["commit", "-qm", "first"]);
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    must(&repo, &home, &["add", "b.txt"]);
    must(&repo, &home, &["commit", "-qm", "second"]);
    must(&repo, &home, &["branch", "side"]);

    (root, repo)
}

/// `verify_filename(…, diagnose_misspelt_rev = 1)`'s three lines, for a token
/// that is neither a revision nor a path.
fn ambiguous(token: &str) -> String {
    format!(
        "fatal: ambiguous argument '{token}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    )
}

/// Run one revision token through all three walkers and require the same fatal
/// from each: they share `setup_revisions()`, so a message that differs between
/// them is a divergence in one command's private resolver.
fn each_walker_dies_with(repo: &Path, home: &Path, token: &str, expected: &str) {
    for prefix in WALKERS {
        let mut args: Vec<&str> = prefix.to_vec();
        args.push(token);
        let (stdout, stderr, code) = git(repo, home, &args);
        assert_eq!(stderr, expected, "stderr from {args:?}");
        assert_eq!(code, 128, "exit from {args:?}");
        assert_eq!(stdout, "", "stdout from {args:?}");
    }
}

#[test]
fn an_absent_full_length_hex_name_is_bad_object_not_an_ambiguous_argument() {
    let (root, repo) = fixture("bad-object");
    let home = root.join("home");

    // `get_oid_hex()` decoded it, so the failure is `get_reference()`'s, and the
    // name it prints is the one that reached it — the `^` already stripped by
    // `handle_revision_arg_1()`.
    each_walker_dies_with(&repo, &home, ABSENT, &format!("fatal: bad object {ABSENT}\n"));
    each_walker_dies_with(
        &repo,
        &home,
        &format!("^{ABSENT}"),
        &format!("fatal: bad object {ABSENT}\n"),
    );
    // `hexval()` takes either case and git echoes the argument unfolded.
    each_walker_dies_with(
        &repo,
        &home,
        ABSENT_UPPER,
        &format!("fatal: bad object {ABSENT_UPPER}\n"),
    );
}

#[test]
fn a_range_with_an_absent_endpoint_is_invalid_revision_range_naming_the_whole_token() {
    let (root, repo) = fixture("invalid-range");
    let home = root.join("home");

    for token in [
        format!("{ABSENT}..HEAD"),
        format!("HEAD..{ABSENT}"),
        format!("{ABSENT}..{ABSENT_2}"),
        // An empty side is `"HEAD"` to `handle_dotdot_1()` but stays empty in the
        // message, which prints `arg` with the separator put back.
        format!("..{ABSENT}"),
        format!("{ABSENT}.."),
    ] {
        let expected = format!("fatal: Invalid revision range {token}\n");
        each_walker_dies_with(&repo, &home, &token, &expected);
    }
}

#[test]
fn a_symmetric_range_with_an_absent_endpoint_names_the_symmetric_difference() {
    let (root, repo) = fixture("invalid-symdiff");
    let home = root.join("home");

    for token in [
        format!("{ABSENT}...HEAD"),
        format!("HEAD...{ABSENT}"),
        format!("{ABSENT}...{ABSENT_2}"),
    ] {
        let expected = format!("fatal: Invalid symmetric difference expression {token}\n");
        each_walker_dies_with(&repo, &home, &token, &expected);
    }
}

#[test]
fn an_endpoint_that_does_not_resolve_at_all_keeps_the_ambiguous_argument_text() {
    let (root, repo) = fixture("range-controls");
    let home = root.join("home");

    // `handle_dotdot()` needs *both* endpoints out of `get_oid_committish()`
    // before it can reach `dotdot_missing()`; one that fails makes it return -1,
    // and the token falls through to `verify_filename()`. Getting this wrong is
    // the obvious over-fix: "there is a full-length hex in here somewhere".
    for token in [
        format!("{ABSENT}..nosuchthing"),
        format!("nosuchthing..{ABSENT}"),
        format!("nosuchthing...{ABSENT}"),
        // `<hex>^` is one character past `len == hexsz`, so the full-hex branch
        // never sees it and the ordinary parser has to peel an object that is
        // not there: the endpoint fails, and the token is ambiguous rather than
        // an invalid range.
        format!("{ABSENT}^..HEAD"),
    ] {
        each_walker_dies_with(&repo, &home, &token, &ambiguous(&token));
    }

    let caret_range = format!("^{ABSENT}..HEAD");
    each_walker_dies_with(
        &repo,
        &home,
        &caret_range,
        &format!("fatal: bad revision '{caret_range}'\n"),
    );
}

#[test]
fn a_name_that_is_not_full_length_hex_is_untouched_by_the_rule() {
    let (root, repo) = fixture("length-controls");
    let home = root.join("home");

    // 39 and 41 digits miss `len == hexsz` and go to the ordinary parser, which
    // finds nothing; a non-hex character does the same at full length. All three
    // are the pre-existing "ambiguous argument", and a rule that keyed off
    // "looks hexish" would break every one of them.
    for token in [
        &ABSENT[..39],
        &format!("{ABSENT}0")[..],
        "z123456789012345678901234567890123456789",
        "nosuchthing",
    ] {
        each_walker_dies_with(&repo, &home, token, &ambiguous(token));
    }
    for token in ["nosuchthing..HEAD", "nosuchthing...HEAD"] {
        each_walker_dies_with(&repo, &home, token, &ambiguous(token));
    }
    each_walker_dies_with(
        &repo,
        &home,
        "^nosuchthing",
        "fatal: bad revision '^nosuchthing'\n",
    );
}

#[test]
fn a_working_tree_file_of_the_same_name_wins_over_bad_object() {
    let (root, repo) = fixture("both-revision-and-filename");
    let home = root.join("home");
    std::fs::write(repo.join(ABSENT), "a file whose name is an object id\n").unwrap();

    // `handle_revision_arg_1()` runs `verify_non_filename()` between resolving
    // the name and looking the object up, so the collision is reported before
    // the object is ever missed. Without that ordering the walkers answer
    // `bad object` here, and `whatchanged` used to take the file as a pathspec
    // and exit 0 with no output at all.
    let expected = format!(
        "fatal: ambiguous argument '{ABSENT}': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
    each_walker_dies_with(&repo, &home, ABSENT, &expected);
    each_walker_dies_with(&repo, &home, &format!("^{ABSENT}"), &expected);

    // The range never reaches that check — `handle_dotdot_1()` only calls it
    // after both endpoints have parsed, which is exactly what fails here.
    let token = format!("{ABSENT}..HEAD");
    each_walker_dies_with(
        &repo,
        &home,
        &token,
        &format!("fatal: Invalid revision range {token}\n"),
    );
}

#[test]
fn resolvable_arguments_still_walk() {
    let (root, repo) = fixture("success-controls");
    let home = root.join("home");

    // The load-bearing control: every fatal above is raised on the path an
    // ordinary revision also takes, so a fix that fired one step too early would
    // turn working commands into exit 128.
    for prefix in WALKERS {
        for token in ["HEAD", "main..side", "main...side"] {
            let mut args: Vec<&str> = prefix.to_vec();
            args.push(token);
            let (_, stderr, code) = git(&repo, &home, &args);
            assert_eq!(stderr, "", "stderr from {args:?}");
            assert_eq!(code, 0, "exit from {args:?}");
        }
        // `HEAD` alone has history to show, so silence here would mean the walk
        // was skipped rather than performed.
        let mut args: Vec<&str> = prefix.to_vec();
        args.push("HEAD");
        let (stdout, _, _) = git(&repo, &home, &args);
        assert!(!stdout.is_empty(), "no output from {args:?}");
    }
}
