//! `git branch` and `git tag` when the object name is well-formed but absent.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id and the object
//! database is never asked whether the object exists. Every command that takes an
//! object name from argv therefore has two failure modes that look alike and are
//! reported differently: a name that does not resolve, and a name that resolves
//! to an object the repository does not have.
//!
//! Resolving through `rev_parse_single()` alone collapses the two, which is what
//! these tests pin. Each expectation below was captured from stock git 2.55.0 in
//! a hermetic repository (fixed identity and date, `GIT_CONFIG_GLOBAL` and
//! `GIT_CONFIG_SYSTEM` at `/dev/null`, `LC_ALL=C`, `TZ=UTC`); they are written as
//! literals rather than compared against a stock binary at run time so the file
//! is meaningful on a headless CI box that has no `git` installed.
//!
//! The three option families are deliberately separated: git resolves them from
//! three different `parse_options()` callbacks that do not agree on message,
//! quoting, or exit status, and folding them into one resolver is exactly the bug
//! this file guards.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Well-formed SHA-1 hex that no repository here contains.
const ABSENT: &str = "0123456789012345678901234567890123456789";
/// The same shape with letters, to exercise `get_oid_hex()`'s case folding.
const ABSENT_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
/// Control: a name that resolves nowhere at all, so it must keep taking the
/// ordinary "this is not an object name" path in every case below.
const CONTROL: &str = "nosuchthing";
/// One digit short of `hexsz`: must fall through to the ordinary parser and so
/// behave like [`CONTROL`], not like [`ABSENT`].
const SHORT_39: &str = "012345678901234567890123456789012345678";
/// One digit past `hexsz`: same, from the other side.
const LONG_41: &str = "01234567890123456789012345678901234567890";

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// A repository with one commit on `master`, a lightweight tag `v1`, and an
/// annotated tag `treetag` whose target is the commit's *tree*, under an isolated
/// `HOME` so no user config reaches it. Each test gets its own, because several of
/// these commands are mutating and a shared fixture would let one test's created
/// ref change another's output.
///
/// `treetag` is the only shape that tells the operand id apart from the peeled id
/// in `object %s is a %s, not a %s`: `lookup_commit_reference_gently()` (2.55.0)
/// prints `oid_to_hex(oid)` — the *operand* — beside `type_name(type)` — the
/// *peeled* type. Without it, every non-commit case is a bare tree whose two ids
/// coincide and the mix-up is invisible.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-bt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    // `-b master` is pinned because several expectations below print the branch
    // list, and the default initial branch is a configurable.
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec!["add", "f"],
        vec!["commit", "-q", "-m", "one"],
        vec!["tag", "v1"],
        vec!["tag", "-a", "-m", "m", "treetag", "HEAD^{tree}"],
    ] {
        if args[0] == "add" {
            std::fs::write(repo.join("f"), b"hello").unwrap();
        }
        let out = run_in(&repo, &home, &args);
        assert_eq!(out.code, 0, "fixture setup `git {args:?}` failed: {}", out.stderr);
    }
    (repo, home)
}

fn run_in(repo: &Path, home: &Path, args: &[&str]) -> Run {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "zvcs parity")
        .env("GIT_AUTHOR_EMAIL", "parity@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs parity")
        .env("GIT_COMMITTER_EMAIL", "parity@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap(),
    }
}

/// Assert one invocation against stock's captured stdout, stderr and status.
fn expect(repo: &Path, home: &Path, args: &[&str], code: i32, stdout: &str, stderr: &str) {
    let got = run_in(repo, home, args);
    assert_eq!(got.stderr, stderr, "git {args:?}: stderr");
    assert_eq!(got.stdout, stdout, "git {args:?}: stdout");
    assert_eq!(got.code, code, "git {args:?}: exit status");
}

/// An object id from the fixture, read back rather than hard-coded so the
/// fixture's content can change without turning these into false failures.
fn rev_parse(repo: &Path, home: &Path, spec: &str) -> String {
    let out = run_in(repo, home, &["rev-parse", spec]);
    assert_eq!(out.code, 0, "rev-parse {spec}: {}", out.stderr);
    out.stdout.trim().to_owned()
}

