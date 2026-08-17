//! `reset` and `grep` handed a well-formed object id the repository does not have.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid))
//!         return 0;
//! ```
//!
//! so a name of exactly `hexsz` hex digits *is* the object id: decoded and
//! returned before the object database is consulted at all. Both commands here
//! take an object name off argv and both therefore see such a name **resolve**,
//! then fail later — from the code that actually wants the object's bytes, with
//! a message of its own:
//!
//! | command                       | git 2.55.0                                     | zvcs before                              |
//! |-------------------------------|------------------------------------------------|------------------------------------------|
//! | `reset --hard <oid>`          | `fatal: Could not parse object '<oid>'.`       | `fatal: ambiguous argument '<oid>': …`   |
//! | `reset <oid> -- f.txt`        | `fatal: Could not parse object '<oid>'.`       | `fatal: ambiguous argument '<oid>': …`   |
//! | `grep hello <oid>`            | `fatal: unable to parse object: <oid>`         | `fatal: ambiguous argument '<oid>': …`   |
//! | `grep hello <oid> --`         | `fatal: unable to parse object: <oid>`         | `fatal: unable to resolve revision: <oid>` |
//!
//! The two wordings are not interchangeable and neither is a variant of the
//! other: `reset`'s comes from `die(_("Could not parse object '%s'."), rev)` in
//! `cmd_reset()` (`builtin/reset.c`), reached when `lookup_commit_reference()` or
//! `repo_parse_tree_indirect()` returns NULL for an id `repo_get_oid_committish()`
//! / `repo_get_oid_treeish()` already accepted. `grep`'s comes from
//! `parse_object_or_die()` (`object.c`), which `cmd_grep()` calls on the id the
//! moment `get_oid_with_context()` hands it over.
//!
//! Resolving with gitoxide's `rev_parse_single()` alone reaches neither, because
//! it goes through the odb and so rejects the name outright — the token then
//! looks like a pathspec (`reset`) or like an unresolvable revision (`grep`).
//!
//! Three properties are pinned per command, since the fix is a *rule* about name
//! shape and any one of them alone can be satisfied by the wrong code:
//!
//! * the absent-but-well-formed id gets the "resolved, object missing" message;
//! * a name that resolves to nothing keeps the "no such name" message — the two
//!   outcomes git distinguishes must stay distinct, in both directions;
//! * the rule is length-exact and case-insensitive — 39 and 41 hex digits fall
//!   through to the ordinary parser (`get_oid_hex()` never sees them), while
//!   upper-case 40 does not, because `hexval()` accepts either case.
//!
//! `reset` additionally asserts the repository is untouched: `--hard` dies before
//! `unpack_trees()` runs, so a failed one must not have reset anything.
//!
//! The same `die(_("Could not parse object '%s'."), rev)` is reached from two
//! arms of `cmd_reset()` that do not agree on what the operand may be, so the
//! rest of the reset tests here pin the arms apart: the pathspec arm takes a
//! *tree* (`repo_parse_tree_indirect()`) and accepts `HEAD^{tree}` where the
//! whole-tree arm (`lookup_commit_reference()`) refuses it, the refusal names
//! the **operand** id and the **peeled** type — checked with an annotated tag on
//! a tree, where those two ids differ — and the pathspec arm fails *silently*
//! because its peel is handed a NULL name, so one blob operand produces two
//! stderr lines in one arm and one in the other.
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

/// The same id in upper case. `get_oid_hex()` is built on `hexval()`, which is
/// case-insensitive, so this must take exactly the same path as [`ABSENT`].
const ABSENT_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

/// One hex digit short of `hexsz`: the first branch does not apply and the name
/// is handled as an (ambiguous, unmatchable) abbreviation instead.
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
    /// One commit, plus a staged-and-modified `f.txt` so a `--hard` that wrongly
    /// went through would be visible in the worktree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-objname-resetgrep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("f.txt"), "hello\n").unwrap();
        std::fs::write(f.repo.join("g.txt"), "pattern here\n").unwrap();
        f.ok(&["add", "f.txt", "g.txt"]);
        f.ok(&["commit", "-q", "-m", "c1"]);
        std::fs::write(f.repo.join("f.txt"), "hello\nstaged change\n").unwrap();
        f.ok(&["add", "f.txt"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
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

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).0
    }
}

