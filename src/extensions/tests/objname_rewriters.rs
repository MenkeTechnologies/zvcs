//! `get_oid_basic()`'s full-length-hex rule in the four commands that rewrite or
//! summarise a range: `filter-branch`, `range-diff`, `request-pull`, `replay`.
//!
//! `object-name.c`'s first branch decodes a name of exactly `hexsz` hex digits
//! into an object id **without asking the object database whether that object
//! exists**. So a well-formed-but-absent id is not a parse failure: it resolves,
//! and the command fails later — somewhere else, with a different message, and
//! often with a different exit status. A port that resolves only through
//! gitoxide's `rev_parse_single()` collapses the two, which is what every case
//! below pins apart.
//!
//! Each case is paired with a control token that resolves to nothing at all
//! (`nosuchthing`), because the two halves have to move in opposite directions:
//! a fix that reports the *absent-object* message for both is as wrong as the
//! bug it replaced. Every expectation was captured from stock git 2.55.0 in the
//! same fixture before being written down.
//!
//! Both streams are also checked for `src/ported/` — the four commands used to
//! propagate a gitoxide error through `?`, printing this port's own vendored
//! source paths and line numbers at the user. That leak reproduced with the
//! control token too, so it is asserted on every case rather than only the ones
//! the hex rule touches.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed object name the repository will never have.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// The same name in upper case. `get_oid_hex()` folds through `hexval()`, so
/// git accepts it — and some messages echo it back as written rather than
/// folded, which is only visible with a name that is not already lower case.
const ABSENT_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";

/// A token that resolves to nothing at all: the control for every case.
const CONTROL: &str = "nosuchthing";

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// Identity and both timestamps are pinned so the fixture's commit ids — which
/// several of these messages quote — are the same on every machine.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    // The leak these commands used to have: a gitoxide error carried out to the
    // user through `?`, naming this port's vendored sources.
    for stream in [&stdout, &stderr] {
        assert!(
            !stream.contains("src/ported/"),
            "git {args:?} leaked a vendored source path:\n{stream}"
        );
    }
    (stdout, stderr, out.status.code().unwrap_or(-1))
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

/// Three commits on `main`, tagged `v1` at the tip.
///
/// Three is the minimum every case needs: `HEAD~2..HEAD~1` and `HEAD~1..HEAD`
/// have to be two *different* non-empty ranges for `range-diff` to have anything
/// to compare, and `request-pull` needs a base that is not the tip.
///
/// Each test gets its own directory because `filter-branch` and `replay` write
/// to the repository they are given.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-rw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    for n in 1..=3 {
        let mut body = String::new();
        for line in 1..=n {
            body.push_str(&format!("line{line}\n"));
        }
        std::fs::write(repo.join("f.txt"), body).unwrap();
        must(&repo, &home, &["add", "f.txt"]);
        must(&repo, &home, &["commit", "-qm", &format!("c{n}")]);
    }
    must(&repo, &home, &["tag", "v1"]);
    (repo, home)
}

