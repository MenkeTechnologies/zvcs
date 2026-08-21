//! `.gitmodules` is read lazily, and when it is read a malformed one is fatal.
//!
//! Both halves matter, and they are easy to get wrong in opposite directions.
//!
//! git does **not** tolerate a broken `.gitmodules`. `config_from_gitmodules()`
//! (submodule-config.c:784-814) passes `const struct config_options opts = { 0 }`
//! and `config_with_options()` then hands `NULL` options to
//! `git_config_from_file_with_options()`, so the per-source
//! `CONFIG_ERROR_DIE` applies exactly as it does to `.git/config`. The
//! `CONFIG_ERROR_SILENT` that really does swallow a parse failure lives in
//! `fsck_blob()` (fsck.c:1212), which reports `gitmodulesParse` itself instead.
//!
//! What git does is **not read the file** unless something asks it to.
//! `repo_read_gitmodules()` (submodule-config.c:830-844) is lazy, and in the
//! index-versus-worktree walk the only caller is `is_submodule_ignored()` on a
//! gitlink entry. A superproject whose index holds no gitlink therefore never
//! opens `.gitmodules`, and its contents cannot matter.
//!
//! So the observable rule is: with no gitlink, every command succeeds no matter
//! what the file says; with a gitlink, the command dies at 128 naming the file by
//! its absolute worktree path (`repo_worktree_path()` builds it from the worktree
//! root). Both measured against git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The whole of a `.gitmodules` that git's parser refuses: a legacy subsection
/// header with a stray word after the quoted name.
const BROKEN: &str = "[core \"a\" b]\n";

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-gmlazy-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    root.canonicalize().expect("canonicalize fixture")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("run zvcs git")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(dir, home, args);
    assert!(out.status.success(), "setup `git {args:?}`: {}", stderr(&out));
    out
}

/// A superproject with one ordinary file committed, and no gitlink anywhere.
fn without_gitlink(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("f.txt"), "hi\n").expect("write file");
    ok(&work, &home, &["add", "f.txt"]);
    ok(&work, &home, &["commit", "-q", "-m", "one"]);
    (work, home)
}

/// The same, plus a gitlink staged into the index by hand.
///
/// `update-index --add --cacheinfo` is used rather than `submodule add` so the
/// fixture needs no network, no transport allow-list and no second repository —
/// the gitlink entry is all that matters, since that is the only thing that makes
/// git open `.gitmodules`.
fn with_gitlink(tag: &str) -> (PathBuf, PathBuf) {
    let (work, home) = without_gitlink(tag);
    let head = String::from_utf8_lossy(&ok(&work, &home, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    std::fs::create_dir_all(work.join("sub")).expect("mkdir sub");
    ok(
        &work,
        &home,
        &["update-index", "--add", "--cacheinfo", &format!("160000,{head},sub")],
    );
    (work, home)
}

/// With no gitlink in the index the file is never opened, so its contents are
/// invisible — even to the commands whose whole job is to look at submodules.
#[test]
fn a_broken_gitmodules_is_invisible_without_a_gitlink() {
    let (work, home) = without_gitlink("nolink");
    std::fs::write(work.join(".gitmodules"), BROKEN).expect("write .gitmodules");
    ok(&work, &home, &["add", ".gitmodules"]);

    for args in [
        vec!["diff-files"],
        vec!["status"],
        vec!["status", "--short"],
        vec!["diff"],
        vec!["diff", "--submodule=log"],
        vec!["diff", "HEAD"],
        vec!["submodule", "status"],
        vec!["submodule", "init"],
        vec!["ls-files"],
    ] {
        let out = run(&work, &home, &args);
        assert!(
            out.status.success(),
            "{args:?}: exit {:?} {}",
            out.status.code(),
            stderr(&out)
        );
        assert_eq!(stderr(&out), "", "{args:?}");
    }
}

/// Once a gitlink is in the index the file is opened, and a malformed one is the
/// same refusal any config file gets: `bad config line <n> in file <path>` at
/// exit 128, with the path absolute.
#[test]
fn a_broken_gitmodules_is_fatal_once_a_gitlink_exists() {
    let (work, home) = with_gitlink("link");
    let modules = work.join(".gitmodules");
    std::fs::write(&modules, BROKEN).expect("write .gitmodules");
    let expected = format!("fatal: bad config line 1 in file {}\n", modules.display());

    for args in [
        vec!["status"],
        vec!["status", "--short"],
        vec!["diff-files"],
        vec!["diff", "HEAD"],
        vec!["diff", "--submodule=log"],
        vec!["submodule", "status"],
        vec!["submodule", "init"],
    ] {
        let out = run(&work, &home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
}

/// The line number is the file's own, not a constant, and a well-formed
/// `.gitmodules` beside a gitlink is silent — the two halves that keep the
/// laziness from being mistaken for "never read it".
#[test]
fn the_named_line_is_the_gitmodules_line() {
    let (work, home) = with_gitlink("line");
    let modules = work.join(".gitmodules");

    std::fs::write(&modules, "[submodule \"sub\"]\n\tpath = sub\n\turl = ../sub\n")
        .expect("write valid .gitmodules");
    let out = run(&work, &home, &["submodule", "status"]);
    assert!(out.status.success(), "exit {:?}: {}", out.status.code(), stderr(&out));

    std::fs::write(
        &modules,
        "[submodule \"sub\"]\n\tpath = sub\n\turl = ../sub\n[]\n",
    )
    .expect("write broken .gitmodules");
    let out = run(&work, &home, &["submodule", "status"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line 4 in file {}\n", modules.display())
    );
}
