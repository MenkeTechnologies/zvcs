//! Which verbs the `GIT_*` setup refusals apply to is decided per *invocation*,
//! not per verb.
//!
//! The port kept one list, `NO_SETUP_VERBS`, and every setup gate consulted it —
//! so `hash-object` was exempt from all of them. git does not work that way:
//! `cmd_hash_object()` (builtin/hash-object.c:99-105) parses its options first
//! and then picks which setup to run.
//!
//! ```c
//! argc = parse_options(argc, argv, prefix, hash_object_options,
//!                      hash_object_usage, 0);
//!
//! if (flags & INDEX_WRITE_OBJECT)
//!         prefix = setup_git_directory(the_repository);
//! else
//!         prefix = setup_git_directory_gently(the_repository, &nongit);
//! ```
//!
//! `INDEX_WRITE_OBJECT` is `-w`, which has no long form
//! (builtin/hash-object.c:85-86). So `git hash-object --stdin` is a pure hash and
//! needs nothing, while `git hash-object -w --stdin` needs a repository it is
//! allowed to write to — and every refusal that stands between it and one
//! applies. All four expectations below were measured against git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

fn run(dir: &Path, home: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_TEST_ASSUME_DIFFERENT_OWNER")
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

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-env-strict-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("r");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("f.txt"), "one\n").expect("write f");
    ok(&work, &home, &["add", "-A"]);
    ok(&work, &home, &["commit", "-qm", "first"]);
    let work = std::fs::canonicalize(&work).expect("canonicalize work");
    (root, home, work)
}

/// The four environment refusals that stand between `hash-object -w` and a
/// repository, each measured on its own.
///
/// * `GIT_OBJECT_DIRECTORY` and `GIT_COMMON_DIR` un-recognise every candidate
///   directory in `is_git_directory()` (setup.c:433-442), so the walk reaches the
///   ceiling and reports the ordinary missing-repository line.
/// * `GIT_DIR` is the explicit form and is reported by name (setup.c:1127-1133).
/// * `safe.directory` refuses a repository we do not own;
///   `GIT_TEST_ASSUME_DIFFERENT_OWNER` is git's own way of reaching that check
///   without a second account.
#[test]
fn hash_object_w_faces_every_setup_refusal() {
    let (root, home, work) = fixture("w-refused");
    let walked = "fatal: not a git repository (or any of the parent directories): .git\n";
    for (env, want) in [
        (("GIT_OBJECT_DIRECTORY", "nosuch"), walked.to_owned()),
        (("GIT_COMMON_DIR", "nosuch"), walked.to_owned()),
        (("GIT_DIR", "nosuch"), "fatal: not a git repository: 'nosuch'\n".to_owned()),
    ] {
        let out = run(&work, &home, &[env], &["hash-object", "-w", "--stdin"]);
        assert_eq!(stderr(&out), want, "{}={}", env.0, env.1);
        assert_eq!(out.status.code(), Some(128), "{}={}", env.0, env.1);
        assert_eq!(stdout(&out), "", "{}={}", env.0, env.1);
    }

    let owned = run(
        &work,
        &home,
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["hash-object", "-w", "--stdin"],
    );
    assert_eq!(owned.status.code(), Some(128), "ownership: {}", stderr(&owned));
    assert!(
        stderr(&owned).starts_with("fatal: detected dubious ownership in repository at "),
        "ownership stderr was {:?}",
        stderr(&owned)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Without `-w` the same four leave it alone: `setup_git_directory_gently()` is
/// content with no repository and the blob is only hashed, never stored.
#[test]
fn hash_object_without_w_is_untouched_by_them() {
    let (root, home, work) = fixture("no-w");
    for env in [
        ("GIT_OBJECT_DIRECTORY", "nosuch"),
        ("GIT_COMMON_DIR", "nosuch"),
        ("GIT_DIR", "nosuch"),
        ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1"),
    ] {
        let out = run(&work, &home, &[env], &["hash-object", "--stdin"]);
        assert!(out.status.success(), "{}={}: {}", env.0, env.1, stderr(&out));
        assert_eq!(stdout(&out).trim_end(), EMPTY_BLOB, "{}={}", env.0, env.1);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `-w` is an option, so it is found by parsing and not by looking for the
/// character. `-t` takes a value, which makes `-tw` the *type* `w` — no write at
/// all — while `-wt blob` clumps a real `-w` in front of it, and everything
/// behind `--` is a path however it is spelled.
#[test]
fn the_w_that_triggers_setup_is_the_option_not_the_letter() {
    let (root, home, work) = fixture("w-parsing");
    let broken = [("GIT_OBJECT_DIRECTORY", "nosuch")];

    // A clumped `-w` in front of `-t` still writes, so the refusal applies.
    let clumped = run(&work, &home, &broken, &["hash-object", "-wt", "blob", "--stdin"]);
    assert_eq!(clumped.status.code(), Some(128), "-wt blob: {}", stdout(&clumped));

    // `-t blob` alone carries no `-w`.
    let typed = run(&work, &home, &broken, &["hash-object", "-t", "blob", "--stdin"]);
    assert!(typed.status.success(), "-t blob: {}", stderr(&typed));
    assert_eq!(stdout(&typed).trim_end(), EMPTY_BLOB);

    // `-w` behind `--` is a path, and a missing one at that — the failure is the
    // file, not the repository.
    let behind = run(&work, &home, &broken, &["hash-object", "--", "-w"]);
    assert_ne!(
        stderr(&behind),
        "fatal: not a git repository (or any of the parent directories): .git\n",
        "a path named -w must not be read as the option"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The verbs that really do skip setup keep skipping it, so widening the gates
/// did not turn `NO_SETUP_VERBS` into "no exemptions at all".
#[test]
fn the_other_no_setup_verbs_stay_exempt() {
    let (root, home, work) = fixture("still-exempt");
    for env in [("GIT_OBJECT_DIRECTORY", "nosuch"), ("GIT_COMMON_DIR", "nosuch")] {
        for args in [&["version"][..], &["var", "GIT_EDITOR"], &["check-ref-format", "refs/heads/x"]] {
            let out = run(&work, &home, &[env], args);
            assert!(out.status.success(), "{args:?} under {}={}: {}", env.0, env.1, stderr(&out));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
