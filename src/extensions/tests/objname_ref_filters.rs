//! `get_oid_basic()`'s full-length-hex rule where `for-each-ref` and
//! `show-branch` take an object name from argv.
//!
//! `object-name.c`'s first branch decodes a name of exactly `hexsz` hex digits
//! and returns **without consulting the object database**:
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) { … return 0; }
//! ```
//!
//! So "well-formed but absent" is not "not a valid object name", and the two
//! must not collapse into one message. In these three commands the difference is
//! not cosmetic — it decides whether the process succeeds:
//!
//!   * `--points-at=<absent-id>` is a filter that matches nothing. git exits 0
//!     with no output; a script polling `--points-at=$sha` against a repository
//!     that has since gc'd the object keeps working.
//!   * `--contains=<absent-id>` resolves and *then* fails to find a commit, so
//!     the message is `no such commit`, not `malformed object name`.
//!   * `show-branch <absent-id>` drops the rev quietly, because `append_ref()`
//!     peels with `quiet = 1`; the command still exits 0.
//!
//! The `lookup_commit_reference()` peel these operands go through is also run by
//! `for-each-ref` over **every ref in the array** whenever an
//! `%(ahead-behind:…)` atom is in play (`filter_ahead_behind()`,
//! ref-filter.c:3213), and there too `quiet` is 0. So the last group of tests
//! pins the pass rather than an operand: which refs it reports, that it runs
//! before `--count` truncates and before the sort, that a `--sort` key alone is
//! enough to trigger it, and that it reports nothing when no such atom is used.
//!
//! Every expectation below was captured from stock git 2.55.0 in an identical
//! throwaway repository, comparing stdout, stderr and exit status separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Forty hex digits that decode cleanly and name nothing in the repository.
const MISSING: &str = "0123456789012345678901234567890123456789";
/// The same rule with letters in it, so a lowercase-only decoder is not enough.
const MISSING_HEX: &str = "abcdef0123456789abcdef0123456789abcdef01";
/// `get_oid_hex()` is case-insensitive (`hexval()` accepts both), so this is the
/// same absent id as [`MISSING_HEX`].
const MISSING_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
/// The control: a name that does not resolve at all, and so is still an error
/// everywhere. Every test that asserts an absent id is *accepted* asserts this
/// one is *rejected* alongside it — otherwise "fixed" would be indistinguishable
/// from "stopped validating".
const CONTROL: &str = "nosuchthing";
/// Thirty-nine hex digits: one short of the rule, so it falls through to the
/// ordinary parser and is rejected like any other unknown name. Guards against a
/// fix that keys off "looks hexish" rather than the exact length.
const SHORT_39: &str = "012345678901234567890123456789012345678";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two commits on `main`, an idle `side`, a lightweight tag on the tip, and
    /// an annotated tag whose target is a *tree* — the last one exists so the
    /// "resolved, but not a commit" path has something to report.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-objname-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("a"), b"one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a"), b"two\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "two"]);
        f.git(&["branch", "side"]);
        f.git(&["tag", "v1"]);
        let tree = f.rev_parse("HEAD^{tree}");
        f.git(&["tag", "-a", "-m", "treetag", "treetag", &tree]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn rev_parse(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "`rev-parse {spec}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// stdout, stderr and exit status, kept apart: two of these sites differ from
    /// stock only in which stream carried the text, or only in the status.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

// ---------------------------------------------------------------------------
// `for-each-ref --points-at` (`parse_opt_object_name`)
// ---------------------------------------------------------------------------

/// The regression that turns a working script into a broken one: the id is
/// well-formed, so it goes straight into the filter's oid array and matches
/// nothing. Nothing is printed and the status is 0 — not 129.
#[test]
fn points_at_an_absent_id_matches_nothing_at_exit_zero() {
    let f = Fixture::new("points-at-absent");
    for id in [MISSING, MISSING_HEX, MISSING_UPPER] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--points-at={id}")]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "", 0),
            "--points-at={id} must be a filter that matches nothing"
        );
    }
    // Same through the separated-argument spelling, which is a different branch
    // of the option parser.
    let (out, err, code) = f.run(&["for-each-ref", "--points-at", MISSING]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
}

/// The control: a name that does not resolve is still rejected, with git's
/// quoted operand and `PARSE_OPT_ERROR`'s bare 129 — no usage block.
#[test]
fn points_at_an_unresolvable_name_is_still_an_error() {
    let f = Fixture::new("points-at-control");
    for name in [CONTROL, SHORT_39] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--points-at={name}")]);
        assert_eq!(out, "", "nothing goes to stdout for {name}");
        assert_eq!(err, format!("error: malformed object name '{name}'\n"));
        assert_eq!(code, 129, "PARSE_OPT_ERROR, and no usage block above");
    }
}

