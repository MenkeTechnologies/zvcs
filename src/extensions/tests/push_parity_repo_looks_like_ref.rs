//! git 2.56's `die_if_repo_looks_like_ref()` (builtin/push.c:668-689).
//!
//! A repository argument that is neither a remote nor a group, has a non-empty
//! tail after its first slash, is not a path, and whose head names a configured
//! remote is refused before any transport is tried:
//!
//! ```c
//! code = die_message(_("'%s' is not a valid push target"), repo);
//! advise_if_enabled(ADVICE_PUSH_REPO_LOOKS_LIKE_REF,
//!                   _("Did you mean to use: git push %s %s?"),
//!                   name.buf, slash + 1);
//! exit(code);
//! ```
//!
//! `cmd_push()` (builtin/push.c:773-784) only calls it while
//! `advice.pushRepoLooksLikeRef` is enabled; with it off the argument falls
//! through to the URL/path attempt as in 2.55. Expectations captured from stock
//! git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-push-repo-looks-like-ref-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A work tree with one commit and an `origin` remote pointing at a sibling
/// bare repository.
fn fixture(name: &str) -> PathBuf {
    let root = scratch(name);
    git(&root, &["init", "-q", "--bare", "up.git"]);
    let work = root.join("w");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main", "."]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&work, &["remote", "add", "origin", "../up.git"]);
    work
}

const DISABLE: &str =
    "hint: Disable this message with \"git config set advice.pushRepoLooksLikeRef false\"\n";

#[test]
fn a_remote_slash_branch_argument_is_refused_with_the_split_spelled_out() {
    let w = fixture("typo");
    let out = git(&w, &["push", "origin/main"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        format!(
            "fatal: 'origin/main' is not a valid push target\n\
             hint: Did you mean to use: git push origin main?\n{DISABLE}"
        )
    );
    assert!(out.stdout.is_empty());

    // Only the first slash splits; the refspecs after it play no part.
    let nested = git(&w, &["push", "origin/feature/x", "main"]);
    assert_eq!(nested.status.code(), Some(128));
    assert_eq!(
        stderr(&nested),
        format!(
            "fatal: 'origin/feature/x' is not a valid push target\n\
             hint: Did you mean to use: git push origin feature/x?\n{DISABLE}"
        )
    );
}

/// `--repo` seeds the same `repo` variable the positional does.
#[test]
fn the_repo_option_is_checked_too() {
    let w = fixture("repo-opt");
    let out = git(&w, &["push", "--repo=origin/main"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        format!(
            "fatal: 'origin/main' is not a valid push target\n\
             hint: Did you mean to use: git push origin main?\n{DISABLE}"
        )
    );
}

/// A configured advice key keeps the hint but drops the "Disable" trailer; a
/// false one skips the check, leaving 2.55's transport failure.
#[test]
fn the_advice_key_gates_the_check() {
    let w = fixture("advice");
    let on = git(&w, &["-c", "advice.pushRepoLooksLikeRef=true", "push", "origin/main"]);
    assert_eq!(on.status.code(), Some(128));
    assert_eq!(
        stderr(&on),
        "fatal: 'origin/main' is not a valid push target\n\
         hint: Did you mean to use: git push origin main?\n"
    );

    let off = git(&w, &["-c", "advice.pushRepoLooksLikeRef=false", "push", "origin/main"]);
    assert_eq!(off.status.code(), Some(128));
    assert!(
        stderr(&off).starts_with("fatal: 'origin/main' does not appear to be a git repository\n"),
        "{}",
        stderr(&off)
    );
}

/// No configured remote before the slash, or nothing after it: no hint.
#[test]
fn only_a_configured_remote_with_a_tail_triggers_it() {
    let w = fixture("negative");
    for arg in ["nosuch/main", "origin/"] {
        let out = git(&w, &["push", arg]);
        assert_eq!(out.status.code(), Some(128), "{arg}");
        assert!(
            stderr(&out).starts_with(&format!("fatal: '{arg}' does not appear to be a git repository\n")),
            "{arg}: {}",
            stderr(&out)
        );
    }
}
