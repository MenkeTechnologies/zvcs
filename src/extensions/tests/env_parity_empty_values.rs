//! The `GIT_*` variables git reads with a *presence* test, probed with the one
//! value that separates presence from content: the empty string.
//!
//! `getenv()` returns a non-NULL pointer for `VAR=`, and so do the wrappers
//! built on it — `getenv_safe()` (environment.c:120-127) and
//! `xstrdup_or_null(getenv(...))`. A port that reaches for `is_some_and(|v|
//! !v.is_empty())`, or that resolves an empty path against a directory, turns
//! those variables into "unset" and silently does something else. Every
//! expectation below was measured against git 2.55.0 on the same fixture.
//!
//! Covered here:
//!
//! * `GIT_WORK_TREE=` — setup.c:1217 routes discovery through
//!   `setup_explicit_git_dir()`, which hands the empty string to
//!   `set_git_work_tree()` (setup.c:1142-1143) →  `repo_set_worktree()`
//!   (repository.c:252-254) → `real_pathdup(path, 1)`, which dies at
//!   abspath.c:89-91 with `The empty string is not a valid path`.
//! * `GIT_INDEX_FILE=` — setup.c:1048 reads it with `getenv_safe()` and
//!   `expand_base_dir()` (repository.c:101-109) stores it verbatim, so the index
//!   path *is* the empty path; `open("")` is `ENOENT` and `do_read_index()` takes
//!   its empty-index branch rather than reporting an error.
//! * `GIT_CONFIG_KEY_<n>=` — `config_parse_pair()` (config.c:630-631) answers the
//!   empty key with its own line, `empty config key`, before
//!   `git_config_parse_key()` is ever reached, so it is not "a key with no
//!   section".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_IMPLICIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1000000000 +0000")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1000000000 +0000");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("run zvcs git")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, &[], args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One commit holding two tracked files, beside an empty `$HOME`. Named per test
/// and per pid so concurrent test binaries never share a directory.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-env-empty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("r");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(work.join("sub")).expect("mkdir work");
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("f.txt"), "one\n").expect("write f");
    std::fs::write(work.join("sub/g.txt"), "two\n").expect("write g");
    ok(&work, &home, &["add", "-A"]);
    ok(&work, &home, &["commit", "-qm", "first"]);
    let work = std::fs::canonicalize(&work).expect("canonicalize work");
    (root, home, work)
}

