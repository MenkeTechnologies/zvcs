//! Two things `git fast-export` decides before it writes a byte: which diff base
//! an *excluded* parent gets, and which of two competing argument errors is the
//! one the user sees.
//!
//! ### `--reference-excluded-parents` picks the diff base too
//!
//! `handle_commit` (`builtin/fast-export.c:743-753`) chooses between an
//! incremental and a root diff on one condition:
//!
//! ```c
//! if (commit->parents &&
//!     (get_object_mark(&commit->parents->item->object) != 0 ||
//!      reference_excluded_commits) &&
//!     !full_tree)
//!         diff_tree_oid(get_commit_tree_oid(commit->parents->item),
//!                       get_commit_tree_oid(commit), "", &rev->diffopt);
//! else
//!         diff_root_tree_oid(get_commit_tree_oid(commit), "", &rev->diffopt);
//! ```
//!
//! An unmarked first parent normally forces the root diff, because the importer
//! has never seen that commit and a delta against it would not apply. The option
//! is exactly the statement that it *has* seen it — the stanza names the parent by
//! raw object id instead of dropping it — so the same flag flips the choice back.
//! `handle_tags_and_duplicates` (`:1154-1174`) reads it the same way for a ref
//! whose commit was excluded: normally the null oid, meaning "delete this branch",
//! but under the option the commit's own id.
//!
//! ### `parse_options` runs to completion before `setup_revisions`
//!
//! `cmd_fast_export` (`:1380-1384`) is three ordered stages:
//!
//! ```c
//! argc = parse_options(argc, argv, prefix, options, fast_export_usage,
//!                 PARSE_OPT_KEEP_ARGV0 | PARSE_OPT_KEEP_UNKNOWN_OPT);
//! argc = setup_revisions(argc, argv, &revs, NULL);
//! if (argc > 1)
//!         usage_with_options (fast_export_usage, options);
//! ```
//!
//! so fast-export's own table is swept first over the whole command line and
//! reports its errors ahead of every rev-list option, diff option and revision
//! argument, wherever they sit relative to each other. `PARSE_OPT_KEEP_UNKNOWN_OPT`
//! is what defers an unknown option to stage 3 rather than rejecting it in stage 1,
//! and the absence of `PARSE_OPT_KEEP_DASHDASH` is what makes `--` end stage 1
//! entirely.
//!
//! Every expectation below was taken from stock git 2.55.0 in this fixture.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// The identity and both timestamps are pinned so the fixture's object ids are the
/// same on every machine and every run — the expectations below quote `main`'s id
/// literally, and the test reads it back out of the fixture to prove it.
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

/// Two commits on `main`, a lightweight and an annotated tag on its tip, and a
/// `feature` branch one commit ahead.
///
/// `feature`'s only commit has `main`'s tip as its first parent, so `--all ^main`
/// exports exactly one commit whose parent is excluded — the shape both halves of
/// `--reference-excluded-parents` are about. `README.md` and `src/` exist so the
/// pathspec cases have something real to name.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fx-optorder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# fixture\n").unwrap();
    must(&repo, &home, &["add", "README.md"]);
    must(&repo, &home, &["commit", "-qm", "initial commit"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n",
    )
    .unwrap();
    must(&repo, &home, &["add", "src/lib.rs"]);
    must(&repo, &home, &["commit", "-qm", "add two"]);
    must(&repo, &home, &["tag", "v0.1.0"]);
    must(&repo, &home, &["tag", "-a", "v0.2.0", "-m", "annotated"]);
    must(&repo, &home, &["branch", "feature"]);
    must(&repo, &home, &["checkout", "-q", "feature"]);
    std::fs::write(repo.join("feature.txt"), "feature work\n").unwrap();
    must(&repo, &home, &["add", "feature.txt"]);
    must(&repo, &home, &["commit", "-qm", "feature commit"]);
    must(&repo, &home, &["checkout", "-q", "main"]);

    (root, repo)
}

