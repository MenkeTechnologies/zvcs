//! `get_oid_basic()`'s full-length-hex rule in `commit-tree` and `merge-base`.
//!
//! `get_oid_basic()` (`object-name.c`) opens with
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) { … return 0; }
//! ```
//!
//! — a name of exactly `hexsz` hex digits *is* the object id, decoded and
//! returned **without the object database ever being consulted**. So a
//! well-formed id of an object the repository does not have is a name that
//! *resolved*; whether the object exists is a later, separate question, asked by
//! a different piece of code and answered with a different message.
//!
//! Resolving only through gitoxide's `rev_parse_single()` collapses the two,
//! because it goes through the odb and fails outright. Both commands below then
//! print the "name did not resolve" message where git prints the "object is
//! missing" one. The two messages are not even the same shape:
//!
//! | command       | name does not resolve            | resolves, object absent          |
//! |---------------|----------------------------------|----------------------------------|
//! | `commit-tree` | `not a valid object name <spec>` | `<oid> is not a valid object`    |
//! | `merge-base`  | `Not a valid object name <spec>` | `Not a valid commit name <spec>` |
//!
//! and they disagree about *what* they name: `commit-tree`'s absent-object
//! message comes from `odb_assert_oid_type()` (`odb.c:977`), which prints
//! `oid_to_hex(oid)` and is therefore always lowercase, while `merge-base`'s
//! comes from `get_commit_reference()` (`builtin/merge-base.c:46`), which prints
//! `arg` as the user typed it. An uppercase id proves both at once, and is the
//! test that fails first if either call site starts formatting the wrong thing.
//!
//! Every expectation here was captured from stock git 2.55.0 in an identical
//! fixture. Each absent-object case is paired with an unresolvable *control*
//! (`nosuchthing`), because a fix that reported the absent-object message for
//! everything would be just as wrong and would pass an unpaired test.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed 40-hex object id that no repository built here can contain.
const ABSENT: &str = "0123456789012345678901234567890123456789";

/// The same, with hex *letters* in upper case: `get_oid_hex()` is
/// case-insensitive, so this resolves exactly as `ABSENT` does.
const ABSENT_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// stdin is `/dev/null`: `commit-tree` with neither `-m` nor `-F` reads the
/// message from stdin, and an inherited stdin would make the test hang or, worse,
/// swallow the harness's own.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) -> String {
    let (stdout, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
    stdout.trim_end().to_string()
}

/// Two commits on `main`, so `HEAD`, `HEAD~1`, a tree and a blob all exist and
/// `merge-base HEAD HEAD~1` has a real answer to give.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "hello\n").unwrap();
    must(&repo, &home, &["add", "f.txt"]);
    must(&repo, &home, &["commit", "-qm", "one"]);
    std::fs::write(repo.join("g.txt"), "second\n").unwrap();
    must(&repo, &home, &["add", "g.txt"]);
    must(&repo, &home, &["commit", "-qm", "two"]);

    (root, repo, home)
}

