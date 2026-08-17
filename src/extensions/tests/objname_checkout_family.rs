//! `git checkout`, `git switch` and `git restore` when the object name is
//! well-formed but the repository does not have the object.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id, decoded without
//! the object database ever being asked whether that object exists. The whole
//! checkout family then funnels through `parse_branchname_arg()`:
//!
//! ```c
//! new_branch_info->commit = lookup_commit_reference_gently(the_repository, rev, 1);
//! if (!new_branch_info->commit) {
//!         *source_tree = parse_tree_indirect(rev);
//!         if (!*source_tree)
//!                 die(_("unable to read tree (%s)"), oid_to_hex(rev));
//! }
//! ```
//!
//! which is the *only* place a missing object is noticed. That gives every verb
//! here three outcomes that look alike and are reported differently:
//!
//! | name resolves to           | git says                                  |
//! |----------------------------|-------------------------------------------|
//! | nothing                    | the verb's own "not a reference" wording  |
//! | an id the repo lacks       | `unable to read tree (<hex>)`, exit 128   |
//! | an object that is no tree  | the same `unable to read tree (<hex>)`    |
//! | a tree                     | `Cannot switch branch to a non-commit`    |
//!
//! Resolving through `rev_parse_single()` alone collapses the first two, which
//! is what these tests pin. The worst instance was `git checkout <full-hex>`:
//! the unresolved name fell through to the pathspec branch, so an ordinary
//! `git checkout <sha-copied-from-an-email>` in a repository lacking that object
//! reported `pathspec … did not match any file(s)` and exited 1 instead of 128.
//!
//! Every expectation below was captured from stock git 2.55.0 in a hermetic
//! repository (fixed identity and date, `GIT_CONFIG_GLOBAL` and
//! `GIT_CONFIG_SYSTEM` at `/dev/null`, `LC_ALL=C`, `TZ=UTC`) and is written as a
//! literal rather than compared against a stock binary at run time, so the file
//! is meaningful on a headless CI box that has no `git` installed.
//!
//! Each absent-object case is paired with a CONTROL name that is not
//! full-length hex. Without the pair, an implementation that simply widened one
//! error message to cover both would pass.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Well-formed SHA-1 hex that no repository here contains.
const ABSENT: &str = "0123456789012345678901234567890123456789";
/// The same shape spelled in upper case. `get_oid_hex()` folds case, and the
/// diagnostic prints `oid_to_hex(rev)` — the *decoded* id — so stock echoes it
/// back in lower case.
const ABSENT_UPPER: &str = "0123456789ABCDEF012345678901234567890123";
const ABSENT_UPPER_HEX: &str = "0123456789abcdef012345678901234567890123";
/// CONTROL: resolves nowhere at all, so it must keep taking each verb's
/// ordinary "not a reference" path.
const CONTROL: &str = "nosuchthing";
/// One digit short of `hexsz`: falls through to the ordinary parser, so it must
/// behave like [`CONTROL`] and not like [`ABSENT`].
const SHORT_39: &str = "012345678901234567890123456789012345678";
/// One digit past `hexsz`: the same check from the other side.
const LONG_41: &str = "01234567890123456789012345678901234567890";

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
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

/// A repository with one commit on `main`, an annotated tag `atag`, and a
/// tracked `f.txt`, under an isolated `HOME`.
///
/// Every test builds its own: most of these verbs mutate refs or `HEAD`, and a
/// shared fixture would let one case's created branch decide another's answer.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-ckf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "f.txt"],
        vec!["commit", "-q", "-m", "one"],
        vec!["tag", "-a", "-m", "t", "atag"],
    ] {
        if args[0] == "add" {
            std::fs::write(repo.join("f.txt"), b"hello\n").unwrap();
        }
        let out = run_in(&repo, &home, &args);
        assert_eq!(out.code, 0, "fixture setup `git {args:?}` failed: {}", out.stderr);
    }
    (repo, home)
}