/// The tree of the fixture's only commit, used for the "resolves, but is not a
/// commit" cases.
fn tree_oid(repo: &Path, home: &Path) -> String {
    rev_parse(repo, home, "v1^{tree}")
}

/// `dwim_branch_start()` separates "did not resolve" from "resolved to nothing
/// the odb has": `repo_get_oid_mb()` failing is `not a valid object name`, but a
/// name that resolved and then failed `lookup_commit_reference()` is `not a valid
/// branch point`. An absent full-length hex reaches the second, and the branch is
/// not created either way.
#[test]
fn branch_start_point_absent_full_hex_is_not_a_valid_branch_point() {
    let (repo, home) = fixture("bsp");
    expect(
        &repo,
        &home,
        &["branch", "newb", ABSENT],
        128,
        "",
        &format!("fatal: not a valid branch point: '{ABSENT}'\n"),
    );
    // `get_oid_hex()` is case-insensitive, so uppercase takes the same branch and
    // the message echoes the argument as typed.
    expect(
        &repo,
        &home,
        &["branch", "newu", ABSENT_UPPER],
        128,
        "",
        &format!("fatal: not a valid branch point: '{ABSENT_UPPER}'\n"),
    );
    // CONTROL and both off-by-one lengths never enter the full-hex branch.
    for name in [CONTROL, SHORT_39, LONG_41] {
        expect(
            &repo,
            &home,
            &["branch", "newc", name],
            128,
            "",
            &format!("fatal: not a valid object name: '{name}'\n"),
        );
    }
    // Nothing above may have created a ref.
    let listed = run_in(&repo, &home, &["branch"]);
    assert_eq!(listed.stdout, "* master\n", "no branch may have been created");
}

/// The other half of `lookup_commit_reference()`: a *present* object of the wrong
/// type makes it print its own line before the caller dies, so the non-commit case
/// is two lines and not one.
///
/// The second case pins whose id that line names.
/// `lookup_commit_reference_gently()` peels first and then reports
/// `oid_to_hex(oid)` — the operand — with `type_name(type)` — the peeled type. A
/// tag on a tree is therefore "object <tag id> is a tree", and printing the peeled
/// id there would look right in every test that uses a bare tree.
#[test]
fn branch_start_point_non_commit_reports_object_as_type_first() {
    let (repo, home) = fixture("bsp-tree");
    let tree = tree_oid(&repo, &home);
    expect(
        &repo,
        &home,
        &["branch", "newd", "v1^{tree}"],
        128,
        "",
        &format!(
            "error: object {tree} is a tree, not a commit\n\
             fatal: not a valid branch point: 'v1^{{tree}}'\n"
        ),
    );
    let tag = rev_parse(&repo, &home, "treetag");
    assert_ne!(tag, tree, "fixture must give the tag and its target distinct ids");
    expect(
        &repo,
        &home,
        &["branch", "newe", "treetag"],
        128,
        "",
        &format!(
            "error: object {tag} is a tree, not a commit\n\
             fatal: not a valid branch point: 'treetag'\n"
        ),
    );
}

/// `OPT_CONTAINS`/`OPT_NO_CONTAINS`/`OPT_WITH`/`OPT_WITHOUT` are
/// `parse_opt_commits()`: an unresolvable name is `malformed object name` and a
/// resolvable-but-missing one is `no such commit`, both unquoted, both exit 129
/// (a callback returning -1 is `PARSE_OPT_ERROR`, not a `die()`).
#[test]
fn contains_family_separates_malformed_from_no_such_commit() {
    let (repo, home) = fixture("contains");
    let tree = tree_oid(&repo, &home);
    for flag in ["--contains", "--no-contains", "--with", "--without"] {
        expect(
            &repo,
            &home,
            &["branch", flag, ABSENT],
            129,
            "",
            &format!("error: no such commit {ABSENT}\n"),
        );
        expect(
            &repo,
            &home,
            &["branch", flag, CONTROL],
            129,
            "",
            &format!("error: malformed object name {CONTROL}\n"),
        );
        // Off-by-one lengths are ordinary names, so they are `malformed`, not
        // `no such commit` — the boundary the `full_hex` length test enforces.
        expect(
            &repo,
            &home,
            &["branch", flag, SHORT_39],
            129,
            "",
            &format!("error: malformed object name {SHORT_39}\n"),
        );
        expect(
            &repo,
            &home,
            &["branch", flag, "v1^{tree}"],
            129,
            "",
            &format!(
                "error: object {tree} is a tree, not a commit\n\
                 error: no such commit v1^{{tree}}\n"
            ),
        );
    }
    // The operand id, not the peeled one, names the object in the type line.
    let tag = rev_parse(&repo, &home, "treetag");
    expect(
        &repo,
        &home,
        &["branch", "--contains", "treetag"],
        129,
        "",
        &format!(
            "error: object {tag} is a tree, not a commit\n\
             error: no such commit treetag\n"
        ),
    );
    // Same shapes through `git tag`, which reaches the same callback.
    expect(
        &repo,
        &home,
        &["tag", "--contains", ABSENT],
        129,
        "",
        &format!("error: no such commit {ABSENT}\n"),
    );
    expect(
        &repo,
        &home,
        &["tag", "--no-contains", CONTROL],
        129,
        "",
        &format!("error: malformed object name {CONTROL}\n"),
    );
}