/// `GIT_WORK_TREE=` is a work tree that was *named*, so the repository the walk
/// found is handed to `setup_explicit_git_dir()` and the empty path kills the
/// command — for every verb that reaches setup, whether or not it has anything
/// to do with a work tree.
///
/// `git config --list` and `git var GIT_EDITOR` are the interesting members of
/// the list: they carry on when setup comes up *empty*, which is a different
/// question from whether setup runs, and both exit 128 here under stock 2.55.0.
#[test]
fn empty_work_tree_is_a_named_work_tree_and_dies_on_the_empty_path() {
    let (root, home, work) = fixture("wt-dies");
    for args in [
        &["rev-parse", "--show-toplevel"][..],
        &["rev-parse", "--is-inside-work-tree"],
        &["status", "--porcelain"],
        &["config", "--list"],
        &["var", "GIT_EDITOR"],
        &["ls-files"],
        &["log", "--oneline"],
        &["for-each-ref"],
        &["cat-file", "-t", "HEAD"],
    ] {
        let out = run(&work, &home, &[("GIT_WORK_TREE", "")], args);
        assert_eq!(out.status.code(), Some(128), "{args:?} exit code");
        assert_eq!(stderr(&out), "fatal: The empty string is not a valid path\n", "{args:?} stderr");
        assert_eq!(stdout(&out), "", "{args:?} stdout");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The refusal is the empty *path*, not the empty *variable*, so a value that
/// names something keeps working — including one that names nothing on disk,
/// which `real_pathdup()` resolves through its existing prefix rather than
/// refusing.
#[test]
fn a_named_work_tree_still_works_empty_or_not() {
    let (root, home, work) = fixture("wt-named");
    let top = run(&work, &home, &[("GIT_WORK_TREE", ".")], &["rev-parse", "--show-toplevel"]);
    assert!(top.status.success(), "{}", stderr(&top));
    assert_eq!(stdout(&top).trim_end(), work.to_str().expect("utf8 work"));

    let missing = run(&work, &home, &[("GIT_WORK_TREE", "nosuch")], &["rev-parse", "--is-inside-work-tree"]);
    assert!(missing.status.success(), "{}", stderr(&missing));
    assert_eq!(stdout(&missing), "false\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// Outside a repository the walk fails before `setup_explicit_git_dir()` is
/// reached, so the variable is never looked at. Measured under stock 2.55.0 in an
/// empty directory: both of these exit 0.
#[test]
fn empty_work_tree_is_silent_without_a_repository() {
    let (root, home, _work) = fixture("wt-norepo");
    let bare_dir = root.join("elsewhere");
    std::fs::create_dir_all(&bare_dir).expect("mkdir elsewhere");
    for args in [&["config", "--list"][..], &["var", "GIT_EDITOR"], &["version"]] {
        let out = run(&bare_dir, &home, &[("GIT_WORK_TREE", "")], args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The four verbs measured to survive `GIT_WORK_TREE=` *inside* a repository
/// under stock 2.55.0, because they never reach repository setup at all. A gate
/// that used the port's `NO_SETUP_VERBS` list instead would wrongly spare
/// `config`, `diff`, `var` and two dozen others alongside them.
#[test]
fn empty_work_tree_spares_only_the_verbs_that_skip_setup() {
    let (root, home, work) = fixture("wt-spared");
    for args in [&["version"][..], &["stripspace"], &["check-ref-format", "refs/heads/x"]] {
        let out = run(&work, &home, &[("GIT_WORK_TREE", "")], args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{args:?} stderr");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The empty index path is the empty path, and a file that cannot be opened is
/// an empty index — not an error. Before the fix the value was joined onto the
/// work tree, which named the work tree *directory*, and opening a directory as
/// an index answered `EINVAL`, which is not the `ENOENT` the empty-index branch
/// is keyed on: every one of these reported an I/O error and exited 128.
#[test]
fn empty_index_file_reads_as_an_empty_index() {
    let (root, home, work) = fixture("idx-empty");
    let empty = [("GIT_INDEX_FILE", "")];

    let listed = run(&work, &home, &empty, &["ls-files"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert_eq!(stdout(&listed), "", "an empty index lists nothing");

    // Nothing is staged, so everything `HEAD` has reads as a deletion.
    let staged = run(&work, &home, &empty, &["diff", "--cached", "--name-only"]);
    assert!(staged.status.success(), "{}", stderr(&staged));
    assert_eq!(stdout(&staged), "f.txt\nsub/g.txt\n");

    let refreshed = run(&work, &home, &empty, &["update-index", "--refresh"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    assert_eq!(stdout(&refreshed), "", "nothing to refresh in an empty index");

    let indexed = run(&work, &home, &empty, &["diff-index", "--cached", "--name-only", "HEAD"]);
    assert!(indexed.status.success(), "{}", stderr(&indexed));
    assert_eq!(stdout(&indexed), "f.txt\nsub/g.txt\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// The repository's own index is still what an *unset* variable means, and a
/// value that names a file that does not exist is the same empty index — the
/// case that already worked and must keep working.
#[test]
fn index_file_unset_and_missing_are_unchanged() {
    let (root, home, work) = fixture("idx-other");
    let unset = run(&work, &home, &[], &["ls-files"]);
    assert_eq!(stdout(&unset), "f.txt\nsub/g.txt\n", "{}", stderr(&unset));

    let missing = run(&work, &home, &[("GIT_INDEX_FILE", "nosuch")], &["ls-files"]);
    assert!(missing.status.success(), "{}", stderr(&missing));
    assert_eq!(stdout(&missing), "");

    let named = run(&work, &home, &[("GIT_INDEX_FILE", ".git/index")], &["ls-files"]);
    assert_eq!(stdout(&named), "f.txt\nsub/g.txt\n", "{}", stderr(&named));
    let _ = std::fs::remove_dir_all(&root);
}

/// `config_parse_pair()` tests the key's *length* before it parses it, and the
/// environment triple reaches that function (config.c:775) by the same door `-c`
/// does (config.c:674) — so both spellings answer an empty key with
/// `empty config key`, and the port answering `key does not contain a section:`
/// for one of them was a private, truncated copy of the check.
#[test]
fn empty_config_key_reports_its_own_line_from_either_door() {
    let (root, home, work) = fixture("key-empty");
    let want = "error: empty config key\nfatal: unable to parse command-line config\n";

    let from_env = run(
        &work,
        &home,
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", ""), ("GIT_CONFIG_VALUE_0", "v")],
        &["config", "--list"],
    );
    assert_eq!(stderr(&from_env), want, "GIT_CONFIG_KEY_0=");
    assert_eq!(from_env.status.code(), Some(128));

    let from_cli = run(&work, &home, &[], &["-c", "=v", "config", "--list"]);
    assert_eq!(stderr(&from_cli), want, "-c =v");
    assert_eq!(from_cli.status.code(), Some(128));
    let _ = std::fs::remove_dir_all(&root);
}

/// The rest of `git_config_parse_key()` applies to the environment triple too.
/// The port's copy tested only for a dot, so every key below was accepted from
/// the environment while `-c` refused it.
#[test]
fn environment_config_keys_face_the_whole_key_parser() {
    let (root, home, work) = fixture("key-rules");
    for (key, want) in [
        ("a.", "key does not contain variable name: a."),
        (".b", "key does not contain a section: .b"),
        ("a.b!", "invalid key: a.b!"),
        ("a.1b", "invalid key: a.1b"),
    ] {
        let out = run(
            &work,
            &home,
            &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", key), ("GIT_CONFIG_VALUE_0", "v")],
            &["config", "--list"],
        );
        assert_eq!(
            stderr(&out),
            format!("error: {want}\nfatal: unable to parse command-line config\n"),
            "GIT_CONFIG_KEY_0={key}"
        );
        assert_eq!(out.status.code(), Some(128), "GIT_CONFIG_KEY_0={key}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The keys the parser accepts keep working, canonicalized the way `-c` does:
/// the section and the variable name are lower-cased and the extended basename
/// between the first and last dot is left exactly as typed.
#[test]
fn valid_environment_config_keys_still_arrive_canonicalized() {
    let (root, home, work) = fixture("key-valid");
    for (key, want) in [("a.b", "a.b=v"), ("A.B", "a.b=v"), ("a.Sub B.c", "a.Sub B.c=v")] {
        let out = run(
            &work,
            &home,
            &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", key), ("GIT_CONFIG_VALUE_0", "v")],
            &["config", "--list"],
        );
        assert!(out.status.success(), "GIT_CONFIG_KEY_0={key}: {}", stderr(&out));
        assert!(
            stdout(&out).lines().any(|line| line == want),
            "GIT_CONFIG_KEY_0={key} wanted {want} among\n{}",
            stdout(&out)
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
