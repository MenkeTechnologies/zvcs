//! A well-formed object id that the repository does not have.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id — decoded and
//! returned before the object database is ever asked whether that object exists.
//! Every caller of `repo_get_oid()` therefore sees such a name succeed, and
//! reports the failure later, from whatever actually needs the object's bytes.
//!
//! gitoxide's `rev_parse_single()` resolves through the odb, so it fails for the
//! same name. Four commands used it as their only resolver and collapsed git's
//! two outcomes into one, printing the "no such name" diagnostic for a name that
//! git considers perfectly well resolved:
//!
//! | command                    | git                                                | zvcs before                              |
//! |----------------------------|----------------------------------------------------|------------------------------------------|
//! | `verify-commit <oid>`      | `error: <oid>: unable to read file.`               | `error: commit '<oid>' not found.`       |
//! | `verify-tag <oid>`         | `error: <oid>: cannot verify a non-tag object of type (null).` | `error: tag '<oid>' not found.` |
//! | `name-rev <oid>`           | `Could not get object for <oid>. Skipping.`        | `Could not get sha1 for <oid>. Skipping.`|
//! | `describe --contains <oid>` | `Could not get object for <oid>. Skipping.`       | `Could not get sha1 for <oid>. Skipping.`|
//!
//! `name-rev` (and `describe --contains`, which literally runs `cmd_name_rev`)
//! is the interesting pair: it prints *both* wordings, chosen by exactly this
//! rule — `repo_get_oid()` failing gives "sha1", `parse_object()` returning NULL
//! afterwards gives "object". A test that only pins the absent-id branch would
//! pass just as well against code that printed "object" unconditionally, so each
//! of those two commands is checked against a name that does not resolve at all
//! *and* a name that resolves to an object that is not there.
//!
//! Expectations below are stock git 2.55.0's, captured with the parity harness's
//! environment (fixed identity and date, no global or system config, `LC_ALL=C`,
//! `TZ=UTC`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Well-formed SHA-1 hex, and not an object any fixture here can contain.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// The control: not hex, not a ref, resolves to nothing at all. Every assertion
/// about `ABSENT` is paired with one about this, because the bug being pinned is
/// precisely that the two were treated alike.
const UNRESOLVABLE: &str = "nosuchthing";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit and one annotated tag, so the "resolves and is present" cases
    /// have something real to land on.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-objname-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["config", "user.email", "committer@example.com"]);
        f.run(&["config", "user.name", "C O Mitter"]);
        std::fs::write(f.repo.join("f.txt"), "hello\n").unwrap();
        f.run(&["add", "f.txt"]);
        f.run(&["commit", "-q", "-m", "c1"]);
        f.run(&["tag", "-a", "-m", "tagmsg", "v1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        run_in(&self.repo, args)
    }
}