/// Every ref and the object it points at, so a case that must not rewrite
/// anything can prove it.
fn refs(repo: &Path, home: &Path) -> String {
    let (out, stderr, code) = git(
        repo,
        home,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    assert_eq!(code, 0, "for-each-ref failed: {stderr}");
    out
}

// ---------------------------------------------------------------------------
// git filter-branch
// ---------------------------------------------------------------------------

/// `git rev-parse --revs-only --symbolic-full-name <absent-full-hex>` succeeds
/// and prints *nothing*: the name is a revision, so it never becomes a pathspec,
/// but it names no ref either. The script's `test -s "$tempdir"/heads` is what
/// fails, at exit 1 — the `rev-list` that would have complained about the
/// missing object never runs.
///
/// The control does not resolve at all, so `rev-parse` itself dies at 128 with
/// the `--` advice. The two endings differ in message *and* status, which is why
/// treating an absent id as unresolvable was observable.
#[test]
fn filter_branch_absent_full_hex_has_no_ref_to_rewrite() {
    let (repo, home) = fixture("fb-tip");
    let before = refs(&repo, &home);

    let (_, stderr, code) = git(&repo, &home, &["filter-branch", "--force", ABSENT]);
    assert_eq!(stderr, "You must specify a ref to rewrite.\n");
    assert_eq!(code, 1);

    let (_, stderr, code) = git(&repo, &home, &["filter-branch", "--force", CONTROL]);
    assert_eq!(
        stderr,
        format!(
            "fatal: ambiguous argument '{CONTROL}': unknown revision or path not in the working \
             tree.\nUse '--' to separate paths from revisions, like this:\n'git <command> \
             [<revision>...] -- [<file>...]'\n"
        )
    );
    assert_eq!(code, 128);

    assert_eq!(refs(&repo, &home), before, "nothing may have been rewritten");
}

/// A range is the shape that gets *past* the heads check: `<absent>..HEAD` still
/// names `refs/heads/main` through its right endpoint, so the script reaches
/// `git rev-list --stdin`, which is the first thing to read the object back and
/// die `bad object <hex>`. The script's own `die` follows on the next line.
///
/// The control fails one step earlier, in `rev-parse` — and names the *whole*
/// token, not the endpoint, because `handle_dotdot_1()` hands the undivided
/// string to `verify_filename()`.
#[test]
fn filter_branch_absent_full_hex_range_is_a_bad_object() {
    let (repo, home) = fixture("fb-range");
    let before = refs(&repo, &home);

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["filter-branch", "-f", &format!("{ABSENT}..HEAD")],
    );
    assert_eq!(
        stderr,
        format!("fatal: bad object {ABSENT}\nCould not get the commits\n")
    );
    assert_eq!(code, 1);

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["filter-branch", "-f", &format!("{CONTROL}..HEAD")],
    );
    assert_eq!(
        stderr,
        format!(
            "fatal: ambiguous argument '{CONTROL}..HEAD': unknown revision or path not in the \
             working tree.\nUse '--' to separate paths from revisions, like this:\n'git <command> \
             [<revision>...] -- [<file>...]'\n"
        )
    );
    assert_eq!(code, 128);

    assert_eq!(refs(&repo, &home), before, "nothing may have been rewritten");
}

/// `^<absent-full-hex>` is a negative revision with no ref name, so it ends the
/// same way a bare absent id does — the heads check, not the missing object.
/// The control keeps its caret in the diagnostic because `verify_filename()` is
/// handed the token as written.
#[test]
fn filter_branch_negative_absent_full_hex_has_no_ref_to_rewrite() {
    let (repo, home) = fixture("fb-caret");

    let (_, stderr, code) = git(&repo, &home, &["filter-branch", "-f", &format!("^{ABSENT}")]);
    assert_eq!(stderr, "You must specify a ref to rewrite.\n");
    assert_eq!(code, 1);

    let (_, stderr, code) = git(&repo, &home, &["filter-branch", "-f", &format!("^{CONTROL}")]);
    assert!(
        stderr.starts_with(&format!("fatal: ambiguous argument '^{CONTROL}':")),
        "the caret belongs in the message: {stderr}"
    );
    assert_eq!(code, 128);
}

// ---------------------------------------------------------------------------
// git range-diff
// ---------------------------------------------------------------------------

/// `is_range_diff_range()` runs the operand through `setup_revisions()`, whose
/// argument vector ends in a literal `--`. That makes `seen_dashdash` true, so a
/// token that simply does not resolve is `bad revision '<token>'` rather than
/// the `--` advice — while an absent full-length hex resolves and dies earlier,
/// in `handle_dotdot_1()`, as `Invalid revision range <token>`.
///
/// Both operands are covered because upstream's `&&` short-circuit only reaches
/// the second when the first is a range.
#[test]
fn range_diff_absent_full_hex_endpoint_is_an_invalid_range() {
    let (repo, home) = fixture("rd-two");

    for (bad, other) in [
        (format!("{ABSENT}..HEAD"), "HEAD~1..HEAD".to_string()),
        ("HEAD~1..HEAD".to_string(), format!("{ABSENT}..HEAD")),
    ] {
        let (_, stderr, code) = git(&repo, &home, &["range-diff", &bad, &other]);
        assert_eq!(
            stderr,
            format!("fatal: Invalid revision range {ABSENT}..HEAD\n"),
            "operands {bad} {other}"
        );
        assert_eq!(code, 128);
    }

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["range-diff", &format!("{CONTROL}..HEAD"), "HEAD~1..HEAD"],
    );
    assert_eq!(stderr, format!("fatal: bad revision '{CONTROL}..HEAD'\n"));
    assert_eq!(code, 128);
}

