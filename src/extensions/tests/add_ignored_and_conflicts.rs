//! What `git add` does with paths it is *not* simply going to stage: ignored
//! ones, ones inside a submodule, and ones the index still holds unmerged.
//!
//! Each of the three is a different part of `cmd_add()`:
//!
//!   * `-f` sets up no excludes at all (builtin/add.c:504-508), so the walk
//!     descends into an ignored directory like any other;
//!   * the ignored-paths block prints `dir->ignored[i]->name` (:349-350), which
//!     is the collapsed directory the walk stopped at, not the file underneath;
//!   * an unmerged path is in the index, so `-u` restages it — which is how a
//!     conflict is resolved.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-addign-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir fixture");
    root
}

/// The files and ignore rules of [`ignored_fixture`], written into `dir`.
fn write_ignored_tree(dir: &Path) {
    for sub in ["build", "logs"] {
        std::fs::create_dir_all(dir.join(sub)).expect("mkdir");
    }
    std::fs::write(dir.join(".gitignore"), "*.log\n!important.log\nbuild/\n/notes.tmp\n")
        .expect("write .gitignore");
    for (path, body) in [
        ("build/output.o", "o\n"),
        ("logs/debug.log", "d\n"),
        ("important.log", "i\n"),
        ("notes.tmp", "n\n"),
        ("tracked.txt", "t\n"),
    ] {
        std::fs::write(dir.join(path), body).expect("write fixture file");
    }
}

/// A worktree whose ignore rules cover a whole directory (`build/`), a directory
/// only by way of its contents (`logs/`, all `*.log`), and single files.
fn ignored_fixture(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    write_ignored_tree(&dir);
    ok(&dir, &["init", "-q", "-b", "main"]);
    dir
}

#[test]
fn force_descends_into_a_directory_the_ignore_rules_cover() {
    let dir = ignored_fixture("force");

    // Without `-f` the excludes are in place and the whole of `build/` and
    // `logs/` stays out; `important.log` is un-ignored by a negation.
    ok(&dir, &["add", "-A"]);
    let staged = stdout_of(&ok(&dir, &["diff", "--cached", "--name-only"]));
    let mut staged: Vec<&str> = staged.lines().collect();
    staged.sort_unstable();
    assert_eq!(staged, [".gitignore", "important.log", "tracked.txt"]);

    // With `-f` there are no excludes at all, so the walk goes inside both — the
    // directory an ignore rule names *and* the one whose every entry is ignored.
    ok(&dir, &["reset", "-q"]);
    ok(&dir, &["add", "-f", "-A"]);
    let forced = stdout_of(&ok(&dir, &["diff", "--cached", "--name-only"]));
    let mut forced: Vec<&str> = forced.lines().collect();
    forced.sort_unstable();
    assert_eq!(
        forced,
        [".gitignore", "build/output.o", "important.log", "logs/debug.log", "notes.tmp", "tracked.txt"]
    );
}

/// `bin` with no configuration but the repository's own: an empty `HOME` and
/// no `XDG_CONFIG_HOME` leave no global `core.excludesFile`, and
/// `GIT_CONFIG_NOSYSTEM` drops the system file, so only the fixture's
/// `.gitignore` decides what is ignored.
fn isolated(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

/// The ignored-paths block names the collapsed directory the walk stopped at
/// when a rule names the directory (`build/`); `logs/` is ignored only by way
/// of `*.log`, so the walk goes inside it. What each pathspec prints, and its
/// exit status, is taken from stock git on the same fixture.
#[test]
fn the_ignored_paths_block_names_the_directory_the_walk_stopped_at() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let home = scratch("report-home");
    let outcome = |bin: &str, tag: &str, args: &[&str]| {
        let dir = scratch(tag);
        write_ignored_tree(&dir);
        for setup in [&["init", "-q", "-b", "main"][..], &["add", "-A"], &["commit", "-qm", "x"]] {
            let out = isolated(bin, &dir, &home, setup);
            assert!(out.status.success(), "{bin} {setup:?}: {}", stderr_of(&out));
        }
        let out = isolated(bin, &dir, &home, args);
        let _ = std::fs::remove_dir_all(&dir);
        (out.status.code(), stdout_of(&out), stderr_of(&out))
    };

    for (n, path) in ["build/output.o", "logs/debug.log", "build/", "logs/", "notes.tmp"].into_iter().enumerate() {
        let args = ["add", path];
        let want = outcome(stock, &format!("report-s{n}"), &args);
        let got = outcome(BIN, &format!("report-z{n}"), &args);
        assert_eq!(got, want, "git add {path}: zvcs and stock git differ");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn update_restages_an_unmerged_path_and_so_resolves_it() {
    let dir = scratch("conflict");
    ok(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("conflict.txt"), "base\n").expect("write");
    std::fs::write(dir.join("other.txt"), "other\n").expect("write");
    ok(&dir, &["add", "-A"]);
    ok(&dir, &["commit", "-qm", "base"]);

    ok(&dir, &["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("conflict.txt"), "theirs\n").expect("write");
    ok(&dir, &["commit", "-qam", "theirs"]);
    ok(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "ours\n").expect("write");
    ok(&dir, &["commit", "-qam", "ours"]);

    let merge = run(&dir, &["merge", "side"]);
    assert_eq!(merge.status.code(), Some(1), "the merge must conflict: {}", stderr_of(&merge));
    assert!(stdout_of(&ok(&dir, &["status", "--short"])).contains("UU conflict.txt"));

    // Resolve it in the worktree and let `-u` stage the result.
    std::fs::write(dir.join("conflict.txt"), "resolved\n").expect("write");
    ok(&dir, &["add", "-u"]);

    let entries = stdout_of(&ok(&dir, &["ls-files", "-s"]));
    assert!(
        entries.lines().all(|l| l.split('\t').next().is_some_and(|meta| meta.ends_with(" 0"))),
        "every entry back at stage 0: {entries}"
    );
    assert_eq!(stdout_of(&ok(&dir, &["status", "--short"])), "M  conflict.txt\n");
}