/// `filter_ref()` drops a ref that does not peel to a commit as soon as any
/// reachability filter is in play, and does it for the whole family at once — so
/// `--no-contains`/`--no-merged`, which read as "keep what does not match", still
/// omit a tag on a tree. `--points-at` is deliberately outside that gate and keeps
/// such a tag, which is what makes this a filter-shape test rather than a
/// listing-order one.
#[test]
fn reachability_filters_drop_refs_that_do_not_peel_to_a_commit() {
    let (repo, home) = fixture("reach-drop");
    expect(&repo, &home, &["tag", "-l"], 0, "treetag\nv1\n", "");
    for flag in ["--contains", "--no-contains", "--merged", "--no-merged"] {
        let out = run_in(&repo, &home, &["tag", flag, "v1"]);
        assert_eq!(out.code, 0, "git tag {flag} v1: exit");
        assert_eq!(out.stderr, "", "git tag {flag} v1: stderr");
        assert!(
            !out.stdout.contains("treetag"),
            "git tag {flag} v1 listed a tag that does not peel to a commit: {:?}",
            out.stdout
        );
    }
    // `--points-at` keeps it, so the exclusion above is the reachability gate and
    // not the tag being unlistable.
    expect(&repo, &home, &["tag", "-l", "--points-at", "v1^{tree}"], 0, "treetag\n", "");
}

/// `OPT_MERGED`/`OPT_NO_MERGED` are `parse_opt_merge_filter()`, which is the one
/// callback in this family that `die()`s: an unresolvable name is `fatal:` at
/// **128**, while a name that resolves to something that is not a commit is
/// `error:` at **129**. Collapsing the two statuses is a pre-existing bug
/// independent of the full-hex rule, and this test pins both halves.
#[test]
fn merged_family_dies_at_128_but_errors_at_129() {
    let (repo, home) = fixture("merged");
    let tree = tree_oid(&repo, &home);
    for (flag, long) in [("--merged", "merged"), ("--no-merged", "no-merged")] {
        // Resolves (full hex), object absent: `error:` + 129.
        expect(
            &repo,
            &home,
            &["branch", flag, ABSENT],
            129,
            "",
            &format!("error: option `{long}' must point to a commit\n"),
        );
        // Does not resolve: `die()` + 128.
        expect(
            &repo,
            &home,
            &["branch", flag, CONTROL],
            128,
            "",
            &format!("fatal: malformed object name {CONTROL}\n"),
        );
        // Resolves to a present non-commit: the type line, then `error:` + 129.
        expect(
            &repo,
            &home,
            &["branch", flag, "v1^{tree}"],
            129,
            "",
            &format!(
                "error: object {tree} is a tree, not a commit\n\
                 error: option `{long}' must point to a commit\n"
            ),
        );
        // The type line names the operand, not the peeled object.
        let tag = rev_parse(&repo, &home, "treetag");
        expect(
            &repo,
            &home,
            &["branch", flag, "treetag"],
            129,
            "",
            &format!(
                "error: object {tag} is a tree, not a commit\n\
                 error: option `{long}' must point to a commit\n"
            ),
        );
        // `git tag` shares the callback, so it shares the 128/129 split.
        expect(
            &repo,
            &home,
            &["tag", flag, ABSENT],
            129,
            "",
            &format!("error: option `{long}' must point to a commit\n"),
        );
        expect(
            &repo,
            &home,
            &["tag", flag, CONTROL],
            128,
            "",
            &format!("fatal: malformed object name {CONTROL}\n"),
        );
    }
}

