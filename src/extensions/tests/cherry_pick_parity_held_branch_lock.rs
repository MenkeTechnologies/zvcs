//! `cherry-pick` (and `revert --continue`) while another process holds the
//! checked-out branch's lock or `HEAD.lock`.
//!
//! The sequencer reports the failed `HEAD` update in place, by C path:
//!
//! * an in-process pick: `try_to_commit()`'s `res = error("%s", err.buf)`
//!   (sequencer.c:1693-1696), folded to `res = !!res` by `do_pick_commit()`
//!   (sequencer.c:2570) — status 1, the merged index, `MERGE_MSG` and
//!   `CHERRY_PICK_HEAD` left behind, a sequence stopped on that pick;
//! * `--ff`: `fast_forward_to()`'s `error()` and `return -1` after the checkout
//!   (sequencer.c:657-676) — `fatal: cherry-pick failed` at 128 with the index
//!   already moved;
//! * `--continue`: the child `git commit`'s `die("%s", err.buf)`
//!   (builtin/commit.c:1945-1948), whose 128 is the command's status.
//!
//! zvcs returned every one of these to the dispatcher as lock contention, which
//! re-ran the verb against the index the first attempt had already written.
//! Each case runs stock git and zvcs on identical fixtures and compares
//! stdout, stderr, the exit status and the repository state afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `main` holds `init` and `x`; `side` forks at `init` and adds `b`, `c`, `d`.
/// `setup` then runs through stock git, so both binaries start from one state.
fn fixture(root: &Path, setup: &[&[&str]]) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let (_, err, code) = run(STOCK, &work, root, args);
        (err, code)
    };
    git(&["init", "-q", "-b", "main", "."]);
    std::fs::write(work.join("a"), "one\n").unwrap();
    git(&["add", "a"]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["checkout", "-q", "-b", "side"]);
    for (name, body) in [("b", "two\n"), ("c", "three\n"), ("d", "four\n")] {
        std::fs::write(work.join(name), body).unwrap();
        git(&["add", name]);
        git(&["commit", "-q", "-m", name]);
    }
    git(&["checkout", "-q", "main"]);
    std::fs::write(work.join("x"), "x\n").unwrap();
    git(&["add", "x"]);
    git(&["commit", "-q", "-m", "x"]);
    for step in setup {
        match step {
            ["write", path, body] => std::fs::write(work.join(path), body).unwrap(),
            args => {
                git(args);
            }
        }
    }
    work
}

/// Everything the failed update can leave behind, read with stock git.
fn state(work: &Path, home: &Path) -> String {
    let mut s = String::new();
    for args in [
        &["for-each-ref"][..],
        &["status", "--porcelain"],
        &["reflog", "list"],
        &["log", "--format=%H %s", "-3", "main"],
        &["fsck"],
    ] {
        let (out, err, code) = run(STOCK, work, home, args);
        s += &format!("$ git {}\n{out}{err}[{code}]\n", args.join(" "));
    }
    let git_dir = work.join(".git");
    for name in ["CHERRY_PICK_HEAD", "REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE"] {
        if let Ok(body) = std::fs::read_to_string(git_dir.join(name)) {
            s += &format!("== {name}\n{body}");
        }
    }
    if let Ok(dir) = std::fs::read_dir(git_dir.join("sequencer")) {
        let mut names: Vec<_> = dir.map(|e| e.unwrap().file_name()).collect();
        names.sort();
        for name in names {
            let body = std::fs::read_to_string(git_dir.join("sequencer").join(&name)).unwrap();
            s += &format!("== sequencer/{}\n{body}", name.to_string_lossy());
        }
    }
    s
}

/// Run `args` under a held `lock` (relative to `.git`) through both binaries
/// and compare the output and the state left behind. Returns stock's stderr
/// and exit status so a case can also pin what stock does.
fn compare(tag: &str, lock: &str, setup: &[&[&str]], args: &[&str]) -> (String, i32) {
    let mut seen = Vec::new();
    for (who, bin) in [("stock", STOCK), ("zvcs", ZVCS)] {
        let root = std::env::temp_dir().join(format!(
            "zvcs-cherry-pick-held-lock-{tag}-{who}-{}",
            std::process::id()
        ));
        let work = fixture(&root, setup);
        let root = root.canonicalize().unwrap();
        let lock_path = work.join(".git").join(lock);
        std::fs::write(&lock_path, "").unwrap();
        let (out, err, code) = run(bin, &work, &root, args);
        std::fs::remove_file(&lock_path).unwrap();
        let after = state(&work, &root);
        let root_str = root.display().to_string();
        let norm = |s: &str| s.replace(&root_str, "<root>");
        seen.push((norm(&out), norm(&err), code, norm(&after)));
        let _ = std::fs::remove_dir_all(&root);
    }
    let (stock, zvcs) = (&seen[0], &seen[1]);
    assert_eq!(zvcs.0, stock.0, "stdout of {args:?} under {lock}");
    assert_eq!(zvcs.1, stock.1, "stderr of {args:?} under {lock}");
    assert_eq!(zvcs.2, stock.2, "status of {args:?} under {lock}");
    assert_eq!(zvcs.3, stock.3, "state after {args:?} under {lock}");
    (stock.1.clone(), stock.2)
}

const LOCKS: [&str; 2] = ["refs/heads/main.lock", "HEAD.lock"];

