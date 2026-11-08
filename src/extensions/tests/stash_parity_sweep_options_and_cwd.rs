//! `git stash` behaviour a differential sweep against stock git 2.56.0 turned up,
//! each case run by both binaries on identical fixtures built by stock git with
//! pinned dates, comparing exit code, stdout, stderr and the post-state.
//!
//! * `push -S -q` with nothing staged is silent: `stash_staged()` prints
//!   `No staged changes` only `if (!quiet)` (builtin/stash.c:1361-1364).
//! * Option ends. `save` keeps a `--` in its message (`PARSE_OPT_KEEP_DASHDASH`,
//!   builtin/stash.c:2048-2050); `push` keeps it as a pathspec when an operand came
//!   first (:1956-1958); `--end-of-options` ends every subcommand's options and is
//!   dropped (parse-options.c:1119-1124); `apply`, `pop` and `branch` drop a `--`;
//!   `show` sends either marker to `setup_revisions()`, which skips the config
//!   defaults, reads dashed words after `--` as paths and refuses them after
//!   `--end-of-options` (setup.c:285-286).
//! * `push -m`, `push --pathspec-from-file` and `store -m` without a value are
//!   parse-options' ``switch `m' requires a value`` at 129.
//! * `show -u` of a stash whose untracked tree repeats a tracked path dies in
//!   `stash_worktree_untracked_merge()` (unpack-trees.c:3069-3071).
//! * From a subdirectory: `push -p` reads the scratch index through an absolute
//!   path and reverts the selection from the top level; `push -k` removes the
//!   directory it empties, cwd included, as the `reset --hard` child does; `push -u`
//!   keeps the cwd with `Refusing to remove current working directory`, as the
//!   `clean` child run in it does (builtin/clean.c:247-265).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Outcome = (i32, String, String);

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> Outcome {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// Two commits of `a`, `b` and `sub/c`, built by stock git; `shape` then
/// dirties the copy with stock git too.
fn fixture(stock: &str, dir: &Path, shape: &dyn Fn(&dyn Fn(&[&str]), &Path)) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let g = |args: &[&str]| {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let out = run(stock, dir, &full, "");
        assert_eq!(out.0, 0, "setup {args:?}: {out:?}");
    };
    g(&["init", "-q", "-b", "main", "."]);
    write(dir, "a", "one\ntwo\nthree\n");
    write(dir, "b", "bee\n");
    write(dir, "sub/c", "c1\nc2\n");
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "init"]);
    write(dir, "a", "one\ntwo\nthree\nfour\n");
    g(&["commit", "-q", "-am", "second"]);
    shape(&g, dir);
}

/// What the steps leave behind, read by stock git from the top level, plus the
/// directory skeleton of the worktree.
fn post_state(stock: &str, dir: &Path) -> Vec<Outcome> {
    let mut state: Vec<_> = [
        &["stash", "list"][..],
        &["for-each-ref", "--format=%(refname) %(objectname)"],
        &["ls-files", "-s"],
        &["status", "--porcelain=v2", "-uall", "--ignored"],
    ]
    .iter()
    .map(|args| run(stock, dir, args, ""))
    .collect();
    let mut dirs = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() && p.file_name().unwrap() != ".git" {
                dirs.push(p.strip_prefix(dir).unwrap().display().to_string());
                stack.push(p);
            }
        }
    }
    dirs.sort();
    state.push((0, dirs.join("\n"), String::new()));
    state
}

/// Runs `steps` (argv, stdin) from `cwd` under each binary and compares every
/// outcome and the final state.
fn compare(tag: &str, shape: &dyn Fn(&dyn Fn(&[&str]), &Path), cwd: &str, steps: &[(&[&str], &str)]) {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-stash-sweep-{tag}-{}", std::process::id()));
    let _guard = Dir(root.clone());
    let mut seen = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = root.join(side);
        fixture(stock, &dir, shape);
        let outs: Vec<Outcome> = steps.iter().map(|(args, stdin)| run(bin, &dir.join(cwd), args, stdin)).collect();
        seen.push((outs, post_state(stock, &dir)));
    }
    assert_eq!(seen[0], seen[1], "{tag}: {steps:?} from {cwd:?} (stock, zvcs)");
}

/// One unstaged, one staged and one staged-plus-unstaged change, an untracked
/// file and directory.
fn dirty(g: &dyn Fn(&[&str]), dir: &Path) {
    write(dir, "a", "ONE\ntwo\nthree\nfour\n");
    write(dir, "b", "bee2\n");
    g(&["add", "b"]);
    write(dir, "sub/c", "c1\nc2\nc3\n");
    g(&["add", "sub/c"]);
    write(dir, "sub/c", "c1\nC2\nc3\n");
    write(dir, "u", "u\n");
    write(dir, "ud/x", "x\n");
}

fn unstaged(_: &dyn Fn(&[&str]), dir: &Path) {
    write(dir, "a", "changed\n");
}

/// `sub/c` dropped from the index (still on disk, so untracked) and `b` deleted
/// from the worktree.
fn dropped(g: &dyn Fn(&[&str]), dir: &Path) {
    g(&["rm", "-q", "--cached", "sub/c"]);
    std::fs::remove_file(dir.join("b")).unwrap();
}

#[test]
fn staged_only_quiet_with_nothing_staged_prints_nothing() {
    compare("sq", &unstaged, "", &[(&["stash", "push", "-S", "-q"], "")]);
    compare("sql", &unstaged, "", &[(&["stash", "save", "--staged", "--quiet"], "")]);
    compare("s", &unstaged, "", &[(&["stash", "push", "-S"], "")]);
}