/// `--points-at` is `parse_opt_object_name()`, which appends the id and never
/// consults the object database. An absent full-length hex is therefore a valid
/// filter that matches nothing: **exit 0, no output**. Turning it into an error is
/// the shape most likely to break a caller's script, so it is asserted for both
/// commands, and against the quoted `'%s'` message this callback alone uses.
#[test]
fn points_at_absent_object_matches_nothing_and_exits_zero() {
    let (repo, home) = fixture("points-at");
    for argv in [
        vec!["branch", "--points-at", ABSENT],
        vec!["branch", "--points-at", ABSENT_UPPER],
        vec!["tag", "-l", "--points-at", ABSENT],
        vec!["tag", "-l", "--points-at", ABSENT_UPPER],
    ] {
        expect(&repo, &home, &argv, 0, "", "");
    }
    // A name that does not resolve is still refused — and this callback is the
    // only one in the family that quotes the operand.
    for name in [CONTROL, SHORT_39, LONG_41] {
        expect(
            &repo,
            &home,
            &["branch", "--points-at", name],
            129,
            "",
            &format!("error: malformed object name '{name}'\n"),
        );
        expect(
            &repo,
            &home,
            &["tag", "-l", "--points-at", name],
            129,
            "",
            &format!("error: malformed object name '{name}'\n"),
        );
    }
    // A filter that does match still lists, so "matches nothing" is a real filter
    // result rather than the listing being disabled.
    expect(&repo, &home, &["tag", "-l", "--points-at", "v1"], 0, "v1\n", "");
}

/// `cmd_tag()` only dies with `Failed to resolve` when `repo_get_oid()` itself
/// fails. A full-length hex resolves, so a lightweight tag gets all the way to
/// `ref_transaction_update()`, which refuses the write with its own message —
/// naming the full ref and the *lowercased* id, since it prints `oid_to_hex()`
/// rather than the argument.
#[test]
fn tag_lightweight_on_absent_object_is_refused_by_the_ref_update() {
    let (repo, home) = fixture("tag-write");
    expect(
        &repo,
        &home,
        &["tag", "newt", ABSENT],
        128,
        "",
        &format!("fatal: trying to write ref 'refs/tags/newt' with nonexistent object {ABSENT}\n"),
    );
    expect(
        &repo,
        &home,
        &["tag", "newu", ABSENT_UPPER],
        128,
        "",
        &format!(
            "fatal: trying to write ref 'refs/tags/newu' with nonexistent object {}\n",
            ABSENT_UPPER.to_ascii_lowercase()
        ),
    );
    for name in [CONTROL, SHORT_39, LONG_41] {
        expect(
            &repo,
            &home,
            &["tag", "newc", name],
            128,
            "",
            &format!("fatal: Failed to resolve '{name}' as a valid ref.\n"),
        );
    }
    // A refused write must leave the ref namespace untouched.
    expect(&repo, &home, &["tag", "-l"], 0, "treetag\nv1\n", "");
}

/// The annotated path fails earlier and differently: `create_tag()` opens with
/// `odb_read_object_info()` and dies with `bad object type.` before any tag object
/// is written, so `-a` on an absent id never reaches the ref update.
#[test]
fn tag_annotated_on_absent_object_is_bad_object_type() {
    let (repo, home) = fixture("tag-annot");
    expect(
        &repo,
        &home,
        &["tag", "-a", "-m", "x", "newa", ABSENT],
        128,
        "",
        "fatal: bad object type.\n",
    );
    expect(
        &repo,
        &home,
        &["tag", "-a", "-m", "x", "newa", CONTROL],
        128,
        "",
        &format!("fatal: Failed to resolve '{CONTROL}' as a valid ref.\n"),
    );
    expect(&repo, &home, &["tag", "-l"], 0, "treetag\nv1\n", "");
}