fn lock_message(lock: &str) -> String {
    format!(
        "cannot lock ref 'HEAD': Unable to create '<root>/work/.git/{lock}': File exists.\n\n\
         Another git process seems to be running in this repository, or the lock file may be stale\n"
    )
}

#[test]
fn a_single_pick_stops_with_the_sequencers_error_at_one() {
    if !stock_available() {
        return;
    }
    for (i, lock) in LOCKS.iter().enumerate() {
        let (err, code) = compare(&format!("single-{i}"), lock, &[], &["cherry-pick", "side"]);
        assert_eq!((err, code), (format!("error: {}", lock_message(lock)), 1));
    }
}

#[test]
fn a_sequence_stops_on_the_pick_whose_head_update_failed() {
    if !stock_available() {
        return;
    }
    for (i, lock) in LOCKS.iter().enumerate() {
        compare(&format!("multi-{i}"), lock, &[], &["cherry-pick", "main..side"]);
        compare(&format!("multi-x-{i}"), lock, &[], &["cherry-pick", "-x", "side~1", "side"]);
    }
}

#[test]
fn an_edited_pick_folds_the_child_commits_fatal_to_one() {
    if !stock_available() {
        return;
    }
    // `-e` hands the commit to a `git commit` child (`run_git_commit()`); its
    // `die()` at 128 becomes the pick's 1 through `res = !!res` (sequencer.c:2570).
    let revert_setup: &[&[&str]] = &[&["write", "x", "y\n"], &["commit", "-q", "-am", "y"]];
    for (i, lock) in LOCKS.iter().enumerate() {
        let (err, code) = compare(&format!("edit-{i}"), lock, &[], &["cherry-pick", "-e", "side"]);
        assert_eq!((err, code), (format!("fatal: {}", lock_message(lock)), 1));
        compare(&format!("edit-multi-{i}"), lock, &[], &["cherry-pick", "-e", "main..side"]);
        compare(&format!("revert-edit-{i}"), lock, revert_setup, &["revert", "-e", "HEAD"]);
        compare(&format!("revert-edit-multi-{i}"), lock, revert_setup, &["revert", "-e", "HEAD", "HEAD~1"]);
    }
}

#[test]
fn no_commit_never_touches_head() {
    if !stock_available() {
        return;
    }
    for (i, lock) in LOCKS.iter().enumerate() {
        let (_, code) = compare(&format!("no-commit-{i}"), lock, &[], &["cherry-pick", "-n", "main..side"]);
        assert_eq!(code, 0);
    }
}

#[test]
fn a_fast_forward_dies_after_moving_the_index() {
    if !stock_available() {
        return;
    }
    let setup: &[&[&str]] = &[&["reset", "-q", "--hard", "HEAD~1"]];
    for (i, lock) in LOCKS.iter().enumerate() {
        let (err, code) = compare(&format!("ff-{i}"), lock, setup, &["cherry-pick", "--ff", "side~2"]);
        assert_eq!(
            (err, code),
            (format!("error: {}fatal: cherry-pick failed\n", lock_message(lock)), 128)
        );
        compare(&format!("ff-multi-{i}"), lock, setup, &["cherry-pick", "--ff", "main..side"]);
    }
}

#[test]
fn continue_is_the_child_commits_fatal() {
    if !stock_available() {
        return;
    }
    // `main` rewrites `d`, so picking `side` stops on an add/add conflict that
    // the setup resolves before `--continue`.
    let single: &[&[&str]] = &[
        &["write", "d", "y\n"],
        &["add", "d"],
        &["commit", "-q", "-m", "y"],
        &["cherry-pick", "side"],
        &["write", "d", "four\n"],
        &["add", "d"],
    ];
    let multi: &[&[&str]] = &[
        &["write", "c", "y\n"],
        &["add", "c"],
        &["commit", "-q", "-m", "y"],
        &["cherry-pick", "main..side"],
        &["write", "c", "three\n"],
        &["add", "c"],
    ];
    for (i, lock) in LOCKS.iter().enumerate() {
        let (err, code) = compare(&format!("continue-{i}"), lock, single, &["cherry-pick", "--continue"]);
        assert_eq!((err, code), (format!("fatal: {}", lock_message(lock)), 128));
        compare(&format!("continue-multi-{i}"), lock, multi, &["cherry-pick", "--continue"]);
    }
}

#[test]
fn revert_continue_is_the_child_commits_fatal() {
    if !stock_available() {
        return;
    }
    // Reverting `x` after `main` rewrote it stops on a modify/delete conflict.
    let single: &[&[&str]] = &[
        &["write", "x", "y\n"],
        &["commit", "-q", "-am", "y"],
        &["revert", "--no-edit", "HEAD~1"],
        &["rm", "-q", "x"],
    ];
    let multi: &[&[&str]] = &[
        &["write", "x", "y\n"],
        &["commit", "-q", "-am", "y"],
        &["write", "q", "q\n"],
        &["add", "q"],
        &["commit", "-q", "-m", "q"],
        &["revert", "--no-edit", "HEAD~2", "HEAD"],
        &["rm", "-q", "x"],
    ];
    for (i, lock) in LOCKS.iter().enumerate() {
        let (err, code) =
            compare(&format!("revert-continue-{i}"), lock, single, &["revert", "--continue", "--no-edit"]);
        assert_eq!((err, code), (format!("fatal: {}", lock_message(lock)), 128));
        compare(&format!("revert-continue-multi-{i}"), lock, multi, &["revert", "--continue", "--no-edit"]);
    }
}