/// One invocation against stock's captured stdout, stderr and exit status.
///
/// stderr is checked first: when an expectation moves it is nearly always the
/// message, and seeing it beats seeing `128 != 1`.
fn expect(repo: &Path, home: &Path, args: &[&str], code: i32, stdout: &str, stderr: &str) {
    let got = run_in(repo, home, args);
    assert_eq!(got.stderr, stderr, "git {args:?}: stderr");
    assert_eq!(got.stdout, stdout, "git {args:?}: stdout");
    assert_eq!(got.code, code, "git {args:?}: exit status");
}

/// `git rev-parse <spec>` in the fixture, for the ids that depend on the
/// hash algorithm rather than being literals.
fn oid(repo: &Path, home: &Path, spec: &str) -> String {
    let out = run_in(repo, home, &["rev-parse", spec]);
    assert_eq!(out.code, 0, "rev-parse {spec}: {}", out.stderr);
    out.stdout.trim().to_string()
}

/// Every ref in the repository as `<name>=<oid>` lines, for the cases whose
/// point is that a failed command created nothing.
fn refs(repo: &Path, home: &Path) -> String {
    run_in(repo, home, &["for-each-ref", "--format=%(refname)=%(objectname)"]).stdout
}

fn unable_to_read_tree(hex: &str) -> String {
    format!("fatal: unable to read tree ({hex})\n")
}

fn pathspec_error(spec: &str) -> String {
    format!("error: pathspec '{spec}' did not match any file(s) known to git\n")
}

// ---------------------------------------------------------------------------
// The six argv shapes the sweep flagged
// ---------------------------------------------------------------------------

