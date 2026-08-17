//! `get_oid_basic()`'s full-length-hex rule where `cherry-pick`, `revert` and
//! `blame` meet it.
//!
//! git resolves an object name through `get_oid_basic()` (`object-name.c`),
//! whose first branch decodes a name of exactly `hexsz` hex digits and returns
//! **without asking the object database whether the object exists**. Everything
//! these three commands print for an absent-but-well-formed name follows from
//! that one fact, in three different places:
//!
//! * `get_reference()` (`revision.c`) `parse_object()`s the id it was handed and
//!   `die("bad object %s", name)` — so a bare absent hex is `bad object <hex>`,
//!   never `bad revision '<hex>'`, and `name` is the operand *after* a leading
//!   `^` and a trailing `^@`/`^!`/`^-<n>` mark were cut away.
//! * `handle_dotdot_1()` resolves **both** endpoints of a range before looking
//!   either up, so an absent endpoint gets past the resolve and fails at
//!   `parse_object()` — `dotdot_missing()`, which names the whole token.
//! * `is_a_rev()` (`builtin/blame.c`) is the exception that proves the rule: it
//!   follows `repo_get_oid()` with an explicit object-info lookup, so an absent
//!   hex is *not* a rev there and `git blame <absent-hex>` asks for a path.
//!
//! gitoxide's `rev_parse_single()` consults the odb, so a port that uses it as
//! its only resolver collapses all of this into "not a valid object name". The
//! controls in each test — `nosuchthing`, and hex strings one digit short and
//! one digit long — are what separate the rule from a blanket rewrite: they must
//! keep reporting `bad revision`.
//!
//! Every expectation below was captured from stock git 2.55.0 in this same
//! fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed SHA-1 object name no repository will ever contain.
const MISSING: &str = "0123456789012345678901234567890123456789";
/// The same name in the case `ObjectId::from_hex` rejects. git quotes the
/// operand as typed, so the diagnostic must come back uppercase.
const MISSING_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
/// 39 hex digits: one short of `hexsz`, so `get_oid_basic()`'s first branch does
/// not fire and the ordinary parser rejects it.
const HEX39: &str = "012345678901234567890123456789012345678";
/// 41 hex digits: one over, same reasoning.
const HEX41: &str = "01234567890123456789012345678901234567890";

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

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

/// Three commits touching one file, plus a `side` branch one commit back.
///
/// The file matters: several cases need a name that is a *path* as well as a
/// possible revision, and one needs a real blob to name with `HEAD:f.txt`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    for (n, line) in [(1, "one\n"), (2, "two\n"), (3, "three\n")] {
        let mut body = String::new();
        for prev in ["one\n", "two\n", "three\n"].iter().take(n - 1) {
            body.push_str(prev);
        }
        body.push_str(line);
        std::fs::write(repo.join("f.txt"), &body).unwrap();
        must(&repo, &home, &["add", "f.txt"]);
        must(&repo, &home, &["commit", "-qm", &format!("c{n}")]);
    }
    must(&repo, &home, &["branch", "side", "HEAD~1"]);
    (repo, home)
}

/// Assert on stderr and the exit status together: a message that arrives with
/// the wrong status is still a divergence, and every case here is exit 128.
#[track_caller]
fn expect_fatal(repo: &Path, home: &Path, args: &[&str], stderr_expected: &str) {
    let (stdout, stderr, code) = git(repo, home, args);
    assert_eq!(stderr, stderr_expected, "git {args:?} stderr");
    assert_eq!(code, 128, "git {args:?} exit (stderr was {stderr:?})");
    assert_eq!(stdout, "", "git {args:?} stdout");
}

// ---------------------------------------------------------------------------
// site 26 — `sequencer::prepare_revs`, shared by cherry-pick and revert
// ---------------------------------------------------------------------------

/// `get_reference()`'s `die("bad object %s")`, for every spelling that reaches
/// it: bare, `^`-excluded, and the two `add_parents_only()` marks — all of which
/// name the *base*, not the operand.
#[test]
fn sequencer_absent_full_hex_is_bad_object() {
    let (repo, home) = fixture("seq-badobject");
    let expected = format!("fatal: bad object {MISSING}\n");

    for args in [
        vec!["cherry-pick", MISSING],
        vec!["cherry-pick", "-n", MISSING],
        vec!["revert", MISSING],
        vec!["revert", "-n", MISSING],
    ] {
        expect_fatal(&repo, &home, &args, &expected);
    }

    // `handle_revision_arg_1()` strips the `^` before `get_reference()`, and
    // `add_parents_only()` strips its own; the mark is cut off by the NUL git
    // writes over it. All three therefore quote 40 characters, not 41 or 42.
    for spelling in [
        format!("^{MISSING}"),
        format!("{MISSING}^!"),
        format!("{MISSING}^@"),
    ] {
        expect_fatal(&repo, &home, &["cherry-pick", &spelling], &expected);
    }

    // `hexval()` is case-insensitive, and `die()` gets the operand as typed.
    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", MISSING_UPPER],
        &format!("fatal: bad object {MISSING_UPPER}\n"),
    );
}