/// The rule is `get_oid_hex()`, which accepts either case, so an uppercase id of
/// an object the repository *does* have still selects its refs.
#[test]
fn points_at_accepts_an_uppercase_present_id() {
    let f = Fixture::new("points-at-upper");
    let head = f.rev_parse("HEAD");
    let (lower, _, _) = f.run(&["for-each-ref", &format!("--points-at={head}")]);
    let (upper, err, code) = f.run(&["for-each-ref", &format!("--points-at={}", head.to_uppercase())]);
    assert!(lower.contains("refs/heads/main"), "the lowercase form selects the tip");
    assert_eq!((upper.as_str(), err.as_str(), code), (lower.as_str(), "", 0));
}

// ---------------------------------------------------------------------------
// `for-each-ref --contains` / `--no-contains` (`parse_opt_commits`)
// ---------------------------------------------------------------------------

/// `parse_opt_commits()` fails in two places and says something different at
/// each: `repo_get_oid()` gives `malformed object name`, and only a name that
/// already resolved reaches `no such commit`. An absent full-length id takes the
/// second path, which is the distinction the odb-only resolver erased.
#[test]
fn contains_separates_unresolvable_from_absent() {
    let f = Fixture::new("contains");
    for opt in ["--contains", "--no-contains"] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("{opt}={MISSING}")]);
        assert_eq!(out, "");
        assert_eq!(err, format!("error: no such commit {MISSING}\n"));
        assert_eq!(code, 129);

        let (out, err, code) = f.run(&["for-each-ref", &format!("{opt}={CONTROL}")]);
        assert_eq!(out, "");
        assert_eq!(
            err,
            format!("error: malformed object name {CONTROL}\n"),
            "the control keeps the other message — and note it is unquoted here"
        );
        assert_eq!(code, 129);
    }
}

/// A name that resolves to a *present* non-commit is a third case again:
/// `lookup_commit_reference_gently()` is called non-quiet, so it prints its own
/// line first. git names the **operand** there and the **peeled** type, which is
/// why a tag pointing at a tree reports the tag's id as being "a tree".
#[test]
fn contains_a_non_commit_reports_the_operand_then_no_such_commit() {
    let f = Fixture::new("contains-tree");
    let tree = f.rev_parse("HEAD^{tree}");
    let (_, err, code) = f.run(&["for-each-ref", &format!("--contains={tree}")]);
    assert_eq!(
        err,
        format!("error: object {tree} is a tree, not a commit\nerror: no such commit {tree}\n")
    );
    assert_eq!(code, 129);

    let tag = f.rev_parse("treetag");
    assert_ne!(tag, tree, "the annotated tag is its own object");
    let (_, err, code) = f.run(&["for-each-ref", "--contains=treetag"]);
    assert_eq!(
        err,
        format!("error: object {tag} is a tree, not a commit\nerror: no such commit treetag\n"),
        "the id is the unpeeled operand, the type is the peeled object's"
    );
    assert_eq!(code, 129);
}

/// The positive control for the whole filter: a real commit still selects refs.
#[test]
fn contains_a_real_commit_still_filters() {
    let f = Fixture::new("contains-real");
    let head = f.rev_parse("HEAD");
    let (out, err, code) = f.run(&["for-each-ref", &format!("--contains={head}")]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(out.contains("refs/heads/main"), "got: {out:?}");
    assert!(out.contains("refs/heads/side"), "got: {out:?}");
}

// ---------------------------------------------------------------------------
// `for-each-ref --merged` / `--no-merged` (`parse_opt_merge_filter`)
// ---------------------------------------------------------------------------

/// The merge filters split the same two failures by *severity*, not just by
/// wording: `repo_get_oid()` failing is a `die()` at 128, while a name that
/// resolved but is not a commit is an `error()` at 129 that names the option
/// rather than the operand.
#[test]
fn merged_splits_die_from_option_error() {
    let f = Fixture::new("merged");
    for opt in ["merged", "no-merged"] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--{opt}={MISSING}")]);
        assert_eq!(out, "");
        assert_eq!(err, format!("error: option `{opt}' must point to a commit\n"));
        assert_eq!(code, 129, "the second failure is error(), not die()");

        let (out, err, code) = f.run(&["for-each-ref", &format!("--{opt}={CONTROL}")]);
        assert_eq!(out, "");
        assert_eq!(err, format!("fatal: malformed object name {CONTROL}\n"));
        assert_eq!(code, 128, "the control still dies, which is what makes it 128");
    }
}

