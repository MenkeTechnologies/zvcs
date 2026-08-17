//! `diff`, `diff-index` and `diff-files` handed a well-formed object id the
//! repository does not have.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id: decoded and
//! returned before the object database is consulted at all. All three commands
//! here reach `setup_revisions()`, which therefore sees such a name **resolve**
//! and only fails one step later, in `get_reference()`'s
//! `die("bad object %s", name)` — a message that names the object, at a point
//! ahead of every operand-count and pathspec check.
//!
//! | argv                              | git 2.55.0                                        | zvcs before                                    |
//! |-----------------------------------|---------------------------------------------------|------------------------------------------------|
//! | `diff <oid>`                      | `fatal: bad object <oid>`                         | `fatal: ambiguous argument '<oid>': …`         |
//! | `diff <oid> HEAD`                 | `fatal: bad object <oid>`                         | `fatal: ambiguous argument '<oid>': …`         |
//! | `diff-index <oid>`                | `fatal: bad object <oid>`                         | `fatal: ambiguous argument '<oid>': …`         |
//! | `diff-files <oid>`                | `fatal: bad object <oid>`                         | `fatal: ambiguous argument '<oid>': …`         |
//! | `diff <oid>..HEAD`                | `fatal: Invalid revision range <oid>..HEAD`       | raw gitoxide error naming `src/ported/…`, exit 1 |
//! | `diff <oid>...HEAD`               | `fatal: Invalid symmetric difference expression …`| raw gitoxide error naming `src/ported/…`, exit 1 |
//! | `diff nosuchthing..HEAD`          | `fatal: ambiguous argument 'nosuchthing..HEAD': …`| raw gitoxide error naming `src/ported/…`, exit 1 |
//! | `diff-files --find-object=<oid>`  | exit 0, no output                                 | `error: unable to resolve '<oid>'`             |
//!
//! A range is its own rule, `handle_dotdot_1()` (`revision.c`): both endpoints go
//! through `get_oid_with_context()` — so a full-length hex passes — and only then
//! are both `parse_object()`ed, with `dotdot_missing()` naming **the whole token**
//! rather than the endpoint that failed. An endpoint that resolves to nothing at
//! all is not fatal there: `handle_dotdot()` merely reports failure and the token
//! is retried as a single name, which is how `../foo` stays a path.
//!
//! Two properties are pinned everywhere below, because either alone can be
//! satisfied by the wrong code:
//!
//! * the absent-but-well-formed id gets the "resolved, object missing" message;
//! * a name that resolves to nothing keeps the "no such name" message — the two
//!   outcomes git distinguishes must stay distinct, in both directions.
//!
//! Plus the shape of the rule itself: length-exact (39 and 41 hex digits fall
//! through to the ordinary parser) and case-insensitive, since `get_oid_hex()` is
//! built on `hexval()`.
//!
//! Expectations are stock git 2.55.0's, captured with the parity harness's
//! environment (fixed identity and date, no global or system config, `LC_ALL=C`,
//! `TZ=UTC`).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Well-formed SHA-1 hex, and not an object any fixture here can contain.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// The same rule, upper case. `get_oid_hex()` is built on `hexval()`, which is
/// case-insensitive, so this must take exactly the same path as [`ABSENT`].
const ABSENT_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

/// One hex digit short of `hexsz`: the first branch does not apply and the name
/// is handled as an (unmatchable) abbreviation instead.
const SHORT_HEX: &str = "012345678901234567890123456789012345678";

/// One hex digit long. Same reasoning, from the other side of the boundary.
const LONG_HEX: &str = "01234567890123456789012345678901234567890";

/// The control: not hex, not a ref, resolves to nothing at all. Every assertion
/// about an absent id is paired with one about this, because the bug being
/// pinned is precisely that the two were treated alike.
const UNRESOLVABLE: &str = "nosuchthing";

/// `verify_filename()`'s die, which is what a token that resolved to nothing
/// gets once it is taken for a path that is not there.
fn ambiguous(arg: &str) -> String {
    format!(
        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    )
}

/// `get_reference()`'s die: the name resolved, the object is not there.
fn bad_object(arg: &str) -> String {
    format!("fatal: bad object {arg}\n")
}

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
    /// One commit, plus an unstaged edit so every command here has real work to
    /// do when it is *not* being handed a broken name.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-objname-difffam-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("f.txt"), "a\nb\nc\n").unwrap();
        f.ok(&["add", "f.txt"]);
        f.ok(&["commit", "-q", "-m", "c1"]);
        std::fs::write(f.repo.join("f.txt"), "a\nb\nc\nd\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(self.repo.clone(), args)
    }

    fn run_in(&self, dir: PathBuf, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "@1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
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

    fn ok(&self, args: &[&str]) {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "setup `git {args:?}` failed: {out}{err}");
    }
}