/// The full object id of `rev` in `repo`.
fn oid(repo: &Path, home: &Path, rev: &str) -> String {
    let (stdout, stderr, code) = git(repo, home, &["rev-parse", rev]);
    assert_eq!(code, 0, "rev-parse {rev}: {stderr}");
    stdout.trim().to_string()
}

/// The identity lines every commit stanza in this fixture carries.
const IDENT: &str = "author A U Thor <author@example.com> 1112904793 +0200\n\
                     committer C O Mitter <committer@example.com> 1112904793 +0200\n";

const NULL_OID: &str = "0000000000000000000000000000000000000000";

/// The option's whole job, in one stream: the excluded parent is *named*, so the
/// commit is a delta against it and the refs on it are restored rather than deleted.
///
/// `feature`'s commit adds `feature.txt` on top of `main`. Diffing against the
/// excluded parent's tree yields that one path, so one blob is exported; the root
/// diff the port used to take yields three, re-sending `README.md` and `src/lib.rs`
/// that the importer already has. Both trailing `reset`s carry `main`'s id for the
/// same reason: the option says those commits are present on the other side.
#[test]
fn an_excluded_parent_is_both_the_diff_base_and_the_reset_target() {
    let (root, repo) = fixture("refexcl");
    let home = root.join("home");
    let main = oid(&repo, &home, "main");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &[
            "fast-export",
            "--all",
            "^main",
            "--reference-excluded-parents",
            "--tag-of-filtered-object=drop",
        ],
    );

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        format!(
            "blob\nmark :1\ndata 13\nfeature work\n\n\
             commit refs/heads/feature\nmark :2\n{IDENT}\
             data 15\nfeature commit\n\
             from {main}\n\
             M 100644 :1 feature.txt\n\n\
             reset refs/tags/v0.1.0\nfrom {main}\n\n\
             reset refs/heads/main\nfrom {main}\n\n"
        )
    );
}

/// The control, so the previous test is not passing by accident: without the
/// option the *same* export re-sends every path and deletes both refs.
///
/// git drops the unmarked parent from the stanza entirely, which leaves the commit
/// a root — and a root has to carry its whole tree, because the importer is being
/// told nothing precedes it.
#[test]
fn without_the_option_the_same_export_is_a_root_commit_and_two_deletions() {
    let (root, repo) = fixture("refexcl-control");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "--all", "^main", "--tag-of-filtered-object=drop"],
    );

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "blob\nmark :1\ndata 10\n# fixture\n\n\
             blob\nmark :2\ndata 13\nfeature work\n\n\
             blob\nmark :3\ndata 52\n\
             pub fn one() -> u32 {{ 1 }}\npub fn two() -> u32 {{ 2 }}\n\n\
             commit refs/heads/feature\nmark :4\n{IDENT}\
             data 15\nfeature commit\n\
             M 100644 :1 README.md\n\
             M 100644 :2 feature.txt\n\
             M 100644 :3 src/lib.rs\n\n\
             reset refs/tags/v0.1.0\nfrom {NULL_OID}\n\n\
             reset refs/heads/main\nfrom {NULL_OID}\n\n"
        )
    );
}

/// `--anonymize` reaches only one of the two raw ids, and the asymmetry is real.
///
/// The `from` inside a commit stanza goes through `anonymize_oid`
/// (`fast-export.c:857-860`), which hands out `…0001`, `…0002` in first-mention
/// order — a raw id there would leak history out of an anonymized stream. The
/// trailing `reset` prints `oid_to_hex(&commit->object.oid)` with no such call
/// (`:1172`), so `main`'s real id appears in an otherwise anonymized export.
#[test]
fn anonymize_renames_the_parent_in_the_commit_but_not_in_the_reset() {
    let (root, repo) = fixture("refexcl-anon");
    let home = root.join("home");
    let main = oid(&repo, &home, "main");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &[
            "fast-export",
            "--all",
            "^main",
            "--reference-excluded-parents",
            "--anonymize",
            "--tag-of-filtered-object=drop",
        ],
    );

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!(
            "blob\nmark :1\ndata 16\nanonymous blob 0\n\
             commit refs/heads/ref0\nmark :2\n\
             author User 1 <user1@example.com> 1112904793 +0200\n\
             committer User 0 <user0@example.com> 1112904793 +0200\n\
             data 16\nsubject 0\n\nbody\n\
             from 0000000000000000000000000000000000000001\n\
             M 100644 :1 path0\n\n\
             reset refs/tags/ref1\nfrom {main}\n\n\
             reset refs/heads/ref2\nfrom {main}\n\n"
        )
    );
}