/// `setup_revisions()` dies on the *first* operand it cannot use, so a good one
/// before or after the absent hex does not change the diagnosis.
#[test]
fn sequencer_bad_object_is_reported_in_operand_order() {
    let (repo, home) = fixture("seq-order");
    let expected = format!("fatal: bad object {MISSING}\n");
    expect_fatal(&repo, &home, &["cherry-pick", "HEAD", MISSING], &expected);
    expect_fatal(&repo, &home, &["cherry-pick", MISSING, "HEAD"], &expected);
    // An unknown option is only reported after the whole list is walked, so the
    // object name still wins.
    expect_fatal(&repo, &home, &["cherry-pick", "--bogus", MISSING], &expected);
}

/// `dotdot_missing()` names the whole token, because both endpoints resolved and
/// only the object lookup failed. Which endpoint failed is not observable.
#[test]
fn sequencer_absent_endpoint_is_invalid_range() {
    let (repo, home) = fixture("seq-range");
    for spec in [
        format!("{MISSING}..HEAD"),
        format!("HEAD..{MISSING}"),
        format!("{MISSING}..{MISSING}"),
        format!("..{MISSING}"),
        format!("{MISSING}.."),
        format!("HEAD:f.txt..{MISSING}"),
        format!("{MISSING}..HEAD:f.txt"),
    ] {
        let expected = format!("fatal: Invalid revision range {spec}\n");
        expect_fatal(&repo, &home, &["cherry-pick", &spec], &expected);
        expect_fatal(&repo, &home, &["revert", &spec], &expected);
    }

    for spec in [
        format!("{MISSING}...HEAD"),
        format!("HEAD...{MISSING}"),
        format!("{MISSING}...{MISSING}"),
        format!("HEAD:f.txt...{MISSING}"),
    ] {
        let expected = format!("fatal: Invalid symmetric difference expression {spec}\n");
        expect_fatal(&repo, &home, &["cherry-pick", &spec], &expected);
        expect_fatal(&repo, &home, &["revert", &spec], &expected);
    }
}

/// `A...B` has merge bases to compute, so it puts both ends through
/// `lookup_commit_reference()` — which prints `object_as_type()`'s line, once
/// per offending endpoint, before the shared fatal. `A..B` only
/// `parse_object()`s and so accepts a blob endpoint outright.
#[test]
fn sequencer_symmetric_non_commit_endpoint_reports_object_type() {
    let (repo, home) = fixture("seq-symdiff-blob");
    let (blob, _, code) = git(&repo, &home, &["rev-parse", "HEAD:f.txt"]);
    assert_eq!(code, 0);
    let blob = blob.trim();

    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", "HEAD:f.txt...HEAD"],
        &format!(
            "error: object {blob} is a blob, not a commit\n\
             fatal: Invalid symmetric difference expression HEAD:f.txt...HEAD\n"
        ),
    );
    // Both ends are looked up, so both complain.
    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", "HEAD:f.txt...HEAD:f.txt"],
        &format!(
            "error: object {blob} is a blob, not a commit\n\
             error: object {blob} is a blob, not a commit\n\
             fatal: Invalid symmetric difference expression HEAD:f.txt...HEAD:f.txt\n"
        ),
    );
    // The non-symmetric form queues the blob instead, and it is
    // `sequencer_pick_revisions()` that refuses it, naming the endpoint as
    // written rather than by id.
    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", "HEAD:f.txt..HEAD"],
        "error: HEAD:f.txt: can't cherry-pick a blob\nfatal: cherry-pick failed\n",
    );
}

/// The controls. A name that does not resolve at all never reaches
/// `get_reference()` or `dotdot_missing()`, and `builtin/revert.c`'s
/// `assume_dashdash` makes `setup_revisions()` report it as `bad revision`.
#[test]
fn sequencer_unresolvable_names_stay_bad_revision() {
    let (repo, home) = fixture("seq-control");
    for spec in [
        "nosuchthing",
        HEX39,
        HEX41,
        "^nosuchthing",
        "nosuchthing..HEAD",
        "nosuchthing...HEAD",
        // A `..`-free spelling built on an absent hex still fails to resolve,
        // because `get_oid_1()` has to read the object to walk to a parent.
        // Only the bare name gets the `bad object` treatment.
        &format!("{MISSING}^"),
        &format!("{MISSING}~1"),
        &format!("{MISSING}:f.txt"),
    ] {
        let expected = format!("fatal: bad revision '{spec}'\n");
        expect_fatal(&repo, &home, &["cherry-pick", spec], &expected);
        expect_fatal(&repo, &home, &["revert", spec], &expected);
    }
    // A range with one unresolvable endpoint returns -1 from
    // `handle_dotdot_1()`, so it never reaches `dotdot_missing()` even though
    // the other endpoint is a well-formed absent hex.
    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", &format!("nosuchthing..{MISSING}")],
        &format!("fatal: bad revision 'nosuchthing..{MISSING}'\n"),
    );
    expect_fatal(
        &repo,
        &home,
        &["cherry-pick", &format!("{HEX39}..HEAD")],
        &format!("fatal: bad revision '{HEX39}..HEAD'\n"),
    );
}

