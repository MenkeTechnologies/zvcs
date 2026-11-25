//! `git branch -m`/`-c` with more than one worktree, against stock git.
//!
//! `replace_each_worktree_head_symref()` (builtin/branch.c:581-604) repoints the
//! `HEAD` of every worktree on the renamed branch, the main worktree included
//! when the rename runs from a linked one; zvcs skipped the main worktree, so its
//! `HEAD` was left naming a ref that no longer existed. A branch's reflog lives in
//! the common directory, and zvcs looked for it in the linked worktree's own git
//! directory, so a rename or copy from there dropped the branch's history.
//! A `-C` onto a branch already at the source's tip changes no value, and zvcs
//! then wrote no `Branch: copied` entry, where `commit_ref_update()` logs it
//! regardless (refs/files-backend.c:1787-1803).
//!
//! `reject_rebase_or_bisect_branch()` (builtin/branch.c:555-573, called at :661)
//! refuses to rename or copy a branch some detached worktree is rebasing or
//! bisecting; zvcs renamed it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// Every reflog under `.git/logs` and `.git/worktrees/*/logs`, without the
/// timestamps.
fn reflogs(git_dir: &Path) -> String {
    let mut files = Vec::new();
    let mut stack = vec![git_dir.join("logs"), git_dir.join("worktrees")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.components().any(|c| c.as_os_str() == "logs") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut out = String::new();
    for f in files {
        out.push_str(&format!("# {}\n", f.strip_prefix(git_dir).unwrap().display()));
        for line in std::fs::read_to_string(&f).unwrap().lines() {
            let (ids, msg) = line.split_once('\t').unwrap_or((line, ""));
            out.push_str(&format!("{} {msg}\n", &ids[..81]));
        }
    }
    out
}

/// Copy the pristine fixture to `run_dir`, the same path for both sides, run
/// `args` with `bin` in the worktree `cwd`, and return the output, the refs, both
/// `HEAD`s and the reflogs it left.
fn outcome(bin: &str, stock: &str, pristine: &Path, run_dir: &Path, cwd: &str, args: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(run_dir);
    let ok = Command::new("cp").arg("-Rp").arg(pristine).arg(run_dir).status().unwrap();
    assert!(ok.success());
    let repo = run_dir.join("repo");
    let got = run(bin, &run_dir.join(cwd), args);
    let refs = run(stock, &repo, &["for-each-ref", "--format=%(refname) %(objectname)"]).0;
    let main_head = std::fs::read_to_string(repo.join(".git/HEAD")).unwrap();
    let wt_head = std::fs::read_to_string(repo.join(".git/worktrees/wt/HEAD")).unwrap();
    format!("{got:?}\n{refs}{main_head}{wt_head}{}", reflogs(&repo.join(".git")))
}

/// `repo` on `main` with branches `feat` and `topic`, and a linked worktree
/// `wt` on `feat`; with `stopped`, `wt` is stopped in a `rebase -i` of `feat`
/// and `repo` in a bisect started from `main`.
fn fixture(stock: &str, root: &Path, stopped: bool) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |dir: &Path, args: &[&str]| assert_eq!(run(stock, dir, args).2, Some(0), "{args:?}");
    git(&repo, &["init", "-q", "-b", "main"]);
    for i in 1..=3 {
        std::fs::write(repo.join("f"), format!("{i}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["-c", "maintenance.auto=false", "commit", "-qm", &format!("c{i}")]);
    }
    git(&repo, &["branch", "feat"]);
    git(&repo, &["branch", "topic"]);
    git(&repo, &["worktree", "add", "-q", "--relative-paths", "../wt", "feat"]);
    if stopped {
        let wt = root.join("wt");
        let seq = "perl -i -pe 's/^pick/edit/ if $.==1'";
        let out = Command::new(stock)
            .args(["rebase", "-q", "-i", "HEAD~2"])
            .current_dir(&wt)
            .env("HOME", &wt)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SEQUENCE_EDITOR", seq)
            .env("GIT_EDITOR", "true")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        git(&repo, &["bisect", "start"]);
        git(&repo, &["bisect", "bad"]);
        git(&repo, &["bisect", "good", "HEAD~2"]);
    }
}

#[test]
fn rename_and_copy_from_a_linked_worktree_follow_every_head_and_keep_the_reflog() {
    let Some(stock) = stock_git::stock_git_at_least((2, 48, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-branch-wt-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pristine = base.join("pristine");
    fixture(stock, &pristine, false);
    let run_dir = base.join("run");
    let cases: &[(&str, &[&str])] = &[
        ("wt", &["branch", "-m", "main", "m2"]),
        ("wt", &["branch", "-M", "topic", "main"]),
        ("wt", &["branch", "-c", "main", "m3"]),
        ("wt", &["branch", "-m", "f2"]),
        ("repo", &["branch", "-m", "feat", "f2"]),
        // `topic` already holds `feat`'s tip: the copy is still logged.
        ("repo", &["branch", "-C", "feat", "topic"]),
    ];
    for (cwd, args) in cases {
        assert_eq!(
            outcome(BIN, stock, &pristine, &run_dir, cwd, args),
            outcome(stock, stock, &pristine, &run_dir, cwd, args),
            "{cwd} {args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_branch_being_rebased_or_bisected_is_neither_renamed_nor_copied() {
    let Some(stock) = stock_git::stock_git_at_least((2, 48, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-branch-wt-reject-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pristine = base.join("pristine");
    fixture(stock, &pristine, true);
    let run_dir = base.join("run");
    let cases: &[(&str, &[&str])] = &[
        ("repo", &["branch", "-m", "feat", "f2"]),
        ("repo", &["branch", "-c", "feat", "f3"]),
        ("wt", &["branch", "-M", "main", "m2"]),
        ("wt", &["branch", "-C", "main", "topic"]),
        ("repo", &["branch", "-m", "topic", "t2"]),
    ];
    for (cwd, args) in cases {
        assert_eq!(
            outcome(BIN, stock, &pristine, &run_dir, cwd, args),
            outcome(stock, stock, &pristine, &run_dir, cwd, args),
            "{cwd} {args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