fn run_in(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
        .env("GIT_COMMITTER_DATE", "@1112911993 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `verify_commit()` reports "not found" only when `repo_get_oid()` fails; a
/// full-length hex name gets past it and dies at `parse_object()` instead
/// (`builtin/verify-commit.c:41-46`).
#[test]
fn verify_commit_separates_unresolvable_from_absent() {
    let f = Fixture::new("vc");

    let (out, err, code) = f.run(&["verify-commit", ABSENT]);
    assert_eq!(err, format!("error: {ABSENT}: unable to read file.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 1);

    let (out, err, code) = f.run(&["verify-commit", UNRESOLVABLE]);
    assert_eq!(err, format!("error: commit '{UNRESOLVABLE}' not found.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 1);
}

/// `gpg_verify_tag()` asks `oid_object_info()` for the type before anything
/// else; a missing object yields no type name, and git's `error()` renders the
/// null pointer as the literal `(null)`. The odd-looking text is git's, and is
/// pinned deliberately.
#[test]
fn verify_tag_separates_unresolvable_from_absent() {
    let f = Fixture::new("vt");

    let (out, err, code) = f.run(&["verify-tag", ABSENT]);
    assert_eq!(
        err,
        format!("error: {ABSENT}: cannot verify a non-tag object of type (null).\n")
    );
    assert_eq!(out, "");
    assert_eq!(code, 1);

    let (out, err, code) = f.run(&["verify-tag", UNRESOLVABLE]);
    assert_eq!(err, format!("error: tag '{UNRESOLVABLE}' not found.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 1);

    // A name that resolves to a present non-tag names its real type, which is
    // what makes `(null)` above a missing object rather than a stock spelling of
    // "wrong type".
    let (_, err, code) = f.run(&["verify-tag", "HEAD"]);
    assert_eq!(err, "error: HEAD: cannot verify a non-tag object of type commit.\n");
    assert_eq!(code, 1);
}

/// Both of name-rev's wordings, from one invocation: `repo_get_oid()` failing
/// gives "sha1", `parse_object()` returning NULL gives "object"
/// (`builtin/name-rev.c:702-721`). Order is asserted too — git processes argv
/// left to right and neither message is buffered.
#[test]
fn name_rev_prints_both_wordings() {
    let f = Fixture::new("nr");

    let (out, err, code) = f.run(&["name-rev", ABSENT, UNRESOLVABLE]);
    assert_eq!(
        err,
        format!(
            "Could not get object for {ABSENT}. Skipping.\n\
             Could not get sha1 for {UNRESOLVABLE}. Skipping.\n"
        )
    );
    // Every argument was skipped, so nothing is named and the exit stays 0.
    assert_eq!(out, "");
    assert_eq!(code, 0);
}

/// `get_oid_hex()` runs on `hexval()`, which is case-insensitive, so an
/// upper-case id takes the same first branch. A resolver that only accepted
/// lower-case hex would silently fall back to the "sha1" wording here.
#[test]
fn name_rev_absent_id_is_case_insensitive() {
    let f = Fixture::new("nrcase");

    let (_, err, code) = f.run(&["name-rev", &ABSENT.to_ascii_uppercase()]);
    assert_eq!(
        err,
        format!(
            "Could not get object for {}. Skipping.\n",
            ABSENT.to_ascii_uppercase()
        )
    );
    assert_eq!(code, 0);
}

/// One hex digit short of `hexsz` is *not* the first branch: it falls through to
/// the ordinary parser, which finds no such abbreviation, so the "sha1" wording
/// is correct there. This is the boundary the length check exists to draw.
#[test]
fn name_rev_39_hex_digits_is_not_an_object_name() {
    let f = Fixture::new("nrshort");

    let short = &ABSENT[..39];
    let (_, err, code) = f.run(&["name-rev", short]);
    assert_eq!(err, format!("Could not get sha1 for {short}. Skipping.\n"));
    assert_eq!(code, 0);
}

/// `describe --contains` is `cmd_name_rev` with `--peel-tag --name-only
/// --no-undefined --tags` (`builtin/describe.c:703-735`), so it inherits both
/// wordings unchanged — and, from `--peel-tag`, a third for a name that resolves
/// to a present object that is not a commit.
#[test]
fn describe_contains_prints_all_three_wordings() {
    let f = Fixture::new("de");

    let (out, err, code) = f.run(&["describe", "--contains", ABSENT, UNRESOLVABLE]);
    assert_eq!(
        err,
        format!(
            "Could not get object for {ABSENT}. Skipping.\n\
             Could not get sha1 for {UNRESOLVABLE}. Skipping.\n"
        )
    );
    assert_eq!(out, "");
    assert_eq!(code, 0);

    let (out, err, code) = f.run(&["describe", "--contains", "HEAD^{tree}"]);
    assert_eq!(err, "Could not get commit for HEAD^{tree}. Skipping.\n");
    assert_eq!(out, "");
    assert_eq!(code, 0);
}

/// The fix must not cost `describe --contains` its actual answer: a real tag
/// still resolves and is still named.
#[test]
fn describe_contains_still_names_a_real_tag() {
    let f = Fixture::new("deok");

    let (out, err, code) = f.run(&["describe", "--contains", "v1"]);
    assert_eq!(err, "");
    assert_eq!(out, "v1^0\n");
    assert_eq!(code, 0);
}

/// Plain `describe` (no `--contains`) reads the same name through the same
/// `repo_get_oid`, so it splits the same way: an absent id is a perfectly valid
/// object *name*, and dies only once `lookup_commit_reference_gently()` and
/// `odb_read_object_info()` have both come up empty
/// (`builtin/describe.c:608-618`).
#[test]
fn describe_absent_id_is_a_valid_object_name() {
    let f = Fixture::new("deplain");

    let (_, err, code) = f.run(&["describe", ABSENT]);
    assert_eq!(err, format!("fatal: {ABSENT} is neither a commit nor blob\n"));
    assert_eq!(code, 128);

    let (_, err, code) = f.run(&["describe", UNRESOLVABLE]);
    assert_eq!(err, format!("fatal: Not a valid object name {UNRESOLVABLE}\n"));
    assert_eq!(code, 128);

    // The blob route still runs, so the "neither a commit nor blob" message
    // above is a genuinely absent object rather than a lost branch.
    let (out, err, code) = f.run(&["describe", "HEAD:f.txt"]);
    assert_eq!(err, "");
    assert_eq!(out, "v1:f.txt\n");
    assert_eq!(code, 0);
}