/// The name is echoed back as written. `get_oid_hex()` is case-insensitive, so
/// an upper-case operand resolves; nothing folds it before `dotdot_missing()`
/// quotes it, so a fix that normalised the name would be visible here.
#[test]
fn range_diff_absent_full_hex_keeps_the_operands_case() {
    let (repo, home) = fixture("rd-case");
    let (_, stderr, code) = git(
        &repo,
        &home,
        &["range-diff", &format!("{ABSENT_UPPER}..HEAD"), "HEAD~1..HEAD"],
    );
    assert_eq!(
        stderr,
        format!("fatal: Invalid revision range {ABSENT_UPPER}..HEAD\n")
    );
    assert_eq!(code, 128);
}

/// The symmetric form takes a different route: `<a>...<b>` is split into two
/// ordinary ranges and each is resolved by an inner `git log`, so the failure is
/// that log's message followed by range-diff's own `error()` at 255 — and the
/// range named is the *rewritten* one, `HEAD..<a>`, not the operand.
#[test]
fn range_diff_symmetric_absent_full_hex_fails_in_the_inner_log() {
    let (repo, home) = fixture("rd-sym");

    let (_, stderr, code) = git(&repo, &home, &["range-diff", &format!("{ABSENT}...HEAD")]);
    assert_eq!(
        stderr,
        format!(
            "fatal: Invalid revision range HEAD..{ABSENT}\n\
             error: could not parse log for 'HEAD..{ABSENT}'\n"
        )
    );
    assert_eq!(code, 255);

    let (_, stderr, code) = git(&repo, &home, &["range-diff", &format!("{CONTROL}...HEAD")]);
    assert_eq!(
        stderr,
        format!(
            "fatal: ambiguous argument 'HEAD..{CONTROL}': unknown revision or path not in the \
             working tree.\nUse '--' to separate paths from revisions, like this:\n'git <command> \
             [<revision>...] -- [<file>...]'\n\
             error: could not parse log for 'HEAD..{CONTROL}'\n"
        )
    );
    assert_eq!(code, 255);
}

/// `--find-object` compares object ids, so `diff_opt_find_object()`'s
/// `repo_get_oid()` never consults the database: an absent full-length hex is a
/// perfectly good filter that matches nothing, and the run *succeeds*. This is
/// the case where getting the rule wrong turns a stock exit 0 into a failure,
/// and the only one here whose fix moves an exit status the other way.
#[test]
fn range_diff_find_object_absent_full_hex_matches_nothing() {
    let (repo, home) = fixture("rd-find");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["range-diff", "--find-object", ABSENT, "HEAD~2..HEAD~1", "HEAD~1..HEAD"],
    );
    assert_eq!(stderr, "");
    assert_eq!(code, 0);
    // The two ranges hold one commit each and nothing pairs them, so both are
    // listed as unmatched — `--find-object` deferred, not applied to the walk.
    assert_eq!(stdout.lines().count(), 2, "unexpected body:\n{stdout}");

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["range-diff", "--find-object", CONTROL, "HEAD~2..HEAD~1", "HEAD~1..HEAD"],
    );
    assert_eq!(stderr, format!("error: unable to resolve '{CONTROL}'\n"));
    assert_eq!(code, 129);
}

// ---------------------------------------------------------------------------
// git request-pull
// ---------------------------------------------------------------------------

/// The script asks three questions about `$3` in turn, and the third —
/// `git rev-parse --quiet --verify "$local"` — answers *yes* for an absent
/// full-length hex. So `$head` is non-empty, the `Not a valid revision` bail is
/// skipped, and the next line's `"$head"^0` is what fails: `Ambiguous revision`.
///
/// The control fails the third question too and stops at the earlier message.
/// Both exit 1, so the message is the only signal.
#[test]
fn request_pull_absent_full_hex_is_ambiguous_not_invalid() {
    let (repo, home) = fixture("rp-end");

    let (_, stderr, code) = git(&repo, &home, &["request-pull", "HEAD~1", ".", ABSENT]);
    assert_eq!(stderr, format!("fatal: Ambiguous revision: {ABSENT}\n"));
    assert_eq!(code, 1);

    let (_, stderr, code) = git(&repo, &home, &["request-pull", "HEAD~1", ".", CONTROL]);
    assert_eq!(stderr, format!("fatal: Not a valid revision: {CONTROL}\n"));
    assert_eq!(code, 1);
}