// ---------------------------------------------------------------------------
// site 27 — blame's revision operands
// ---------------------------------------------------------------------------

/// blame hands its operands to `setup_revisions()`, so it inherits
/// `get_reference()`'s wording verbatim — including through `annotate`, which is
/// the same builtin, and through `--reverse`, which resolves its operands in a
/// different function.
#[test]
fn blame_absent_full_hex_is_bad_object() {
    let (repo, home) = fixture("blame-badobject");
    let expected = format!("fatal: bad object {MISSING}\n");

    for args in [
        vec!["blame", MISSING, "--", "f.txt"],
        // No `--`: the last positional is the path, the rest are revisions.
        vec!["blame", MISSING, "f.txt"],
        vec!["annotate", MISSING, "--", "f.txt"],
        vec!["blame", "HEAD", MISSING, "--", "f.txt"],
        vec!["blame", "--reverse", MISSING, "--", "f.txt"],
    ] {
        expect_fatal(&repo, &home, &args, &expected);
    }
    // `handle_revision_arg_1()` strips the `^` before `get_reference()` here too.
    expect_fatal(
        &repo,
        &home,
        &["blame", &format!("^{MISSING}"), "--", "f.txt"],
        &expected,
    );
    expect_fatal(
        &repo,
        &home,
        &["blame", MISSING_UPPER, "--", "f.txt"],
        &format!("fatal: bad object {MISSING_UPPER}\n"),
    );
}

/// `handle_dotdot_1()` again, reached from both of blame's operand paths.
#[test]
fn blame_absent_endpoint_is_invalid_range() {
    let (repo, home) = fixture("blame-range");
    for spec in [format!("{MISSING}..HEAD"), format!("HEAD..{MISSING}")] {
        let expected = format!("fatal: Invalid revision range {spec}\n");
        expect_fatal(&repo, &home, &["blame", &spec, "--", "f.txt"], &expected);
        expect_fatal(&repo, &home, &["annotate", &spec, "--", "f.txt"], &expected);
        // `--reverse` reads ranges itself, but `setup_revisions()` has already
        // refused the operand before `cmd_blame()` looks at it.
        expect_fatal(
            &repo,
            &home,
            &["blame", "--reverse", &spec, "--", "f.txt"],
            &expected,
        );
    }
    let spec = format!("{MISSING}...HEAD");
    expect_fatal(
        &repo,
        &home,
        &["blame", &spec, "--", "f.txt"],
        &format!("fatal: Invalid symmetric difference expression {spec}\n"),
    );
}

/// `is_a_rev()` is the one place blame must **not** take an absent full hex as
/// an object name: it follows `repo_get_oid()` with an object-info lookup, so
/// the name falls through to the path slot. Getting this wrong in the obvious
/// direction — "full hex wins everywhere" — turns both of these into
/// `bad object`, which is why they are here.
#[test]
fn blame_is_a_rev_still_consults_the_object_database() {
    let (repo, home) = fixture("blame-is-a-rev");
    // One positional and no `--`: it is the path, and blame looks for it in HEAD.
    expect_fatal(
        &repo,
        &home,
        &["blame", MISSING],
        &format!("fatal: no such path '{MISSING}' in HEAD\n"),
    );
    // Two positionals: the trailing one is only the revision when `is_a_rev()`
    // says so, and it does not, so `f.txt` is read as the revision instead.
    expect_fatal(
        &repo,
        &home,
        &["blame", "f.txt", MISSING],
        "fatal: bad revision 'f.txt'\n",
    );
}

/// The controls, plus the `--reverse` range that must keep working: the new
/// resolver runs on every operand, so a range whose endpoints are all present
/// has to come back untouched.
#[test]
fn blame_unresolvable_names_stay_bad_revision() {
    let (repo, home) = fixture("blame-control");
    for spec in ["nosuchthing", HEX39, HEX41] {
        let expected = format!("fatal: bad revision '{spec}'\n");
        expect_fatal(&repo, &home, &["blame", spec, "--", "f.txt"], &expected);
        expect_fatal(&repo, &home, &["annotate", spec, "--", "f.txt"], &expected);
    }
    expect_fatal(
        &repo,
        &home,
        &["blame", "nosuchthing..HEAD", "--", "f.txt"],
        "fatal: bad revision 'nosuchthing..HEAD'\n",
    );
    expect_fatal(
        &repo,
        &home,
        &["blame", "--ignore-rev", MISSING, "--", "f.txt"],
        &format!("fatal: cannot find revision {MISSING} to ignore\n"),
    );

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["blame", "--reverse", "HEAD~2..HEAD", "--", "f.txt"],
    );
    assert_eq!(code, 0, "reverse range regressed: {stderr}");
    assert!(
        stdout.contains(") one"),
        "reverse range lost its output: {stdout:?}"
    );
}