/// Non-commit operands reach the same `must point to a commit` line, behind the
/// type complaint the non-quiet peel emits.
#[test]
fn merged_a_non_commit_reports_the_type_first() {
    let f = Fixture::new("merged-tree");
    let tree = f.rev_parse("HEAD^{tree}");
    let (_, err, code) = f.run(&["for-each-ref", &format!("--merged={tree}")]);
    assert_eq!(
        err,
        format!(
            "error: object {tree} is a tree, not a commit\n\
             error: option `merged' must point to a commit\n"
        )
    );
    assert_eq!(code, 129);
}

// ---------------------------------------------------------------------------
// `for-each-ref`'s `%(ahead-behind:…)` per-ref pre-pass
// ---------------------------------------------------------------------------

/// The same non-quiet peel, reached from the *other* side: not the atom's
/// operand but every **ref in the array**.
///
/// `filter_ahead_behind()` (ref-filter.c:3213-3218) runs between `filter_refs()`
/// and the sort:
///
/// ```c
/// for (size_t i = 0; i < array->nr; i++) {
///         const char *name = array->items[i]->refname;
///         commits[commits_nr] = lookup_commit_reference_by_name(name);
///         if (!commits[commits_nr])
///                 continue;
/// ```
///
/// and `lookup_commit_reference_by_name()` is the `quiet = 0` form, so a ref that
/// does not peel to a commit is reported even though the atom's own operand is
/// fine and the atom itself renders as the empty string for that ref. Peeling
/// lazily inside the formatter produces the identical stdout and no stderr at
/// all, which is why stdout and stderr are asserted separately here.
#[test]
fn ahead_behind_reports_every_ref_that_is_not_a_commit() {
    let f = Fixture::new("ab-prepass");
    let tag = f.rev_parse("treetag");
    let tree = f.rev_parse("HEAD^{tree}");
    assert_ne!(tag, tree, "the annotated tag must be distinguishable from its target");

    let (out, err, code) = f.run(&["for-each-ref", "--format=%(refname) [%(ahead-behind:main)]"]);
    assert_eq!(err, format!("error: object {tag} is a tree, not a commit\n"));
    assert_eq!(
        out,
        "refs/heads/main [0 0]\n\
         refs/heads/side [0 0]\n\
         refs/tags/treetag []\n\
         refs/tags/v1 [0 0]\n"
    );
    assert_eq!(code, 0, "the pass reports and continues; it never fails the command");
}