/// The `<start>` operand takes the opposite route: the script asks for
/// `"$base"^0` directly, and peeling an absent object fails whichever way the
/// name was resolved. Pinned so the fix to `$3` above is not copied onto an
/// operand that must keep its current message.
#[test]
fn request_pull_absent_full_hex_base_is_still_invalid() {
    let (repo, home) = fixture("rp-base");

    let (_, stderr, code) = git(&repo, &home, &["request-pull", ABSENT, ".", "HEAD"]);
    assert_eq!(stderr, format!("fatal: Not a valid revision: {ABSENT}\n"));
    assert_eq!(code, 1);
}

// ---------------------------------------------------------------------------
// git replay
// ---------------------------------------------------------------------------

/// `git replay` resolves its revision range through `setup_revisions()`, so the
/// three endings are the ones `revision.c` produces, all at 128: a range whose
/// endpoint resolves but is absent is `Invalid revision range <token>`, a bare
/// absent id is `bad object <hex>` (with the leading `^` already stripped), and
/// a name that resolves to nothing keeps `bad revision`/`ambiguous argument`.
///
/// The repository must be untouched afterwards — `replay` writes refs, and a
/// rejection has to happen before any of that.
#[test]
fn replay_revision_range_reports_setup_revisions_diagnostics() {
    let (repo, home) = fixture("rp-revs");
    let before = refs(&repo, &home);

    let cases: [(&[&str], String); 5] = [
        (
            &["--onto", "HEAD", "0123456789012345678901234567890123456789..HEAD"],
            format!("fatal: Invalid revision range {ABSENT}..HEAD\n"),
        ),
        (
            &["--onto", "HEAD", "nosuchthing..HEAD"],
            format!(
                "fatal: ambiguous argument '{CONTROL}..HEAD': unknown revision or path not in the \
                 working tree.\nUse '--' to separate paths from revisions, like this:\n'git \
                 <command> [<revision>...] -- [<file>...]'\n"
            ),
        ),
        (
            &["--onto", "HEAD", "^0123456789012345678901234567890123456789", "HEAD"],
            format!("fatal: bad object {ABSENT}\n"),
        ),
        (
            &["--onto", "HEAD", "^nosuchthing", "HEAD"],
            format!("fatal: bad revision '^{CONTROL}'\n"),
        ),
        (
            &["--onto", "HEAD", "0123456789012345678901234567890123456789"],
            format!("fatal: bad object {ABSENT}\n"),
        ),
    ];
    for (tail, expected) in cases {
        let mut args = vec!["replay"];
        args.extend_from_slice(tail);
        let (_, stderr, code) = git(&repo, &home, &args);
        assert_eq!(stderr, expected, "args {args:?}");
        assert_eq!(code, 128, "args {args:?}");
    }

    assert_eq!(refs(&repo, &home), before, "a rejected replay writes nothing");
}

/// `--onto` goes through `peel_committish()`, whose `repo_get_oid()` succeeds
/// for a full-length hex — so `parse_object_or_die()` is the next thing to run
/// and it dies first, naming the operand *as written* rather than the id it
/// decoded. That is why the upper-case spelling comes back upper-case, and why
/// this message is neither of the two `--onto` already had.
#[test]
fn replay_onto_absent_full_hex_cannot_be_parsed() {
    let (repo, home) = fixture("rp-onto");

    for name in [ABSENT, ABSENT_UPPER] {
        let (_, stderr, code) = git(&repo, &home, &["replay", "--onto", name, "HEAD~1..HEAD"]);
        assert_eq!(stderr, format!("fatal: unable to parse object: {name}\n"));
        assert_eq!(code, 128);
    }

    let (_, stderr, code) = git(&repo, &home, &["replay", "--onto", CONTROL, "HEAD~1..HEAD"]);
    assert_eq!(
        stderr,
        format!("fatal: '{CONTROL}' is not a valid commit-ish for --onto\n")
    );
    assert_eq!(code, 128);
}