/// `cmd_reset()`'s whole-tree form: `repo_get_oid_committish()` accepts the id,
/// `lookup_commit_reference()` then finds nothing to look up.
///
/// `--hard` and `--soft` are both checked because they diverge earlier in
/// `cmd_reset()` (`setup_work_tree()` runs for one and not the other) and only
/// rejoin at the resolution this is about.
#[test]
fn reset_whole_tree_reports_the_absent_object() {
    let f = Fixture::new("reset-whole");
    let want = format!("fatal: Could not parse object '{ABSENT}'.\n");

    for mode in ["--hard", "--soft", "--mixed"] {
        let (out, err, code) = f.run(&["reset", mode, ABSENT]);
        assert_eq!(err, want, "reset {mode}");
        assert_eq!(out, "", "reset {mode}");
        assert_eq!(code, 128, "reset {mode}");
    }

    // No mode at all: the same path, reached through the MIXED default.
    let (out, err, code) = f.run(&["reset", ABSENT]);
    assert_eq!(err, want);
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// The pathspec form takes the *other* branch of `cmd_reset()` —
/// `repo_get_oid_treeish()` then `repo_parse_tree_indirect()` — which happens to
/// die with the same words. Both spellings of it are checked, since `--` and a
/// bare trailing path reach `parse_args()` through different arms.
#[test]
fn reset_with_paths_reports_the_absent_object() {
    let f = Fixture::new("reset-paths");
    let want = format!("fatal: Could not parse object '{ABSENT}'.\n");

    for args in [
        vec!["reset", ABSENT, "--", "f.txt"],
        vec!["reset", ABSENT, "f.txt"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, want, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }
}

/// The other direction: a name that resolves to nothing must keep the diagnostic
/// it has always had. Without this the "fix" could be a blanket rewording.
///
/// The three wordings are all `reset`'s, picked by where the name stood:
/// `parse_args()` falls through to `verify_filename()` when there is no `--`,
/// while a name before a `--` is never probed there and dies at the explicit
/// lookup in `cmd_reset()` — as a revision or as a tree depending on whether any
/// pathspec followed.
#[test]
fn reset_keeps_the_unresolvable_diagnostics() {
    let f = Fixture::new("reset-unresolvable");

    let (out, err, code) = f.run(&["reset", "--hard", UNRESOLVABLE]);
    assert_eq!(err, ambiguous(UNRESOLVABLE));
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let (out, err, code) = f.run(&["reset", UNRESOLVABLE, "--"]);
    assert_eq!(err, format!("fatal: Failed to resolve '{UNRESOLVABLE}' as a valid revision.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let (out, err, code) = f.run(&["reset", UNRESOLVABLE, "--", "f.txt"]);
    assert_eq!(err, format!("fatal: Failed to resolve '{UNRESOLVABLE}' as a valid tree.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// The rule is `len == hexsz`, not "looks like hex": 39 and 41 digits are ordinary
/// names that resolve to nothing, while 40 upper-case digits are an object id.
#[test]
fn reset_applies_the_rule_by_length_and_ignores_case() {
    let f = Fixture::new("reset-shape");

    for near in [SHORT_HEX, LONG_HEX] {
        let (out, err, code) = f.run(&["reset", "--hard", near]);
        assert_eq!(err, ambiguous(near), "{near} must not be read as an object id");
        assert_eq!(out, "", "{near}");
        assert_eq!(code, 128, "{near}");
    }

    let (out, err, code) = f.run(&["reset", "--hard", ABSENT_UPPER]);
    assert_eq!(err, format!("fatal: Could not parse object '{ABSENT_UPPER}'.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// The two arms of `cmd_reset()`'s resolution do **not** want the same kind of
/// object, and a present-but-wrong-kind operand is where that shows:
///
/// ```c
/// } else if (!pathspec.nr && !patch_mode) {
///         commit = lookup_commit_reference(the_repository, &oid);
///         if (!commit)
///                 die(_("Could not parse object '%s'."), rev);
/// } else {
///         tree = repo_parse_tree_indirect(the_repository, &oid);
///         if (!tree)
///                 die(_("Could not parse object '%s'."), rev);
/// }
/// ```
///
/// `git reset <tree> -- <path>` is the documented way to load paths out of a
/// tree, so the pathspec arm has to accept one; peeling to a commit in both arms
/// turns the working form into a failure. Every mode of the whole-tree form is
/// checked because they diverge earlier in `cmd_reset()` and only rejoin here.
#[test]
fn a_tree_operand_is_an_error_only_in_the_whole_tree_arm() {
    let f = Fixture::new("reset-tree-arm");
    let tree = f.run(&["rev-parse", "HEAD^{tree}"]).0.trim().to_string();
    let head_f = f.run(&["rev-parse", "HEAD:f.txt"]).0.trim().to_string();

    for mode in ["--hard", "--soft", "--mixed"] {
        let (out, err, code) = f.run(&["reset", mode, "HEAD^{tree}"]);
        assert_eq!(
            err,
            format!("error: object {tree} is a tree, not a commit\nfatal: Could not parse object 'HEAD^{{tree}}'.\n"),
            "reset {mode} <tree>"
        );
        assert_eq!(out, "", "reset {mode} <tree>");
        assert_eq!(code, 128, "reset {mode} <tree>");
    }

    // Same operand, one pathspec later: accepted, and the index entry really is
    // the tree's version — a message-only fix would pass the first half.
    let (out, err, code) = f.run(&["reset", "HEAD^{tree}", "--", "f.txt"]);
    assert_eq!(err, "");
    assert_eq!(out, "Unstaged changes after reset:\nM\tf.txt\n");
    assert_eq!(code, 0);
    assert_eq!(f.run(&["rev-parse", ":f.txt"]).0.trim(), head_f);
}

/// `lookup_commit_reference()` names the id it was *handed* and the type it
/// *arrived at*:
///
/// ```c
/// error(_("object %s is a %s, not a %s"),
///       oid_to_hex(oid), type_name(type), type_name(OBJ_COMMIT));
/// ```
///
/// A bare tree makes those two ids coincide and hides a mix-up, so the operand
/// here is an annotated tag pointing at a tree: git prints the *tag's* id next
/// to the word `tree`. The test asserts the two ids differ first, so it cannot
/// pass by accident on a build where they happen to be equal.
#[test]
fn the_type_error_names_the_operand_id_and_the_peeled_type() {
    let f = Fixture::new("reset-tagged-tree");
    let tree = f.run(&["rev-parse", "HEAD^{tree}"]).0.trim().to_string();
    f.ok(&["tag", "-a", "-m", "t", "treetag", &tree]);
    let tag = f.run(&["rev-parse", "treetag"]).0.trim().to_string();
    assert_ne!(tag, tree, "the fixture must make operand and peeled ids distinguishable");

    let (out, err, code) = f.run(&["reset", "treetag"]);
    assert_eq!(
        err,
        format!("error: object {tag} is a tree, not a commit\nfatal: Could not parse object 'treetag'.\n")
    );
    assert_eq!(out, "");
    assert_eq!(code, 128);

    // The pathspec arm peels the same tag all the way to the tree and succeeds.
    let (out, err, code) = f.run(&["reset", "treetag", "--", "f.txt"]);
    assert_eq!(err, "");
    assert_eq!(out, "Unstaged changes after reset:\nM\tf.txt\n");
    assert_eq!(code, 0);
}

/// The two arms also differ in how *loudly* they fail. `repo_parse_tree_indirect()`
/// is `repo_peel_to_type(r, NULL, 0, obj, OBJ_TREE)` — a NULL name, so
/// `repo_peel_to_type()` skips its `error()` and returns NULL silently, leaving
/// `cmd_reset()`'s `die()` as the only line. The whole-tree arm's
/// `lookup_commit_reference()` has no such guard and reports first.
///
/// So the same blob operand is two lines in one arm and one line in the other,
/// and an implementation that routes both through one helper gets one of them
/// wrong whichever helper it picks.
#[test]
fn a_blob_operand_is_reported_once_with_paths_and_twice_without() {
    let f = Fixture::new("reset-blob-arm");
    let blob = f.run(&["rev-parse", "HEAD:g.txt"]).0.trim().to_string();

    let (out, err, code) = f.run(&["reset", &blob]);
    assert_eq!(
        err,
        format!("error: object {blob} is a blob, not a commit\nfatal: Could not parse object '{blob}'.\n")
    );
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let (out, err, code) = f.run(&["reset", &blob, "--", "f.txt"]);
    assert_eq!(err, format!("fatal: Could not parse object '{blob}'.\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// A `--hard` that dies at name resolution has not reset anything: the die comes
/// long before `unpack_trees()`. Worth its own assertion because the message and
/// the damage are independent — an implementation could print the right words
/// after having already clobbered the worktree.
#[test]
fn a_failed_reset_leaves_the_repository_alone() {
    let f = Fixture::new("reset-state");
    let head = f.head();
    let staged = f.run(&["diff", "--cached", "--name-only"]).0;
    assert_eq!(staged, "f.txt\n", "fixture must start with a staged change");

    let (_, err, code) = f.run(&["reset", "--hard", ABSENT]);
    assert_eq!(code, 128, "{err}");

    assert_eq!(f.head(), head, "HEAD moved");
    assert_eq!(
        std::fs::read_to_string(f.repo.join("f.txt")).unwrap(),
        "hello\nstaged change\n",
        "the worktree was reset despite the failure"
    );
    assert_eq!(f.run(&["diff", "--cached", "--name-only"]).0, staged, "the index was reset");
}

/// `cmd_grep()` calls `parse_object_or_die()` on the id the moment
/// `get_oid_with_context()` returns it — so this fires for an absent object
/// whatever the surrounding options are, and long before any pathspec handling.
#[test]
fn grep_reports_the_absent_object() {
    let f = Fixture::new("grep-absent");
    let want = format!("fatal: unable to parse object: {ABSENT}\n");

    for args in [
        vec!["grep", "hello", ABSENT],
        vec!["grep", "-e", "pattern", "--cached", ABSENT],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, want, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }
}

/// With a `--` present the token can only be a revision, and the "unresolvable"
/// die is a *different* one. Pinning both against the same trailing `--` is what
/// proves `parse_object_or_die()` runs first rather than the token quietly
/// falling through to the `unable to resolve revision` path.
#[test]
fn grep_before_a_dashdash_still_separates_the_two() {
    let f = Fixture::new("grep-dashdash");

    let (out, err, code) = f.run(&["grep", "hello", ABSENT, "--"]);
    assert_eq!(err, format!("fatal: unable to parse object: {ABSENT}\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let (out, err, code) = f.run(&["grep", "-e", "pattern", UNRESOLVABLE, "--"]);
    assert_eq!(err, format!("fatal: unable to resolve revision: {UNRESOLVABLE}\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// Without a `--`, an unresolvable token is a pathspec that is not there, and
/// `verify_filename()` diagnoses it as a possibly-misspelt revision. Unchanged by
/// the fix, and the control that keeps the absent-object message from spreading.
#[test]
fn grep_keeps_the_unresolvable_diagnostic() {
    let f = Fixture::new("grep-unresolvable");

    let (out, err, code) = f.run(&["grep", "hello", UNRESOLVABLE]);
    assert_eq!(err, ambiguous(UNRESOLVABLE));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// Same length-exact, case-insensitive rule as `reset`, checked independently:
/// the two commands resolve through different call sites and one could easily be
/// fixed with a looser test than the other.
#[test]
fn grep_applies_the_rule_by_length_and_ignores_case() {
    let f = Fixture::new("grep-shape");

    for near in [SHORT_HEX, LONG_HEX] {
        let (out, err, code) = f.run(&["grep", "hello", near]);
        assert_eq!(err, ambiguous(near), "{near} must not be read as an object id");
        assert_eq!(out, "", "{near}");
        assert_eq!(code, 128, "{near}");
    }

    let (out, err, code) = f.run(&["grep", "hello", ABSENT_UPPER]);
    assert_eq!(err, format!("fatal: unable to parse object: {ABSENT_UPPER}\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);
}

/// Names that *are* present must still work — the resolver change sits on the
/// path every ordinary invocation takes, so a regression there would be silent
/// in every test above.
#[test]
fn present_objects_are_unaffected() {
    let f = Fixture::new("present");

    let (out, err, code) = f.run(&["grep", "hello", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "HEAD:f.txt:hello\n");

    let head = f.head();
    let (_, err, code) = f.run(&["reset", "--hard", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(f.head(), head);
    assert_eq!(std::fs::read_to_string(f.repo.join("f.txt")).unwrap(), "hello\n");
}