/// Three properties that separate the pre-pass from a formatter-side peel, each
/// of which a lazy implementation gets wrong on its own:
///
///   * `--count` is applied later, in `print_formatted_ref_array()`, so a ref
///     that never reaches the output is still walked and still reports;
///   * a `--sort` key is a `used_atom` too, so the pass fires for a format that
///     names no such atom at all;
///   * a pattern that excludes the offending ref removes it from the array, and
///     with it the report — the line follows the *array*, not the repository.
#[test]
fn the_ahead_behind_pass_follows_the_array_and_not_the_output() {
    let f = Fixture::new("ab-prepass-scope");
    let tag = f.rev_parse("treetag");
    let want = format!("error: object {tag} is a tree, not a commit\n");

    let (out, err, code) = f.run(&[
        "for-each-ref",
        "--format=%(refname) [%(ahead-behind:main)]",
        "--count=1",
    ]);
    assert_eq!(err, want, "--count truncates the output, not the pass");
    assert_eq!(out, "refs/heads/main [0 0]\n");
    assert_eq!(code, 0);

    let (out, err, code) =
        f.run(&["for-each-ref", "--format=%(refname)", "--sort=ahead-behind:main"]);
    assert_eq!(err, want, "a sort key is a used atom");
    assert_eq!(
        out,
        "refs/tags/treetag\nrefs/heads/main\nrefs/heads/side\nrefs/tags/v1\n"
    );
    assert_eq!(code, 0);

    let (out, err, code) = f.run(&[
        "for-each-ref",
        "--format=%(refname) [%(ahead-behind:main)]",
        "refs/heads",
    ]);
    assert_eq!(err, "", "the tag is not in the array, so nothing reports it");
    assert_eq!(out, "refs/heads/main [0 0]\nrefs/heads/side [0 0]\n");
    assert_eq!(code, 0);

    // The control: without any such atom there is no pass and no report, so the
    // line above cannot be coming from ref iteration in general.
    let (out, err, code) = f.run(&["for-each-ref", "--format=%(refname)"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "refs/heads/main\nrefs/heads/side\nrefs/tags/treetag\nrefs/tags/v1\n"
    );
    assert_eq!(code, 0);
}

/// The atom's own operand keeps its separate diagnostic:
/// `ahead_behind_atom_parser()` reports the type through the same non-quiet peel
/// and then `die("failed to find '%s'", arg)` — 128, before any ref is looked at,
/// so exactly one type line appears rather than one per ref.
#[test]
fn a_bad_ahead_behind_operand_still_dies_before_the_pass() {
    let f = Fixture::new("ab-operand");
    let tag = f.rev_parse("treetag");
    let (out, err, code) =
        f.run(&["for-each-ref", "--format=%(refname) [%(ahead-behind:treetag)]"]);
    assert_eq!(
        err,
        format!("error: object {tag} is a tree, not a commit\nfatal: failed to find 'treetag'\n")
    );
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

// ---------------------------------------------------------------------------
// `show-branch`
// ---------------------------------------------------------------------------

/// `append_one_rev()` takes `repo_get_oid()`'s answer and hands it to
/// `append_ref()`, which peels with `quiet = 1` — so an absent id is dropped
/// without a word rather than reaching the `die("bad sha1 reference %s")` at the
/// bottom. With nothing else asked for, that leaves the rev list empty.
#[test]
fn show_branch_drops_an_absent_id_and_exits_zero() {
    let f = Fixture::new("sb-absent");
    for id in [MISSING, MISSING_HEX, MISSING_UPPER] {
        let (out, err, code) = f.run(&["show-branch", id]);
        assert_eq!(out, "", "the absent rev contributes no output");
        assert_eq!(err, "No revs to be shown.\n", "and the notice is on stderr");
        assert_eq!(code, 0, "{id} must not turn show-branch into a failure");
    }
    // Two absent ids are still just an empty list, not an error each.
    let (out, err, code) = f.run(&["show-branch", MISSING, MISSING_HEX]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "No revs to be shown.\n", 0));
}

/// Dropped, not fatal: whatever else was asked for is still shown. `--current`
/// appends `HEAD` afterwards, and a second named rev survives on its own.
#[test]
fn show_branch_still_shows_the_other_revs() {
    let f = Fixture::new("sb-mixed");
    let expected = "[main] two\n";
    for args in [
        vec!["show-branch", "--current", MISSING],
        vec!["show-branch", MISSING, "main"],
        vec!["show-branch", "main", MISSING],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(out, expected, "for {args:?}");
        assert_eq!(err, "", "for {args:?}");
        assert_eq!(code, 0, "for {args:?}");
    }
    // `--merge-base` reduces to the surviving rev rather than failing on the
    // absent one.
    let head = f.rev_parse("HEAD");
    let (out, err, code) = f.run(&["show-branch", "--merge-base", MISSING, "main"]);
    assert_eq!((out.trim(), err.as_str(), code), (head.as_str(), "", 0));
}

/// The control: `append_one_rev()` still dies for a name `repo_get_oid()` cannot
/// decode, including one hex digit short of the rule.
#[test]
fn show_branch_still_dies_for_an_unresolvable_name() {
    let f = Fixture::new("sb-control");
    for name in [CONTROL, SHORT_39] {
        let (out, err, code) = f.run(&["show-branch", name]);
        assert_eq!(out, "");
        assert_eq!(err, format!("fatal: bad sha1 reference {name}\n"));
        assert_eq!(code, 128);
        // …and with `--current`, which reaches the same call a second time.
        let (_, err, code) = f.run(&["show-branch", "--current", name]);
        assert_eq!(err, format!("fatal: bad sha1 reference {name}\n"));
        assert_eq!(code, 128);
    }
}

/// The other half of `append_ref()`'s quiet peel: a name that resolves to a
/// present non-commit is dropped just as silently as an absent one, with no
/// `object … is a tree` line, because `show-branch` passes `quiet = 1` where the
/// ref filters pass 0.
#[test]
fn show_branch_drops_a_non_commit_silently() {
    let f = Fixture::new("sb-tree");
    let tree = f.rev_parse("HEAD^{tree}");
    for name in [tree.as_str(), "treetag"] {
        let (out, err, code) = f.run(&["show-branch", name]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "No revs to be shown.\n", 0), "for {name}");
    }
    let (out, err, code) = f.run(&["show-branch", "--current", &tree]);
    assert_eq!((out.as_str(), err.as_str(), code), ("[main] two\n", "", 0));
}

/// The positive control: real revs are unaffected by any of the above.
#[test]
fn show_branch_shows_real_revs() {
    let f = Fixture::new("sb-real");
    let head = f.rev_parse("HEAD");
    let (out, err, code) = f.run(&["show-branch", &head]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!("[{head}] two\n"));

    // Bare `--current` names no rev, so `snarf_refs()` supplies every head and
    // the two-branch listing is what a rev *surviving* the filters looks like.
    let (out, err, code) = f.run(&["show-branch", "--current"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, "* [main] two\n ! [side] two\n--\n*+ [main] two\n");
}