/// The single-name form across all three verbs, in every argv position git can
/// reach it from: alone, as the first of two revisions, and before a `--`.
///
/// The `-- f.txt` spellings matter on their own because `verify_filename()` would
/// otherwise be a plausible place for the token to end up, and `diff-files HEAD`
/// proves the die beats `cmd_diff_files()`'s operand-count usage error rather than
/// merely coinciding with it.
#[test]
fn absent_full_hex_is_reported_as_a_bad_object() {
    let f = Fixture::new("bad-object");

    for args in [
        vec!["diff", ABSENT],
        vec!["diff", ABSENT, "HEAD"],
        vec!["diff", ABSENT, "--", "f.txt"],
        vec!["diff", "--cached", ABSENT],
        vec!["diff-index", ABSENT],
        vec!["diff-index", ABSENT, "HEAD"],
        vec!["diff-index", ABSENT, "--", "f.txt"],
        vec!["diff-files", ABSENT],
        vec!["diff-files", ABSENT, "--", "f.txt"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, bad_object(ABSENT), "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }
}

/// The other direction, verb by verb: a name that resolves to nothing keeps
/// `verify_filename()`'s wording. Without this the fix could be a blanket
/// rewording that collapses the distinction the other way.
#[test]
fn unresolvable_names_keep_the_ambiguous_diagnostic() {
    let f = Fixture::new("unresolvable");

    for args in [
        vec!["diff", UNRESOLVABLE],
        vec!["diff-index", UNRESOLVABLE],
        vec!["diff-files", UNRESOLVABLE],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, ambiguous(UNRESOLVABLE), "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }
}

/// `handle_dotdot_1()` + `dotdot_missing()`: both endpoints resolve, one object is
/// missing, and the *whole token* is named. Checked from both sides of the range
/// and in both the empty-endpoint spellings, since `HEAD` is substituted for an
/// empty side before resolution and an implementation that substituted afterwards
/// would still pass the two-sided cases.
#[test]
fn a_range_with_an_absent_endpoint_names_the_whole_token() {
    let f = Fixture::new("range-missing");

    for tok in [
        format!("{ABSENT}..HEAD"),
        format!("HEAD..{ABSENT}"),
        format!("{ABSENT}..{ABSENT}"),
        format!("{ABSENT}.."),
        format!("..{ABSENT}"),
        format!("{ABSENT_UPPER}..HEAD"),
    ] {
        let (out, err, code) = f.run(&["diff", &tok]);
        assert_eq!(err, format!("fatal: Invalid revision range {tok}\n"), "{tok}");
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 128, "{tok}");
    }

    for tok in [
        format!("{ABSENT}...HEAD"),
        format!("HEAD...{ABSENT}"),
        format!("{ABSENT}..."),
    ] {
        let (out, err, code) = f.run(&["diff", &tok]);
        assert_eq!(
            err,
            format!("fatal: Invalid symmetric difference expression {tok}\n"),
            "{tok}"
        );
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 128, "{tok}");
    }
}

/// A range endpoint that resolves to *nothing* is a different outcome:
/// `handle_dotdot()` reports failure, the token is retried as a single name, and
/// `verify_filename()` has the last word — naming the token as written.
///
/// This is the control for the range rule, and it is also where the raw gitoxide
/// error used to escape: before the fix every case here printed a vendored
/// `src/ported/…` path and exited 1.
#[test]
fn a_range_with_an_unresolvable_endpoint_falls_back_to_verify_filename() {
    let f = Fixture::new("range-unresolvable");

    for tok in [
        format!("{UNRESOLVABLE}..HEAD"),
        format!("HEAD..{UNRESOLVABLE}"),
        format!("{UNRESOLVABLE}...HEAD"),
        format!("{SHORT_HEX}..HEAD"),
        format!("{LONG_HEX}..HEAD"),
    ] {
        let (out, err, code) = f.run(&["diff", &tok]);
        assert_eq!(err, ambiguous(&tok), "{tok}");
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 128, "{tok}");
    }
}

/// No diff-family diagnostic may quote the vendored gitoxide tree at the user, and
/// none of these shapes may exit 1 — every one of them is a `die()` in git, so 128
/// is the only acceptable code. Asserted separately from the wording because a
/// future refactor could restore a `?` without changing any message above.
#[test]
fn no_diagnostic_leaks_the_vendored_gitoxide_tree() {
    let f = Fixture::new("no-leak");

    for tok in [
        format!("{ABSENT}..HEAD"),
        format!("{ABSENT}...HEAD"),
        format!("{UNRESOLVABLE}..HEAD"),
        format!("{UNRESOLVABLE}...HEAD"),
        format!("{ABSENT}..{UNRESOLVABLE}"),
    ] {
        let (out, err, code) = f.run(&["diff", &tok]);
        assert!(!err.contains("src/ported"), "{tok} leaked: {err}");
        assert!(!err.contains("gix-revision"), "{tok} leaked: {err}");
        assert!(err.starts_with("fatal: "), "{tok} is not a git die(): {err}");
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 128, "{tok}");
    }
}