/// `commit-tree`: an absent-but-well-formed tree id gets past
/// `repo_get_oid_tree()` and dies in `odb_assert_oid_type()` instead
/// (`odb.c:981`, `die(_("%s is not a valid object"), oid_to_hex(oid))`).
///
/// The control is the whole point: `nosuchthing` never resolves, so it must keep
/// the *other* message, lower-case `not a valid object name` with the spec last.
#[test]
fn commit_tree_absent_full_hex_tree_is_not_a_valid_object() {
    let (root, repo, home) = fixture("ct-tree");

    let (out, err, code) = git(&repo, &home, &["commit-tree", ABSENT]);
    assert_eq!(err, format!("fatal: {ABSENT} is not a valid object\n"));
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let (out, err, code) = git(&repo, &home, &["commit-tree", "nosuchthing"]);
    assert_eq!(err, "fatal: not a valid object name nosuchthing\n");
    assert_eq!(out, "");
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// The same for `-p`: `parse_parent_arg_callback()` runs `repo_get_oid_commit()`
/// then `odb_assert_oid_type(&oid, OBJ_COMMIT)` (`builtin/commit-tree.c:49-52`),
/// so the parent reports it during option parsing — before the `argc != 1` check,
/// which is why the third case dies about the parent and not about the tree.
#[test]
fn commit_tree_absent_full_hex_parent_is_not_a_valid_object() {
    let (root, repo, home) = fixture("ct-parent");
    let tree = must(&repo, &home, &["rev-parse", "HEAD^{tree}"]);

    let (_, err, code) = git(&repo, &home, &["commit-tree", "-p", ABSENT, &tree]);
    assert_eq!(err, format!("fatal: {ABSENT} is not a valid object\n"));
    assert_eq!(code, 128);

    let (_, err, code) = git(&repo, &home, &["commit-tree", "-p", "nosuchthing", &tree]);
    assert_eq!(err, "fatal: not a valid object name nosuchthing\n");
    assert_eq!(code, 128);

    let (_, err, code) = git(&repo, &home, &["commit-tree", "-p", ABSENT]);
    assert_eq!(err, format!("fatal: {ABSENT} is not a valid object\n"));
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// `odb_assert_oid_type()` prints `oid_to_hex(oid)`, never the spec — so an
/// uppercase id comes back lower-cased, and an *abbreviation* comes back full.
///
/// Echoing the spec would look right in every all-lowercase test and be wrong
/// here, which is exactly why this case exists.
#[test]
fn commit_tree_absent_object_message_names_canonical_hex() {
    let (root, repo, home) = fixture("ct-hex");

    let (_, err, code) = git(&repo, &home, &["commit-tree", ABSENT_UPPER]);
    assert_eq!(
        err,
        format!("fatal: {} is not a valid object\n", ABSENT_UPPER.to_lowercase())
    );
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// The object *existing* with the wrong type stays on its own branch of
/// `odb_assert_oid_type()` — `%s is not a valid '%s' object` — and must not be
/// swallowed by the new missing-object branch above it.
#[test]
fn commit_tree_present_object_of_wrong_type_still_names_the_type() {
    let (root, repo, home) = fixture("ct-kind");
    let blob = must(&repo, &home, &["rev-parse", "HEAD:f.txt"]);
    let tree = must(&repo, &home, &["rev-parse", "HEAD^{tree}"]);

    let (_, err, code) = git(&repo, &home, &["commit-tree", &blob]);
    assert_eq!(err, format!("fatal: {blob} is not a valid 'tree' object\n"));
    assert_eq!(code, 128);

    let (_, err, code) = git(&repo, &home, &["commit-tree", "-p", &tree, &tree]);
    assert_eq!(err, format!("fatal: {tree} is not a valid 'commit' object\n"));
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// The tree is a *non-option* argument: `parse_options()` only collects it, and
/// both the `argc != 1` check and `repo_get_oid_tree()` run after the whole
/// command line is parsed (`builtin/commit-tree.c:134-140`).
///
/// Resolving the tree eagerly, where it appears, would make an absent tree
/// outrank a later bad option and outrank the arity check — so this pins the
/// order the missing-object message must *not* jump ahead of.
#[test]
fn commit_tree_bad_option_and_arity_outrank_the_absent_tree() {
    let (root, repo, home) = fixture("ct-order");

    let (_, err, code) = git(&repo, &home, &["commit-tree", ABSENT, "--bogus"]);
    assert!(
        err.starts_with("error: unknown option `bogus'\n"),
        "expected the option error first, got: {err}"
    );
    assert!(!err.contains("is not a valid object"), "tree reported too early: {err}");
    assert_eq!(code, 129);

    let (_, err, code) = git(&repo, &home, &["commit-tree", ABSENT, ABSENT]);
    assert_eq!(err, "fatal: must give exactly one tree\n");
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// `merge-base`: `repo_get_oid()` accepts the absent id, then
/// `lookup_commit_reference()` cannot peel it, so `get_commit_reference()` dies
/// on its *second* `die` — `Not a valid commit name`, not `Not a valid object
/// name` (`builtin/merge-base.c:51-55`).
///
/// Every mode routes through the same function, so all four are pinned; each is
/// paired with its unresolvable control.
#[test]
fn merge_base_absent_full_hex_is_not_a_valid_commit_name() {
    let (root, repo, home) = fixture("mb-modes");
    let absent = format!("fatal: Not a valid commit name {ABSENT}\n");
    let control = "fatal: Not a valid object name nosuchthing\n";

    for args in [
        vec!["merge-base", ABSENT, "HEAD"],
        vec!["merge-base", "HEAD", ABSENT],
        vec!["merge-base", "--all", ABSENT, "HEAD"],
        vec!["merge-base", "--is-ancestor", ABSENT, "HEAD"],
        vec!["merge-base", "--is-ancestor", "HEAD", ABSENT],
        vec!["merge-base", "--octopus", ABSENT, "HEAD"],
        vec!["merge-base", "--independent", ABSENT, "HEAD"],
    ] {
        let (out, err, code) = git(&repo, &home, &args);
        assert_eq!(err, absent, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    for args in [
        vec!["merge-base", "nosuchthing", "HEAD"],
        vec!["merge-base", "HEAD", "nosuchthing"],
        vec!["merge-base", "--is-ancestor", "nosuchthing", "HEAD"],
        vec!["merge-base", "--octopus", "nosuchthing", "HEAD"],
        vec!["merge-base", "--independent", "nosuchthing", "HEAD"],
    ] {
        let (_, err, code) = git(&repo, &home, &args);
        assert_eq!(err, control, "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    // The working case must stay working: this is what tells a fix that turned
    // every resolution into a failure apart from one that only split the errors.
    let head = must(&repo, &home, &["rev-parse", "HEAD~1"]);
    let (out, _, code) = git(&repo, &home, &["merge-base", "HEAD", "HEAD~1"]);
    assert_eq!(out, format!("{head}\n"));
    assert_eq!(code, 0);

    let _ = std::fs::remove_dir_all(&root);
}

/// `get_commit_reference()` prints `arg`, not `oid_to_hex()` — the opposite of
/// `commit-tree`'s absent-object message, which is why the two are tested
/// against the same uppercase id.
#[test]
fn merge_base_absent_object_message_names_the_spec_as_typed() {
    let (root, repo, home) = fixture("mb-hex");

    let (_, err, code) = git(&repo, &home, &["merge-base", ABSENT_UPPER, "HEAD"]);
    assert_eq!(err, format!("fatal: Not a valid commit name {ABSENT_UPPER}\n"));
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// Two annotated tags on top of [`fixture`]: one on the root tree, one on the
/// blob. Both are objects the tag *and* its target are non-commits, and the two
/// ids differ — which is what makes them able to tell an operand id apart from a
/// peeled type. Returns `(blob, tree, treetag id, blobtag id)`.
fn with_non_commit_tags(repo: &Path, home: &Path) -> (String, String, String, String) {
    let blob = must(repo, home, &["rev-parse", "HEAD:f.txt"]);
    let tree = must(repo, home, &["rev-parse", "HEAD^{tree}"]);
    must(repo, home, &["tag", "-a", "-m", "t", "treetag", &tree]);
    must(repo, home, &["tag", "-a", "-m", "b", "blobtag", &blob]);
    let treetag = must(repo, home, &["rev-parse", "treetag"]);
    let blobtag = must(repo, home, &["rev-parse", "blobtag"]);
    (blob, tree, treetag, blobtag)
}

/// `lookup_commit_reference()` is `lookup_commit_reference_gently(r, oid, 0)`,
/// so `quiet` is 0 and a *present* object of the wrong type is announced before
/// `get_commit_reference()` dies (`commit.c:61-67`):
///
/// ```c
/// if (type != OBJ_COMMIT) {
///         if (!quiet)
///                 error(_("object %s is a %s, not a %s"),
///                       oid_to_hex(oid), type_name(type), type_name(OBJ_COMMIT));
/// ```
///
/// Two ids are in play and the C mixes them: `oid` is the **operand's** id, while
/// `type` is what `peel_object_ext()` *arrived at*. A bare blob or tree hides the
/// mix-up because the two coincide, so the annotated tags carry the test: `treetag`
/// must report the **tag object's** id together with the word **tree**, and the
/// `fatal:` under it must still name the spec as typed.
///
/// An absent id is the control — `peel_object_ext()` takes its `default:` arm,
/// which returns NULL in silence, so the `fatal:` must arrive with no `error:`
/// ahead of it.
#[test]
fn merge_base_wrong_type_operand_is_announced_before_the_fatal() {
    let (root, repo, home) = fixture("mb-type");
    let (blob, tree, treetag, blobtag) = with_non_commit_tags(&repo, &home);

    for (spec, id, kind) in [
        (blob.as_str(), blob.as_str(), "blob"),
        (tree.as_str(), tree.as_str(), "tree"),
        ("treetag", treetag.as_str(), "tree"),
        ("blobtag", blobtag.as_str(), "blob"),
    ] {
        let want = format!(
            "error: object {id} is a {kind}, not a commit\nfatal: Not a valid commit name {spec}\n"
        );
        for args in [
            vec!["merge-base", spec, "HEAD"],
            vec!["merge-base", "HEAD", spec],
            vec!["merge-base", "--all", spec, "HEAD"],
            vec!["merge-base", "--is-ancestor", spec, "HEAD"],
            vec!["merge-base", "--is-ancestor", "HEAD", spec],
            vec!["merge-base", "--octopus", spec, "HEAD"],
            vec!["merge-base", "--octopus", "HEAD", spec],
            vec!["merge-base", "--independent", spec, "HEAD"],
            vec!["merge-base", "--independent", "HEAD", spec],
        ] {
            let (out, err, code) = git(&repo, &home, &args);
            assert_eq!(err, want, "{args:?}");
            assert_eq!(out, "", "{args:?}");
            assert_eq!(code, 128, "{args:?}");
        }
    }

    // Control: absent, not wrong-typed — the `fatal:` stands alone.
    let (_, err, code) = git(&repo, &home, &["merge-base", ABSENT, "HEAD"]);
    assert_eq!(err, format!("fatal: Not a valid commit name {ABSENT}\n"));
    assert_eq!(code, 128);

    // Control: a name that never resolves reaches the *first* `die`, still alone.
    let (_, err, code) = git(&repo, &home, &["merge-base", "--octopus", "nosuchthing", "HEAD"]);
    assert_eq!(err, "fatal: Not a valid object name nosuchthing\n");
    assert_eq!(code, 128);

    let _ = std::fs::remove_dir_all(&root);
}

/// `--octopus` and `--independent` build their `commit_list` back to front
/// (`builtin/merge-base.c:65-66` and `:86-87`):
///
/// ```c
/// for (i = count - 1; i >= 0; i--)
///         commit_list_insert(get_commit_reference(args[i]), &revs);
/// ```
///
/// `commit_list_insert()` prepends, so the assembled list still comes out in
/// input order and a *successful* run is indistinguishable — the direction is
/// observable only through which bad rev dies first. Every other mode walks argv
/// forward, so the same two revs must produce opposite diagnostics under the two
/// families; a single-bad-rev case would pass either way and prove nothing.
#[test]
fn merge_base_octopus_and_independent_resolve_their_revs_last_first() {
    let (root, repo, home) = fixture("mb-order");
    let (blob, tree, treetag, blobtag) = with_non_commit_tags(&repo, &home);
    let absent = format!("fatal: Not a valid commit name {ABSENT}\n");
    let unresolvable = "fatal: Not a valid object name nosuchthing\n".to_string();

    // Both revs bad, in both orders: forward modes name the left one, the two
    // reversed modes name the right one.
    for (mode, first, second) in [
        (None, absent.clone(), unresolvable.clone()),
        (Some("--all"), absent.clone(), unresolvable.clone()),
        (Some("--is-ancestor"), absent.clone(), unresolvable.clone()),
        (Some("--octopus"), unresolvable.clone(), absent.clone()),
        (Some("--independent"), unresolvable.clone(), absent.clone()),
    ] {
        let mut args = vec!["merge-base"];
        args.extend(mode);
        let (lhs, rhs) = (args.len(), args.len() + 1);
        args.extend([ABSENT, "nosuchthing"]);

        let (out, err, code) = git(&repo, &home, &args);
        assert_eq!(err, first, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");

        args.swap(lhs, rhs);
        let (_, err, code) = git(&repo, &home, &args);
        assert_eq!(err, second, "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    // The `error:` note travels with whichever rev was picked, so the reversed
    // modes report the *tag on the blob* where the forward ones report the tree.
    let tree_note =
        format!("error: object {treetag} is a tree, not a commit\nfatal: Not a valid commit name treetag\n");
    let blob_note =
        format!("error: object {blobtag} is a blob, not a commit\nfatal: Not a valid commit name blobtag\n");
    for (mode, want) in [
        (None, &tree_note),
        (Some("--all"), &tree_note),
        (Some("--is-ancestor"), &tree_note),
        (Some("--octopus"), &blob_note),
        (Some("--independent"), &blob_note),
    ] {
        let mut args = vec!["merge-base"];
        args.extend(mode);
        args.extend(["treetag", "blobtag"]);
        let (_, err, code) = git(&repo, &home, &args);
        assert_eq!(&err, want, "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    // Three revs: the reversed modes reach the *last* one whatever sits in front
    // of it, while the default mode still stops at the first.
    for mode in ["--octopus", "--independent"] {
        for args in [
            vec!["merge-base", mode, "HEAD", ABSENT, "nosuchthing"],
            vec!["merge-base", mode, ABSENT, "HEAD", "nosuchthing"],
        ] {
            let (_, err, code) = git(&repo, &home, &args);
            assert_eq!(err, unresolvable, "{args:?}");
            assert_eq!(code, 128, "{args:?}");
        }
        let args = vec!["merge-base", mode, "nosuchthing", "HEAD", ABSENT];
        let (_, err, code) = git(&repo, &home, &args);
        assert_eq!(err, absent, "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }
    let (_, err, code) = git(&repo, &home, &["merge-base", ABSENT, "HEAD", "nosuchthing"]);
    assert_eq!(err, absent);
    assert_eq!(code, 128);

    // Controls: with every rev good, the direction is invisible and all three
    // modes must still answer normally — a fix that reversed the *result* rather
    // than the resolution order fails here and nowhere else.
    let head = must(&repo, &home, &["rev-parse", "HEAD"]);
    let base = must(&repo, &home, &["rev-parse", "HEAD~1"]);
    for (args, want) in [
        (vec!["merge-base", "HEAD", "HEAD~1"], format!("{base}\n")),
        (vec!["merge-base", "--octopus", "HEAD", "HEAD~1"], format!("{base}\n")),
        (vec!["merge-base", "--independent", "HEAD", "HEAD~1"], format!("{head}\n")),
    ] {
        let (out, err, code) = git(&repo, &home, &args);
        assert_eq!(out, want, "{args:?}");
        assert_eq!(err, "", "{args:?}");
        assert_eq!(code, 0, "{args:?}");
    }

    // Unused here, but resolving them proves the fixture really holds a bare blob
    // and tree distinct from the tag ids above.
    assert_ne!(blob, blobtag);
    assert_ne!(tree, treetag);

    let _ = std::fs::remove_dir_all(&root);
}

/// `handle_fork_point()` checks the rev with `repo_get_oid()` *before*
/// `get_fork_point()` looks the ref up, and only that check is fatal
/// (`builtin/merge-base.c:128-147`).
///
/// So an absent full-hex rev leaves `derived` NULL, finds no fork point, and
/// exits 1 in silence — while an unresolvable rev still dies, and dies ahead of
/// a bad ref given alongside it.
#[test]
fn merge_base_fork_point_absent_rev_is_a_silent_exit_one() {
    let (root, repo, home) = fixture("mb-fork");

    let (out, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "main", ABSENT]);
    assert_eq!(err, "");
    assert_eq!(out, "");
    assert_eq!(code, 1);

    let (_, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "main", "nosuchrev"]);
    assert_eq!(err, "fatal: Not a valid object name: 'nosuchrev'\n");
    assert_eq!(code, 128);

    // Bad rev beats bad ref: `repo_get_oid()` runs first.
    let (_, err, code) = git(
        &repo,
        &home,
        &["merge-base", "--fork-point", "nosuchref", "nosuchrev"],
    );
    assert_eq!(err, "fatal: Not a valid object name: 'nosuchrev'\n");
    assert_eq!(code, 128);

    // …but a rev that *does* resolve leaves the ref check to report.
    for rev in ["HEAD", ABSENT] {
        let (_, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "nosuchref", rev]);
        assert_eq!(err, "fatal: No such ref: 'nosuchref'\n", "rev {rev}");
        assert_eq!(code, 128, "rev {rev}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `--fork-point`'s two non-fatal complaints, which are easy to lose because
/// neither changes the exit status on its own.
///
/// `lookup_commit_reference()` runs at `builtin/merge-base.c:138`, *before*
/// `get_fork_point()` looks the ref up on `:140` — and it is not quiet, so a rev
/// naming a present non-commit prints `object %s is a %s, not a %s` and then the
/// command goes on to exit 1 in silence, or to die on the ref with the note
/// already above it. Checking the ref first would swap those two lines.
///
/// The ref itself is the other one: with no reflog to walk, `get_fork_point()`
/// falls back to `add_one_commit()` on the id `repo_dwim_ref()` returned, which
/// is **not** peeled — so a ref naming an annotated tag contributes no candidate
/// and `repo_parse_commit()` says so (`commit.c:646-649`, `error("Object %s not
/// a commit")`, capital `O` and no type named, a different message from the one
/// above). Peeling the fallback instead would find the tagged commit, print a
/// fork point and exit 0.
#[test]
fn merge_base_fork_point_reports_non_commits_without_dying() {
    let (root, repo, home) = fixture("mb-fork-type");
    let (blob, tree, treetag, blobtag) = with_non_commit_tags(&repo, &home);

    for (spec, id, kind) in [
        (blob.as_str(), blob.as_str(), "blob"),
        (tree.as_str(), tree.as_str(), "tree"),
        ("treetag", treetag.as_str(), "tree"),
        ("blobtag", blobtag.as_str(), "blob"),
    ] {
        let note = format!("error: object {id} is a {kind}, not a commit\n");

        let (out, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "main", spec]);
        assert_eq!(err, note, "rev {spec}");
        assert_eq!(out, "", "rev {spec}");
        assert_eq!(code, 1, "rev {spec}");

        // The note is printed on the way to the ref check, not instead of it.
        let (_, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "nosuchref", spec]);
        assert_eq!(err, format!("{note}fatal: No such ref: 'nosuchref'\n"), "rev {spec}");
        assert_eq!(code, 128, "rev {spec}");
    }

    // A tag ref as the *reflog* argument: no reflog, unpeeled fallback, no
    // candidate — and the parse-time wording, which is not the one above.
    for (refname, id) in [("treetag", &treetag), ("blobtag", &blobtag)] {
        let (out, err, code) = git(&repo, &home, &["merge-base", "--fork-point", refname, "main"]);
        assert_eq!(err, format!("error: Object {id} not a commit\n"), "ref {refname}");
        assert_eq!(out, "", "ref {refname}");
        assert_eq!(code, 1, "ref {refname}");
    }

    // Controls: a branch ref and a commit rev still find their fork point, so
    // none of the above can be a blanket "report and give up".
    let head = must(&repo, &home, &["rev-parse", "HEAD"]);
    let base = must(&repo, &home, &["rev-parse", "HEAD~1"]);
    for (rev, want) in [("HEAD", &head), ("HEAD~1", &base)] {
        let (out, err, code) = git(&repo, &home, &["merge-base", "--fork-point", "main", rev]);
        assert_eq!(out, format!("{want}\n"), "rev {rev}");
        assert_eq!(err, "", "rev {rev}");
        assert_eq!(code, 0, "rev {rev}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