/// `git checkout <name>`: a full-length hex name is the object id, so an absent
/// one is a missing *object* (exit 128) and never degrades into a pathspec.
///
/// The three controls are what make this test about the length rule: 39 hex
/// digits, 41 hex digits, and a non-hex word all keep taking the pathspec path
/// that `ABSENT` must not take.
#[test]
fn checkout_bare_name_full_hex_is_the_object_id() {
    let (repo, home) = fixture("ck-bare");
    expect(&repo, &home, &["checkout", ABSENT], 128, "", &unable_to_read_tree(ABSENT));
    expect(
        &repo,
        &home,
        &["checkout", ABSENT_UPPER],
        128,
        "",
        // `oid_to_hex()` of the decoded id, so the case the user typed is gone.
        &unable_to_read_tree(ABSENT_UPPER_HEX),
    );
    for control in [CONTROL, SHORT_39, LONG_41] {
        expect(&repo, &home, &["checkout", control], 1, "", &pathspec_error(control));
    }
    // Nothing above may have moved HEAD off the branch.
    assert_eq!(run_in(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout, "main\n");
}

/// `git checkout <name> -- <path>`: with `--` present the name is unambiguously
/// a reference, so an unresolvable one is `invalid reference` rather than a
/// pathspec — and an absent id is still the missing-object message.
///
/// This shape also carried a second, independent defect: the revision parser's
/// error was propagated verbatim, printing vendored `src/ported/…` source paths
/// and a Rust type name at the user, and exiting 1 where git exits 128. The
/// CONTROL case is what covers that, since it reproduced without a hex name.
#[test]
fn checkout_dashdash_reports_reference_errors_not_parser_internals() {
    let (repo, home) = fixture("ck-dashdash");
    expect(&repo, &home, &["checkout", ABSENT, "--", "f.txt"], 128, "", &unable_to_read_tree(ABSENT));
    expect(
        &repo,
        &home,
        &["checkout", ABSENT_UPPER, "--", "f.txt"],
        128,
        "",
        &unable_to_read_tree(ABSENT_UPPER_HEX),
    );
    for control in [CONTROL, SHORT_39, LONG_41] {
        expect(
            &repo,
            &home,
            &["checkout", control, "--", "f.txt"],
            128,
            "",
            &format!("fatal: invalid reference: {control}\n"),
        );
    }
}

/// `git checkout <name> <path>`: without `--`, a name that resolves makes the
/// rest pathspecs, and a name that does not makes *everything* a pathspec. The
/// full-hex rule decides which, so an absent id takes the tree-ish branch.
#[test]
fn checkout_treeish_then_path_uses_the_full_hex_rule_to_split_argv() {
    let (repo, home) = fixture("ck-path");
    expect(&repo, &home, &["checkout", ABSENT, "f.txt"], 128, "", &unable_to_read_tree(ABSENT));
    for control in [CONTROL, SHORT_39] {
        expect(&repo, &home, &["checkout", control, "f.txt"], 1, "", &pathspec_error(control));
    }
}

/// `git switch --detach <name>`: `switch` shares `parse_branchname_arg()`, so an
/// absent id is the missing-object message and only an unresolvable name is
/// `invalid reference`.
#[test]
fn switch_detach_separates_absent_object_from_unresolvable_name() {
    let (repo, home) = fixture("sw-detach");
    let before = refs(&repo, &home);
    expect(&repo, &home, &["switch", "--detach", ABSENT], 128, "", &unable_to_read_tree(ABSENT));
    for control in [CONTROL, SHORT_39] {
        expect(
            &repo,
            &home,
            &["switch", "--detach", control],
            128,
            "",
            &format!("fatal: invalid reference: {control}\n"),
        );
    }
    assert_eq!(refs(&repo, &home), before, "a refused detach must not touch any ref");
    assert_eq!(run_in(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout, "main\n");
}

/// `git switch -c <new> <start>`: the start-point is resolved before the branch
/// is created, so neither failure may leave `refs/heads/nb` behind.
#[test]
fn switch_create_rejects_absent_start_point_without_creating_the_branch() {
    let (repo, home) = fixture("sw-create");
    let before = refs(&repo, &home);
    expect(&repo, &home, &["switch", "-c", "nb", ABSENT], 128, "", &unable_to_read_tree(ABSENT));
    for control in [CONTROL, SHORT_39] {
        expect(
            &repo,
            &home,
            &["switch", "-c", "nb", control],
            128,
            "",
            &format!("fatal: invalid reference: {control}\n"),
        );
    }
    assert_eq!(refs(&repo, &home), before, "a refused start-point must create no branch");
}

/// `git restore --source=<name>`: the same split, with `restore`'s own wording
/// for a name that resolves nowhere — quoted, unlike the checkout family's.
#[test]
fn restore_source_separates_absent_object_from_unresolvable_name() {
    let (repo, home) = fixture("rs-source");
    for staged in [&[][..], &["--staged"][..]] {
        let mut args = vec!["restore"];
        args.extend_from_slice(staged);
        let src = format!("--source={ABSENT}");
        args.push(&src);
        args.push("f.txt");
        expect(&repo, &home, &args, 128, "", &unable_to_read_tree(ABSENT));
    }
    for control in [CONTROL, SHORT_39] {
        expect(
            &repo,
            &home,
            &["restore", &format!("--source={control}"), "f.txt"],
            128,
            "",
            &format!("fatal: could not resolve '{control}'\n"),
        );
    }
}

// ---------------------------------------------------------------------------
// The rest of the family, which resolves argv object names the same way
// ---------------------------------------------------------------------------

/// `-b`/`-B`, `--orphan` and `-p` reach the same classifier. Each keeps its own
/// wording for a name that resolves nowhere, and all four report an absent id
/// as the missing object.
#[test]
fn remaining_checkout_shapes_report_the_absent_object() {
    let (repo, home) = fixture("ck-rest");
    let before = refs(&repo, &home);
    for args in [
        vec!["checkout", "-b", "nb", ABSENT],
        vec!["checkout", "-B", "nb", ABSENT],
        vec!["checkout", "--orphan", "o1", ABSENT],
        vec!["checkout", "-p", ABSENT, "--", "f.txt"],
    ] {
        expect(&repo, &home, &args, 128, "", &unable_to_read_tree(ABSENT));
    }
    // CONTROL: branch creation blames the start-point by name, `-p` calls it a
    // reference. Neither is the missing-object message.
    expect(
        &repo,
        &home,
        &["checkout", "-b", "nb", CONTROL],
        128,
        "",
        &format!("fatal: '{CONTROL}' is not a commit and a branch 'nb' cannot be created from it\n"),
    );
    expect(
        &repo,
        &home,
        &["checkout", "--orphan", "o1", CONTROL],
        128,
        "",
        &format!("fatal: '{CONTROL}' is not a commit and a branch 'o1' cannot be created from it\n"),
    );
    expect(
        &repo,
        &home,
        &["checkout", "-p", CONTROL, "--", "f.txt"],
        128,
        "",
        &format!("fatal: invalid reference: {CONTROL}\n"),
    );
    assert_eq!(refs(&repo, &home), before, "no refused start-point may create a ref");
}

/// `git switch <name>` (no `-c`, no `--detach`) and `git switch --orphan`.
///
/// `switch <name>` is the sharpest of the pair: for a *present* commit id git
/// reports "a branch is expected" and offers `--detach`, so an absent id
/// reaching that same message would claim the repository has an object it does
/// not have.
#[test]
fn switch_plain_name_and_orphan_report_the_absent_object() {
    let (repo, home) = fixture("sw-rest");
    expect(&repo, &home, &["switch", ABSENT], 128, "", &unable_to_read_tree(ABSENT));
    expect(&repo, &home, &["switch", "--orphan", "o1", ABSENT], 128, "", &unable_to_read_tree(ABSENT));
    expect(
        &repo,
        &home,
        &["switch", CONTROL],
        128,
        "",
        &format!("fatal: invalid reference: {CONTROL}\n"),
    );
    expect(
        &repo,
        &home,
        &["switch", "--orphan", "o1", CONTROL],
        128,
        "",
        &format!("fatal: invalid reference: {CONTROL}\n"),
    );
    // A resolvable start-point is `--orphan`'s own refusal, not a missing object.
    expect(
        &repo,
        &home,
        &["switch", "--orphan", "o1", "HEAD"],
        128,
        "",
        "fatal: '--orphan' cannot take <start-point>\n",
    );
    // A present commit id keeps the "expected a branch" answer plus its hint,
    // which is exactly what an absent id must not be able to reach.
    let head = oid(&repo, &home, "HEAD");
    expect(
        &repo,
        &home,
        &["switch", &head],
        128,
        "",
        &format!(
            "fatal: a branch is expected, got commit '{head}'\n\
             hint: If you want to detach HEAD at the commit, try again with the --detach option.\n"
        ),
    );
}

// ---------------------------------------------------------------------------
// Present ids that are not commits — the other half of the same classifier
// ---------------------------------------------------------------------------

/// A blob id is present but `parse_tree_indirect()` cannot reach a tree through
/// it, so git prints the *same* missing-tree message it prints for an absent id.
///
/// This is what stops a fix from being "special-case the absent object": the
/// message belongs to the tree lookup, not to object existence.
#[test]
fn blob_id_reports_the_same_unreadable_tree_as_an_absent_id() {
    let (repo, home) = fixture("blob");
    let blob = oid(&repo, &home, "HEAD:f.txt");
    let expected = unable_to_read_tree(&blob);
    let source = format!("--source={blob}");
    for args in [
        vec!["checkout", blob.as_str()],
        vec!["checkout", &blob, "--", "f.txt"],
        vec!["checkout", &blob, "f.txt"],
        vec!["checkout", "-b", "nb", &blob],
        vec!["checkout", "--orphan", "o1", &blob],
        vec!["switch", "--detach", &blob],
        vec!["switch", "-c", "nb", &blob],
        vec!["switch", &blob],
        vec!["switch", "--orphan", "o1", &blob],
        vec!["restore", &source, "f.txt"],
    ] {
        expect(&repo, &home, &args, 128, "", &expected);
    }
}

/// A tree id resolves *and* is readable, so it is a legitimate `source_tree`:
/// the path-restoring shapes succeed with it, and only the branch-switching
/// shapes refuse — by a different message, naming the argument as spelled.
#[test]
fn tree_id_is_a_source_tree_but_not_something_to_switch_to() {
    let (repo, home) = fixture("tree");
    let tree = oid(&repo, &home, "HEAD^{tree}");
    let refusal = format!("fatal: Cannot switch branch to a non-commit '{tree}'\n");
    for args in [
        vec!["checkout", tree.as_str()],
        vec!["checkout", "-b", "nb", &tree],
        vec!["checkout", "--orphan", "o1", &tree],
        vec!["switch", "--detach", &tree],
        vec!["switch", "-c", "nb", &tree],
        vec!["switch", &tree],
    ] {
        expect(&repo, &home, &args, 128, "", &refusal);
    }
    // Restoring paths *from* a tree is fine, and so is `switch --orphan`'s own
    // refusal, which outranks the non-commit one.
    expect(&repo, &home, &["checkout", &tree, "--", "f.txt"], 0, "", "");
    let source = format!("--source={tree}");
    expect(&repo, &home, &["restore", &source, "f.txt"], 0, "", "");
    expect(
        &repo,
        &home,
        &["switch", "--orphan", "o1", &tree],
        128,
        "",
        "fatal: '--orphan' cannot take <start-point>\n",
    );
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// No diagnostic in this family may expose the vendored revision parser.
///
/// A `?` on `rev_parse_single()` renders the whole `gix` error chain, which
/// carries `src/ported/gix-revision/src/spec/parse/function.rs:491` and friends
/// — file paths from inside this repository, printed at a user who ran
/// `git checkout`. Kept as its own test because the leak reproduces for names
/// that have nothing to do with the full-hex rule, so it must not be able to
/// come back through some shape the expectations above happen not to cover.
#[test]
fn no_invocation_leaks_vendored_parser_paths() {
    let (repo, home) = fixture("leak");
    for args in [
        vec!["checkout", ABSENT],
        vec!["checkout", CONTROL],
        vec!["checkout", ABSENT, "--", "f.txt"],
        vec!["checkout", CONTROL, "--", "f.txt"],
        vec!["checkout", SHORT_39, "--", "f.txt"],
        vec!["checkout", ABSENT, "f.txt"],
        vec!["checkout", "-b", "nb", CONTROL],
        vec!["checkout", "-p", CONTROL, "--", "f.txt"],
        vec!["switch", "--detach", ABSENT],
        vec!["switch", "-c", "nb", CONTROL],
        vec!["switch", CONTROL],
        vec!["restore", "--source=nosuchthing", "f.txt"],
    ] {
        let got = run_in(&repo, &home, &args);
        for needle in ["src/ported", "couldn't parse revision", "zvcs: "] {
            assert!(
                !got.stderr.contains(needle),
                "git {args:?} leaked {needle:?}:\n{}",
                got.stderr
            );
        }
    }
}

/// The names that must keep working, so none of the above is a fix that just
/// makes the family refuse more.
#[test]
fn resolvable_names_still_check_out() {
    let (repo, home) = fixture("ok");
    let head = oid(&repo, &home, "HEAD");

    // A tag is peeled to its commit, both as a detach target and as the
    // start-point a new branch is created at — an annotated tag's own id must
    // never become a branch tip.
    assert_eq!(run_in(&repo, &home, &["switch", "-c", "nb", "atag"]).code, 0);
    assert_eq!(oid(&repo, &home, "refs/heads/nb"), head);
    assert_ne!(oid(&repo, &home, "refs/tags/atag"), head, "atag must be an annotated tag");

    assert_eq!(run_in(&repo, &home, &["switch", "main"]).code, 0);
    assert_eq!(run_in(&repo, &home, &["switch", "--detach", &head]).code, 0);
    assert_eq!(run_in(&repo, &home, &["rev-parse", "HEAD"]).stdout.trim(), head);

    expect(&repo, &home, &["checkout", "-q", "main"], 0, "", "");
    expect(&repo, &home, &["checkout", &head, "--", "f.txt"], 0, "", "");
    expect(&repo, &home, &["restore", &format!("--source={head}"), "f.txt"], 0, "", "");
    assert_eq!(std::fs::read(repo.join("f.txt")).unwrap(), b"hello\n");
}