/// A non-commit endpoint of a symmetric range is `lookup_commit_reference_gently()`
/// complaining first and `dotdot_missing()` having the last word — two lines, in
/// that order, at 128. This is the other shape that used to escape as a raw
/// gitoxide error, and it exercises the "resolves, exists, wrong type" corner that
/// the absent-object cases never reach.
#[test]
fn a_symmetric_range_on_a_non_commit_reports_both_lines() {
    let f = Fixture::new("range-non-commit");
    let tree = f.run(&["rev-parse", "HEAD^{tree}"]).0.trim().to_owned();
    assert_eq!(tree.len(), 40, "fixture must yield a full tree id");

    for tok in [format!("{tree}...HEAD"), format!("HEAD...{tree}")] {
        let (out, err, code) = f.run(&["diff", &tok]);
        assert_eq!(
            err,
            format!(
                "error: object {tree} is a tree, not a commit\n\
                 fatal: Invalid symmetric difference expression {tok}\n"
            ),
            "{tok}"
        );
        assert_eq!(out, "", "{tok}");
        assert_eq!(code, 128, "{tok}");
    }
}

/// The rule is `len == hexsz`, not "looks like hex", and it ignores case. Checked
/// per verb because each resolves at its own call site and one could easily be
/// fixed with a looser test than another.
#[test]
fn the_rule_is_length_exact_and_case_insensitive() {
    let f = Fixture::new("shape");

    for verb in ["diff", "diff-index", "diff-files"] {
        for near in [SHORT_HEX, LONG_HEX] {
            let (out, err, code) = f.run(&[verb, near]);
            assert_eq!(err, ambiguous(near), "{verb} {near} must not be read as an object id");
            assert_eq!(out, "", "{verb} {near}");
            assert_eq!(code, 128, "{verb} {near}");
        }

        let (out, err, code) = f.run(&[verb, ABSENT_UPPER]);
        assert_eq!(err, bad_object(ABSENT_UPPER), "{verb} {ABSENT_UPPER}");
        assert_eq!(out, "", "{verb} {ABSENT_UPPER}");
        assert_eq!(code, 128, "{verb} {ABSENT_UPPER}");
    }
}

/// `--find-object` is `repo_get_oid()` too, so an id the repository does not have
/// is a perfectly valid needle that simply matches nothing — exit 0, no output. A
/// name that resolves to nothing is still `error()`, which `diff_opt_parse()`
/// answers with parse-options' 129 and no usage block.
#[test]
fn find_object_accepts_an_absent_id_and_still_rejects_a_bad_name() {
    let f = Fixture::new("find-object");

    for needle in [ABSENT, ABSENT_UPPER] {
        let (out, err, code) = f.run(&["diff-files", &format!("--find-object={needle}")]);
        assert_eq!(err, "", "{needle}");
        assert_eq!(out, "", "{needle}");
        assert_eq!(code, 0, "{needle}");
    }

    let (out, err, code) = f.run(&["diff-files", &format!("--find-object={UNRESOLVABLE}")]);
    assert_eq!(err, format!("error: unable to resolve '{UNRESOLVABLE}'\n"));
    assert_eq!(out, "");
    assert_eq!(code, 129);
}

/// Names that *are* present must still work — the resolver change sits on the path
/// every ordinary invocation takes, so a regression there would be invisible in
/// every test above. The range forms are included because they now resolve their
/// endpoints eagerly, which the old code did not.
#[test]
fn present_names_are_unaffected() {
    let f = Fixture::new("present");
    let want = ":100644 100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e \
                0000000000000000000000000000000000000000 M\tf.txt\n";

    let (out, err, code) = f.run(&["diff-files"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, want);

    let (out, err, code) = f.run(&["diff-index", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, want);

    let (out, err, code) = f.run(&["diff", "--name-only", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "f.txt\n");

    // `A..B` and `A...B` between a commit and itself are empty, but only if both
    // endpoints were resolved and diffed rather than mistaken for pathspecs.
    for tok in ["HEAD..HEAD", "HEAD...HEAD"] {
        let (out, err, code) = f.run(&["diff", tok]);
        assert_eq!(code, 0, "{tok}: {err}");
        assert_eq!(out, "", "{tok}");
    }

    // A path that merely contains `..` stays a path: `handle_dotdot()` fails to
    // resolve `/f.txt` and the token is retried whole.
    let sub = f.repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let (out, err, code) = f.run_in(sub, &["diff", "--name-only", "../f.txt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "f.txt\n");
}