#[test]
fn save_and_push_keep_or_drop_the_option_end_as_parse_options_does() {
    for (tag, args) in [
        ("save-dd", &["stash", "save", "--", "-dash"][..]),
        ("save-mid", &["stash", "save", "a", "--", "-q", "b"]),
        ("save-eoo", &["stash", "save", "--end-of-options", "-q"]),
        ("save-eoo-dd", &["stash", "save", "--end-of-options", "--", "-q"]),
        ("push-mid", &["stash", "push", "a", "--", "b"]),
        ("push-eoo-dd", &["stash", "push", "--end-of-options", "--", "a"]),
        ("push-eoo-eoo", &["stash", "push", "--end-of-options", "--end-of-options"]),
        ("assumed-eoo", &["stash", "--end-of-options"]),
        ("assumed-eoo-op", &["stash", "--end-of-options", "a"]),
        ("assumed-q-eoo", &["stash", "-q", "--end-of-options", "--", "a"]),
        ("push-m", &["stash", "push", "-m"]),
        ("push-message", &["stash", "push", "--message"]),
        ("push-pff", &["stash", "push", "--pathspec-from-file"]),
        ("store-m", &["stash", "store", "-m"]),
    ] {
        compare(tag, &dirty, "", &[(args, ""), (&["stash", "list"], "")]);
    }
}

#[test]
fn apply_pop_branch_and_show_end_their_options() {
    let push: &[&str] = &["stash", "push", "-u", "-q"];
    for (tag, args) in [
        ("apply-dd", &["stash", "apply", "--", "0"][..]),
        ("pop-eoo", &["stash", "pop", "--end-of-options", "0"]),
        ("branch-dd", &["stash", "branch", "--", "nb"]),
        ("branch-eoo", &["stash", "branch", "--end-of-options", "nb"]),
        ("show-dd", &["stash", "show", "--", "0"]),
        ("show-stat-dd", &["stash", "show", "--stat", "--", "0"]),
        ("show-dd-path", &["stash", "show", "--", "--stat"]),
        ("show-eoo", &["stash", "show", "--end-of-options", "0"]),
        ("show-eoo-dashed", &["stash", "show", "--end-of-options", "--stat"]),
    ] {
        compare(tag, &dirty, "", &[(push, ""), (args, ""), (&["stash", "list"], "")]);
    }
}

#[test]
fn show_untracked_refuses_a_path_both_trees_record() {
    let push: &[&str] = &["stash", "push", "-u", "-q"];
    compare("dup-u", &dropped, "", &[(push, ""), (&["stash", "show", "-u"], "")]);
    compare("dup-only", &dropped, "", &[(push, ""), (&["stash", "show", "--only-untracked", "--include-untracked"], "")]);
    compare("dup-cfg", &dropped, "", &[(push, ""), (&["-c", "stash.showIncludeUntracked=true", "stash", "show", "-p"], "")]);
    compare("dup-only-ok", &dropped, "", &[(push, ""), (&["stash", "show", "--only-untracked"], "")]);
}

#[test]
fn patch_mode_from_a_subdirectory() {
    compare("p-all", &dirty, "sub", &[(&["stash", "push", "-p"], "y\ny\ny\n")]);
    compare("p-some", &dirty, "sub", &[(&["stash", "push", "-p"], "n\ny\ny\n")]);
    compare("p-up", &dirty, "sub", &[(&["stash", "push", "-p", "--", "../a", "c"], "n\ny\n")]);
    compare("p-dot", &dirty, "sub", &[(&["stash", "push", "-p", "--", "."], "y\n")]);
    compare("p-dotdot", &dirty, "sub", &[(&["stash", "push", "-p", "--", ".."], "y\ny\ny\n")]);
    compare("p-k", &dirty, "sub", &[(&["stash", "push", "-p", "-k"], "y\nn\ny\n")]);
}

#[test]
fn keep_index_removes_the_directories_it_empties() {
    compare("k-top", &dropped, "", &[(&["stash", "push", "-k"], ""), (&["stash", "pop"], "")]);
    compare("k-q", &dropped, "", &[(&["stash", "push", "--keep-index", "-q"], "")]);
    // The emptied directory is the cwd: the reset child takes it anyway.
    compare("k-cwd", &dropped, "sub", &[(&["stash", "push", "-k"], "")]);
    // `git rm` took the file and its directory already.
    let removed = |g: &dyn Fn(&[&str]), dir: &Path| {
        g(&["rm", "-q", "sub/c"]);
        write(dir, "a", "dirty\n");
    };
    compare("k-rm", &removed, "", &[(&["stash", "push", "-k"], "")]);
    // Another untracked file keeps the directory standing.
    let kept = |g: &dyn Fn(&[&str]), dir: &Path| {
        g(&["rm", "-q", "--cached", "sub/c"]);
        write(dir, "sub/other", "o\n");
    };
    compare("k-kept", &kept, "", &[(&["stash", "push", "-k"], "")]);
}

#[test]
fn include_untracked_keeps_the_cwd_it_empties() {
    compare("u-cwd", &dropped, "sub", &[(&["stash", "push", "-u"], "")]);
    compare("u-cwd-q", &dropped, "sub", &[(&["stash", "push", "-u", "-q"], "")]);
    compare("u-cwd-pop", &dropped, "sub", &[(&["stash", "push", "-u"], ""), (&["stash", "pop", "--index"], "")]);
    compare("u-cwd-ps", &dropped, "sub", &[(&["stash", "push", "-u", "--", "c"], "")]);
    compare("a-cwd", &dirty, "ud", &[(&["stash", "push", "-a"], "")]);
}
