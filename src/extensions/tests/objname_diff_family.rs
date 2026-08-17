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

/// `setup_revisions()`'s `seen_dashdash`, the *other* half of the gate that
/// decides between `bad revision` and `ambiguous argument`:
///
/// ```c
/// if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
///         int j;
///         if (seen_dashdash || *arg == '^')
///                 die(_("bad revision '%s'"), arg);
///         for (j = i; j < argc; j++)
///                 verify_filename(revs->prefix, argv[j], j == i);
///         …
/// }
/// ```
///
/// It is established by a scan of the whole argument vector before any operand
/// is resolved, so it is *not* positional: a separator anywhere makes every
/// operand revision-only, including the ones written in front of it. Each case
/// is paired with the same argv minus the separator, because the bug being
/// pinned is that both spellings produced the pathspec wording.
#[test]
fn a_separator_anywhere_makes_a_failed_operand_a_bad_revision() {
    let f = Fixture::new("seen-dashdash");

    for verb in ["diff", "diff-index", "diff-files"] {
        for tok in [
            UNRESOLVABLE,
            "nosuchthing..HEAD",
            "HEAD..nosuchthing",
            "nosuchthing...HEAD",
        ] {
            // With the separator: `die(_("bad revision '%s'"), arg)`, naming the
            // token as written — `setup_revisions()` still holds `argv[i]`.
            let (out, err, code) = f.run(&[verb, tok, "--"]);
            assert_eq!(code, 128, "{verb} {tok} --: {out}{err}");
            assert_eq!(err, format!("fatal: bad revision '{tok}'\n"), "{verb} {tok} --");

            // Without it the same token is still a pathspec candidate, so it
            // reaches `verify_filename()` and gets the three-line wording.
            let (out, err, code) = f.run(&[verb, tok]);
            assert_eq!(code, 128, "{verb} {tok}: {out}{err}");
            assert_eq!(err, ambiguous(tok), "{verb} {tok}");
        }
    }
}