/// The reported case: an option fast-export owns beats one it does not, whatever
/// the order on the line.
///
/// `--max-count` belongs to `setup_revisions`, which does not run until
/// `parse_options` has returned, so the `--signed-tags` callback fails first even
/// though `--max-count=0x10` is the earlier argument. A parse-options callback that
/// returns non-zero exits 129 straight out of `parse_options_step`, so the `error:`
/// line stands alone with no option list behind it.
#[test]
fn fast_exports_own_option_is_rejected_before_a_rev_list_one() {
    let (root, repo) = fixture("precedence");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &[
            "fast-export",
            "--max-count=0x10",
            "--signed-tags=false",
            "feature..main",
        ],
    );

    assert_eq!(stdout, "");
    assert_eq!(stderr, "error: unknown signed-tags mode: false\n");
    assert_eq!(code, 129);

    // Drop the stage-1 error and the stage-2 one surfaces, with rev-list's own
    // `fatal:` wording and exit code rather than a usage error.
    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "--max-count=0x10", "feature..main"],
    );
    assert_eq!(stdout, "");
    assert_eq!(stderr, "fatal: '0x10': not an integer\n");
    assert_eq!(code, 128);
}

/// The same precedence against the other two things stage 1 outranks: an unknown
/// option, which `PARSE_OPT_KEEP_UNKNOWN_OPT` defers to stage 3, and a malformed
/// diff score, which `diff_scoreopt_parse` only sees in stage 2.
#[test]
fn stage_one_outranks_an_unknown_option_and_a_bad_diff_score() {
    let (root, repo) = fixture("precedence-more");
    let home = root.join("home");

    for leading in ["--bogus", "-Mfoo"] {
        let (stdout, stderr, code) = git(
            &repo,
            &home,
            &["fast-export", leading, "--signed-tags=false", "main"],
        );
        assert_eq!(stdout, "", "leading {leading}");
        assert_eq!(stderr, "error: unknown signed-tags mode: false\n", "leading {leading}");
        assert_eq!(code, 129, "leading {leading}");
    }

    // Each of those errors is real on its own; it is only outranked, not lost.
    let (_, stderr, code) = git(&repo, &home, &["fast-export", "-Mfoo", "main"]);
    assert_eq!(stderr, "error: invalid argument to find-renames\n");
    assert_eq!(code, 129);
}

/// Within stage 2 there is no precedence table at all — `setup_revisions` walks
/// its argv left to right, so whichever of the two bad arguments comes first wins.
///
/// This is the half that must *not* be reordered along with the rest: hoisting
/// `--max-count` out of argv order would make both spellings report the integer.
#[test]
fn inside_setup_revisions_the_earlier_argument_wins() {
    let (root, repo) = fixture("argv-order");
    let home = root.join("home");

    let (_, stderr, code) = git(&repo, &home, &["fast-export", "nosuchrev", "--max-count=0x10"]);
    assert_eq!(
        stderr,
        "fatal: ambiguous argument 'nosuchrev': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
    assert_eq!(code, 128);

    let (_, stderr, code) = git(&repo, &home, &["fast-export", "--max-count=0x10", "nosuchrev"]);
    assert_eq!(stderr, "fatal: '0x10': not an integer\n");
    assert_eq!(code, 128);
}

/// `--` ends stage 1, so an option behind it is not fast-export's to parse.
///
/// `parse_options_step()` breaks on `--` and, without `PARSE_OPT_KEEP_DASHDASH`,
/// drops it. `--signed-tags=false` therefore reaches `setup_revisions` as an
/// ordinary unknown argument, survives it, and leaves through the stage-3 leftover
/// check — the option list on stderr, exit 129, with the bad mode never diagnosed.
#[test]
fn the_separator_ends_fast_exports_own_parsing() {
    let (root, repo) = fixture("dashdash");
    let home = root.join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "main", "--", "--signed-tags=false"]);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("usage: git fast-export [<rev-list-opts>]\n"),
        "expected the option list, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("unknown signed-tags mode"),
        "the value must never be parsed behind `--`:\n{stderr}"
    );
    assert_eq!(code, 129);
}