/// `handle_revision_arg_1()`'s exclusion mark:
///
/// ```c
/// if (*arg == '^') {
///         flags ^= UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// ```
///
/// The mark is stripped before anything resolves, so the two diagnostics below
/// it name *different* strings and that is the point of the pairing: the
/// `bad object` die comes from `get_reference()`, past the mark, while the
/// `bad revision` die comes from `setup_revisions()`, which still holds the
/// original `argv[i]` and prints the mark.
#[test]
fn an_exclusion_mark_is_stripped_before_resolution_but_kept_in_the_die() {
    let f = Fixture::new("uninteresting");

    for verb in ["diff", "diff-index", "diff-files"] {
        // Resolved by the full-hex rule, absent from the odb: named past the mark.
        let (out, err, code) = f.run(&[verb, &format!("^{ABSENT}")]);
        assert_eq!(code, 128, "{verb} ^<absent>: {out}{err}");
        assert_eq!(err, bad_object(ABSENT), "{verb} ^<absent>");

        // Resolves to nothing: never offered to `verify_filename()`, and named
        // *with* the mark.
        let (out, err, code) = f.run(&[verb, &format!("^{UNRESOLVABLE}")]);
        assert_eq!(code, 128, "{verb} ^<unresolvable>: {out}{err}");
        assert_eq!(err, format!("fatal: bad revision '^{UNRESOLVABLE}'\n"), "{verb} ^…");
    }

    // With a single tree the flag changes nothing: `cmd_diff_index()` reads the
    // tree out of `revs->pending` regardless of the flag bits, so `^HEAD` and
    // `HEAD` are the same diff.
    let (marked, err, code) = f.run(&["diff-index", "^HEAD"]);
    assert_eq!(code, 0, "{err}");
    let (plain, err, code) = f.run(&["diff-index", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(marked, plain);
    assert!(!plain.is_empty(), "the fixture must have an unstaged edit to show");
}

/// `diff_opt_find_object()`:
///
/// ```c
/// if (repo_get_oid(the_repository, arg, &oid))
///         return error(_("unable to resolve '%s'"), arg);
/// ```
///
/// `repo_get_oid()`, so the full-hex rule applies and an id the repository does
/// not have is a perfectly good needle that simply matches nothing — exit 0 with
/// empty output, not an error. Only a name that resolves to *nothing* is the
/// callback's `error()`, and `error()` from an option callback is
/// parse-options' `PARSE_OPT_ERROR`: a bare exit 129 with no usage block.
#[test]
fn find_object_takes_any_resolvable_id_and_only_errors_on_an_unresolvable_name() {
    // The tag is the fixture directory name and tests run in parallel in one
    // process, so it has to be unique across this file.
    let f = Fixture::new("find-object-resolve");

    for verb in ["diff", "diff-index", "diff-files"] {
        // The two spellings are one `OPT_CALLBACK_F` without `PARSE_OPT_OPTARG`,
        // so the separated form takes the next argv entry as its value.
        for mut args in [
            vec![verb.to_string(), format!("--find-object={ABSENT}")],
            vec![verb.to_string(), "--find-object".to_string(), ABSENT.to_string()],
            vec![verb.to_string(), format!("--find-object={ABSENT_UPPER}")],
        ] {
            // `diff-index` needs its tree-ish; the other two take none.
            if verb == "diff-index" {
                args.push("HEAD".to_string());
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let (out, err, code) = f.run(&argv);
            assert_eq!(code, 0, "{argv:?}: {out}{err}");
            assert_eq!(out, "", "{argv:?}");
            assert_eq!(err, "", "{argv:?}");
        }

        // A name that resolves to nothing at all.
        for args in [
            vec![verb.to_string(), format!("--find-object={UNRESOLVABLE}")],
            vec![verb.to_string(), "--find-object".to_string(), UNRESOLVABLE.to_string()],
        ] {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let (out, err, code) = f.run(&argv);
            assert_eq!(code, 129, "{argv:?}: {out}{err}");
            assert_eq!(err, format!("error: unable to resolve '{UNRESOLVABLE}'\n"), "{argv:?}");
        }
    }

    // A real id does select: the one blob `f.txt` had at HEAD is the pre-image of
    // the only pair `diff-files` has, so the pair survives the objfind filter.
    let (blob, err, code) = f.run(&["rev-parse", "HEAD:f.txt"]);
    assert_eq!(code, 0, "{err}");
    let blob = blob.trim();
    let (out, err, code) = f.run(&["diff-files", "--name-only", &format!("--find-object={blob}")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "f.txt\n");
}

/// `diff_setup_done()`'s two pickaxe `die()`s
/// (`HAS_MULTI_BITS(pickaxe_opts & DIFF_PICKAXE_KINDS_MASK)` and the
/// `…_ALL_OBJFIND_MASK` one right after it). Both fire on the *combination*, so
/// each verb has to reach them even when it does not render the options
/// involved: `diff` does not implement `-S`/`-G`, and still owes git's `fatal:`
/// at 128 rather than a bail. Measured against stock 2.55.0, they beat an
/// unknown option in either argv position.
#[test]
fn conflicting_pickaxe_kinds_are_gits_fatal_on_every_verb() {
    let f = Fixture::new("pickaxe-conflict");
    let kinds = "fatal: options '-G', '-S', and '--find-object' cannot be used together\n";
    let all_objfind = "fatal: options '--pickaxe-all' and '--find-object' cannot be used \
                       together, use '--pickaxe-all' with '-G' and '-S'\n";

    let find = format!("--find-object={ABSENT}");
    for verb in ["diff", "diff-index", "diff-files"] {
        for (argv, want) in [
            (vec![verb, &find, "-Sfoo"], kinds),
            (vec![verb, "-Gfoo", "-Sfoo"], kinds),
            (vec![verb, &find, "--pickaxe-all"], all_objfind),
            (vec![verb, "--pickaxe-all", &find], all_objfind),
            // `diff_setup_done()` closes `setup_revisions()`, so it beats the
            // leftover-argv complaint about an unknown option in either position.
            (vec![verb, "--bogus-flag", &find, "--pickaxe-all"], all_objfind),
            (vec![verb, &find, "--pickaxe-all", "--bogus-flag"], all_objfind),
        ] {
            let (out, err, code) = f.run(&argv);
            assert_eq!(code, 128, "{argv:?}: {out}{err}");
            assert_eq!(err, want, "{argv:?}");
        }
    }
}

/// `diff_cache()` (`diff-lib.c`), reached from `run_diff_index()`:
///
/// ```c
/// tree = repo_parse_tree_indirect(the_repository, tree_oid);
/// if (!tree)
///         return error("bad tree object %s",
///                      tree_name ? tree_name : oid_to_hex(tree_oid));
/// ```
///
/// ```c
/// if (diff_cache(revs, &oid, name, cached))
///         exit(128);
/// ```
///
/// `error()`, so the line reads `error:` and not `fatal:`, and the status comes
/// from the caller's `exit(128)`. The operand is *not* re-diagnosed as a bad
/// revision: it resolved perfectly well, it simply does not peel to a tree.
#[test]
fn a_diff_index_operand_that_is_not_a_treeish_is_a_bad_tree_object() {
    let f = Fixture::new("bad-tree-object");
    let (blob, err, code) = f.run(&["rev-parse", "HEAD:f.txt"]);
    assert_eq!(code, 0, "{err}");
    let blob = blob.trim().to_string();

    // Marked or not: `handle_revision_arg_1()` advances past the `^` before the
    // object is pended, so `ent->name` — and the message — has no mark either way.
    for arg in [blob.clone(), format!("^{blob}")] {
        let (out, err, code) = f.run(&["diff-index", &arg]);
        assert_eq!(code, 128, "diff-index {arg}: {out}{err}");
        assert_eq!(err, format!("error: bad tree object {blob}\n"), "diff-index {arg}");
    }
}

/// `handle_revision_arg_1()`'s guard in front of `handle_dotdot()`:
///
/// ```c
/// if (!cant_be_filename && !strcmp(arg, "..")) {
///         /*
///          * Just ".."?  That is not a range but the
///          * pathspec for the parent directory.
///          */
///         ret = -1;
///         goto out;
/// }
/// ```
///
/// So the token becomes prune data, and the diagnostic the user finally gets is
/// the *pathspec* layer's, from `init_pathspec_item()` — not a revision error.
/// `parse_pathspec()` runs inside `setup_revisions()`, which is why it precedes
/// `diff_setup_done()`'s pickaxe checks and the operand-count usage error
/// (measured: `git diff-index -Gx -Sx -- ..` reports the pathspec).
#[test]
fn a_bare_parent_directory_pathspec_dies_in_the_pathspec_layer() {
    let f = Fixture::new("parent-dir-pathspec");
    let want = format!(
        "fatal: ..: '..' is outside repository at '{}'\n",
        f.repo.display()
    );

    for argv in [
        vec!["diff", ".."],
        vec!["diff-files", ".."],
        vec!["diff-index", "HEAD", "--", ".."],
        vec!["diff-index", "-Gx", "-Sx", "--", ".."],
        vec!["diff-files", "-Gx", "-Sx", "--", ".."],
    ] {
        let (out, err, code) = f.run(&argv);
        assert_eq!(code, 128, "{argv:?}: {out}{err}");
        assert_eq!(err, want, "{argv:?}");
    }
}

/// `parse_algorithm_value()` (`diff.c`) names four algorithms, and all four now
/// select a real one in every diff verb:
///
/// ```c
/// static int parse_algorithm_value(const char *value)
/// {
///         if (!value)
///                 return -1;
///         else if (!strcasecmp(value, "myers") || !strcasecmp(value, "default"))
///                 return 0;
///         else if (!strcasecmp(value, "minimal"))
///                 return XDF_NEED_MINIMAL;
///         else if (!strcasecmp(value, "patience"))
///                 return XDF_PATIENCE_DIFF;
///         else if (!strcasecmp(value, "histogram"))
///                 return XDF_HISTOGRAM_DIFF;
///         return -1;
/// }
/// ```
///
/// The fixture is chosen so stock 2.55.0 gives three *different* patches for the
/// four names (myers == minimal, patience and histogram each its own), because a
/// fixture where they coincide cannot tell "the algorithm ran" from "the flag was
/// ignored" — which is exactly the failure this pins. The expected bytes below
/// are stock's, captured under the parity environment.
#[test]
fn every_algorithm_name_selects_a_distinct_ported_xdiff() {
    let f = Fixture::new("algorithms");
    std::fs::write(
        f.repo.join("alg.txt"),
        "a\nb\nc\nd\ne\nf\ng\nh\na\nb\nc\nd\ne\nf\ng\nh\nx\ny\nz\na\nb\nc\n",
    )
    .unwrap();
    f.ok(&["add", "alg.txt"]);
    f.ok(&["commit", "-q", "-m", "alg"]);
    std::fs::write(
        f.repo.join("alg.txt"),
        "x\na\nb\nc\nd\ne\nf\ng\nh\ny\na\nb\nc\nd\ne\nf\ng\nh\na\nb\nc\nz\n",
    )
    .unwrap();

    const HEADER: &str = "diff --git a/alg.txt b/alg.txt\n\
                          index 3043f60..7918bdd 100644\n\
                          --- a/alg.txt\n\
                          +++ b/alg.txt\n";
    let myers = format!(
        "{HEADER}@@ -1,3 +1,4 @@\n+x\n a\n b\n c\n@@ -6,6 +7,7 @@ e\n f\n g\n h\n+y\n a\n b\n c\n\
         @@ -14,9 +16,7 @@ e\n f\n g\n h\n-x\n-y\n-z\n a\n b\n c\n+z\n"
    );
    let patience = format!(
        "{HEADER}@@ -1,22 +1,22 @@\n-a\n-b\n-c\n-d\n-e\n-f\n-g\n-h\n-a\n-b\n-c\n-d\n-e\n-f\n\
         -g\n-h\n x\n+a\n+b\n+c\n+d\n+e\n+f\n+g\n+h\n y\n+a\n+b\n+c\n+d\n+e\n+f\n+g\n+h\n+a\n\
         +b\n+c\n z\n-a\n-b\n-c\n"
    );
    let histogram = format!(
        "{HEADER}@@ -1,22 +1,22 @@\n-a\n-b\n-c\n-d\n-e\n-f\n-g\n-h\n-a\n-b\n-c\n-d\n-e\n-f\n\
         -g\n-h\n x\n-y\n-z\n a\n b\n c\n+d\n+e\n+f\n+g\n+h\n+y\n+a\n+b\n+c\n+d\n+e\n+f\n+g\n\
         +h\n+a\n+b\n+c\n+z\n"
    );

    // `myers`/`default` are one value, matched case-insensitively (`strcasecmp`),
    // and `minimal` coincides with them on this input — under stock too, which is
    // why it is asserted equal rather than distinct.
    for value in ["myers", "MYERS", "default", "Default", "minimal"] {
        let (out, err, code) =
            f.run(&["diff-files", "-p", &format!("--diff-algorithm={value}"), "--", "alg.txt"]);
        assert_eq!(code, 0, "{value}: {err}");
        assert_eq!(out, myers, "--diff-algorithm={value}");
    }

    // The three spellings of one setting: `=<v>`, the separated form (an
    // `OPT_CALLBACK_F` with a required argument), and the `OPT_BIT` alias.
    for (value, want) in [("patience", &patience), ("histogram", &histogram)] {
        let glued = format!("--diff-algorithm={value}");
        let alias = format!("--{value}");
        for argv in [
            vec!["diff-files", "-p", glued.as_str(), "--", "alg.txt"],
            vec!["diff-files", "-p", "--diff-algorithm", value, "--", "alg.txt"],
            vec!["diff-files", "-p", alias.as_str(), "--", "alg.txt"],
        ] {
            let (out, err, code) = f.run(&argv);
            assert_eq!(code, 0, "{argv:?}: {err}");
            assert_eq!(out, *want, "{argv:?}");
        }
    }

    // The same three patches through `diff` and `diff-index`, so the selection is
    // not wired in one verb only. `diff` reads the worktree like `diff-files`;
    // `diff-index --cached` would see no change, so it is given the staged pair.
    for (value, want) in [("myers", &myers), ("patience", &patience), ("histogram", &histogram)] {
        let glued = format!("--diff-algorithm={value}");
        let (out, err, code) = f.run(&["diff", glued.as_str(), "--", "alg.txt"]);
        assert_eq!(code, 0, "diff {value}: {err}");
        assert_eq!(out, *want, "diff --diff-algorithm={value}");
    }
    f.ok(&["add", "alg.txt"]);
    for (value, want) in [("myers", &myers), ("patience", &patience), ("histogram", &histogram)] {
        let glued = format!("--diff-algorithm={value}");
        let (out, err, code) = f.run(&[
            "diff-index",
            "-p",
            "--cached",
            glued.as_str(),
            "HEAD",
            "--",
            "alg.txt",
        ]);
        assert_eq!(code, 0, "diff-index {value}: {err}");
        assert_eq!(out, *want, "diff-index --diff-algorithm={value}");
    }
}

/// The `return -1` arm of `parse_algorithm_value()`, which
/// `diff_opt_diff_algorithm()` turns into `error()` — parse-options'
/// `PARSE_OPT_ERROR`, i.e. a bare exit 129 with no usage block after it.
#[test]
fn an_unknown_algorithm_value_is_error_129_on_every_verb() {
    let f = Fixture::new("algorithm-bad");
    let want = "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" \
                and \"histogram\"\n";

    for verb in ["diff", "diff-index", "diff-files"] {
        for value in ["bogus", "", "patienc"] {
            let (out, err, code) = f.run(&[verb, &format!("--diff-algorithm={value}")]);
            assert_eq!(code, 129, "{verb} --diff-algorithm={value}: {out}{err}");
            assert_eq!(err, want, "{verb} --diff-algorithm={value}");
        }
        // The separated form reaches the same callback.
        let (out, err, code) = f.run(&[verb, "--diff-algorithm", "bogus"]);
        assert_eq!(code, 129, "{verb} --diff-algorithm bogus: {out}{err}");
        assert_eq!(err, want, "{verb} --diff-algorithm bogus");
    }
}

/// `cmd_diff()`'s two operand arrays (`builtin/diff.c:576-604`): a pending object
/// is deref-tagged, a commit is replaced by its tree, and what is left is sorted
/// into `ent` (trees) or `blob` (blobs) — a blob is its own arm, not a tree-ish
/// that failed to peel.
///
/// ```c
/// if (!ent.nr) {
///         switch (blobs) {
///         case 0:  builtin_diff_files(&rev, argc, argv); break;
///         case 1:  if (paths != 1) usage(builtin_diff_usage);
///                  builtin_diff_b_f(&rev, argc, argv, blob); break;
///         case 2:  if (paths) usage(builtin_diff_usage);
///                  builtin_diff_blobs(&rev, argc, argv, blob); break;
///         default: usage(builtin_diff_usage);
///         }
/// }
/// else if (blobs)
///         usage(builtin_diff_usage);
/// ```
///
/// Only the shapes that reach a `usage()` are pinned here: `builtin_diff_b_f()`
/// and `builtin_diff_blobs()` are not ported and are refused rather than
/// approximated, so asserting their output would be asserting a gap.
#[test]
fn a_blob_operand_takes_cmd_diffs_blob_arm_not_the_tree_dispatch() {
    let f = Fixture::new("blob-operand");
    let (blob, err, code) = f.run(&["rev-parse", "HEAD:f.txt"]);
    assert_eq!(code, 0, "{err}");
    let blob = blob.trim().to_string();

    for argv in [
        // `ent.nr == 0`, one blob, no path: `paths != 1` → usage.
        vec!["diff", blob.as_str()],
        // The mark is a flag; the operand under it is still a blob.
        vec!["diff", &format!("^{blob}")],
        // A range pends the blob *and* HEAD's tree: `ent.nr` is 1 and `blobs` is
        // 1, which is the `else if (blobs) usage(...)` arm.
        vec!["diff", &format!("{blob}..HEAD")],
        // Same arm, reached with the two operands written out.
        vec!["diff", blob.as_str(), "HEAD"],
    ] {
        let (out, err, code) = f.run(&argv);
        assert_eq!(code, 129, "{argv:?}: {out}{err}");
        assert!(
            err.starts_with("usage: git diff [<options>] [<commit>] [--] [<path>...]\n"),
            "{argv:?}: {err}"
        );
        // The gitoxide peel error this used to surface must not be back.
        assert!(!err.contains("peel"), "{argv:?}: {err}");
    }
}

/// The one place the exclusion mark changes `diff`'s *output* rather than only
/// its diagnostics — `builtin_diff_tree()` (`builtin/diff.c:196-204`):
///
/// ```c
/// /*
///  * We saw two trees, ent0 and ent1.  If ent1 is uninteresting,
///  * swap them.
///  */
/// if (ent1->item->flags & UNINTERESTING)
///         swap = 1;
/// oid[swap] = &ent0->item->oid;
/// oid[1 - swap] = &ent1->item->oid;
/// ```
///
/// It reads `ent1` alone, so the mark counts on the *second* operand and not the
/// first. The unmarked spelling is asserted to differ, because a test where the
/// two coincide cannot tell a working swap from no swap at all.
#[test]
fn an_uninteresting_second_tree_swaps_the_pre_image() {
    let f = Fixture::new("tree-swap");
    std::fs::write(f.repo.join("f.txt"), "a\nb\nc\nd\n").unwrap();
    f.ok(&["add", "f.txt"]);
    f.ok(&["commit", "-q", "-m", "c2"]);

    let forward = f.run(&["diff", "HEAD~1", "HEAD", "--", "f.txt"]);
    let reverse = f.run(&["diff", "HEAD", "HEAD~1", "--", "f.txt"]);
    assert_eq!(forward.2, 0, "{}", forward.1);
    assert_ne!(forward.0, reverse.0, "the fixture must make direction observable");

    // `^` on the second operand: `ent1` is UNINTERESTING, so the pair swaps and
    // the result is the *forward* diff.
    let (out, err, code) = f.run(&["diff", "HEAD", "^HEAD~1", "--", "f.txt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, forward.0, "`HEAD ^HEAD~1` must diff HEAD~1 against HEAD");

    // `^` on the first operand only: `ent1` is clean, no swap.
    let (out, err, code) = f.run(&["diff", "^HEAD~1", "HEAD", "--", "f.txt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, forward.0, "`^HEAD~1 HEAD` is the plain forward diff");

    // Both marked: `ent1` is still UNINTERESTING, so it still swaps.
    let (out, err, code) = f.run(&["diff", "^HEAD", "^HEAD~1", "--", "f.txt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, forward.0, "`^HEAD ^HEAD~1` swaps on ent1 alone");
}