/// The rest of stage 1's value parsers, each with the message its own callback
/// prints. All four are `error:` plus exit 129, and none of them prints the option
/// list — the distinction the port used to collapse.
#[test]
fn each_option_callback_reports_its_own_wording() {
    let (root, repo) = fixture("callbacks");
    let home = root.join("home");

    for (arg, msg) in [
        ("--signed-tags=false", "error: unknown signed-tags mode: false\n"),
        ("--signed-commits=false", "error: unknown signed-commits mode: false\n"),
        ("--tag-of-filtered-object=false", "error: unknown tag-of-filtered mode: false\n"),
        ("--reencode=maybe", "error: unknown reencoding mode: maybe\n"),
        (
            "--progress=zz",
            "error: option `progress' expects an integer value with an optional k/m/g suffix\n",
        ),
    ] {
        let (stdout, stderr, code) = git(&repo, &home, &["fast-export", arg, "main"]);
        assert_eq!(stdout, "", "for {arg}");
        assert_eq!(stderr, msg, "for {arg}");
        assert_eq!(code, 129, "for {arg}");
    }
}

/// `parse_sign_mode` accepts `ignore` as a synonym for `verbatim`
/// (`gpg-interface.c:1159`) but fast-export rejects the three `*-if-invalid` modes
/// it also accepts (`fast-export.c:67-69`), because those are for signing rather
/// than exporting.
#[test]
fn sign_mode_takes_ignore_but_not_the_if_invalid_modes() {
    let (root, repo) = fixture("signmode");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "--signed-tags=ignore", "--max-count=1", "main"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.starts_with("blob\n"), "expected a stream, got:\n{stdout}");

    for arg in ["abort-if-invalid", "strip-if-invalid", "sign-if-invalid"] {
        let (_, stderr, code) = git(&repo, &home, &["fast-export", &format!("--signed-tags={arg}"), "main"]);
        assert_eq!(stderr, format!("error: unknown signed-tags mode: {arg}\n"));
        assert_eq!(code, 129);
    }
}

/// Stage 2 stops parsing at the first argument that is a path rather than a
/// revision: `setup_revisions` hands `argv + i` wholesale to `verify_filename` and
/// `prune_data`, then breaks (`revision.c:3079-3095`).
///
/// So `--max-count=1` behind a pathspec is not an option at all. It is a path
/// beginning with `-`, and `verify_filename` (`setup.c:287-288`) says so. A second
/// path that does not exist gets the shorter of the two `die_verify_filename`
/// messages, because by then the ambiguity with a revision is already resolved.
#[test]
fn revision_parsing_stops_at_the_first_pathspec() {
    let (root, repo) = fixture("pathspec-break");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "main", "README.md", "--max-count=1"],
    );
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "fatal: option '--max-count=1' must come before non-option arguments\n"
    );
    assert_eq!(code, 128);

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "main", "README.md", "does-not-exist"],
    );
    assert_eq!(
        stderr,
        "fatal: does-not-exist: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n"
    );
    assert_eq!(code, 128);

    // A `^` could only ever have been a revision, so it never reaches the path
    // fallback and gets `bad revision` instead of the ambiguous wording.
    let (_, stderr, code) = git(&repo, &home, &["fast-export", "^nosuchrev"]);
    assert_eq!(stderr, "fatal: bad revision '^nosuchrev'\n");
    assert_eq!(code, 128);
}
