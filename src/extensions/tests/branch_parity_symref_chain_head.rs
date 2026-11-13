//! `HEAD -> refs/heads/alias -> refs/heads/side`, a chain of symbolic
//! references, read the way stock git 2.56 reads it.
//!
//! `refs_resolve_ref_unsafe(refs, "HEAD", 0, …)` (refs.c:2147-2194) follows the
//! whole chain, so the current branch is `refs/heads/side`:
//!
//! * `add_head_info()` keeps it as the worktree's `head_ref` (worktree.c:40-56),
//!   so `branch -f side main` dies with `cannot force update the branch`
//!   (branch.c:481-484) and `branch -d side` refuses a checked-out branch.
//! * `cmd_merge()` fast-forwards through the chain, logging on `HEAD`, the
//!   branch and the symref the update went through.
//! * `branch -m side newside` repoints `HEAD` itself at the new name
//!   (`replace_each_worktree_head_symref()`, builtin/branch.c:581-603) with a
//!   `Branch: renamed` line in `logs/HEAD`.
//! * `status`, `commit` and `branch --show-current` name `side`, and `branch -vv`
//!   marks no symref of the chain as checked out elsewhere (ref-filter.c's
//!   worktree map is keyed by `head_ref`).
//! * `alias` itself is not checked out: `branch -f alias main` moves `side`
//!   through it (no `REF_NO_DEREF`, refs/files-backend.c:2555-2607) and `HEAD`'s
//!   log-only entry takes the null id its parent lock still holds; `branch -d
//!   alias` deletes the symref, and `HEAD` logs the id it resolved to
//!   (refs/files-backend.c:2735-2773).
//!
//! Each case runs on a fresh copy of one fixture at the same path for both
//! binaries, and compares stdout, stderr, exit code, the refs, `HEAD`, the
//! index and every reflog file. Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    c.args(args);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "false")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .expect("spawn git")
}

/// `main` two commits ahead of nothing, `side` at its first commit, and `HEAD`
/// reaching `side` through `levels` symbolic references.
fn template(stock: &str, root: &Path, home: &Path, levels: usize) -> std::path::PathBuf {
    let g = |dir: &Path, args: &[&str]| {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let out = run(stock, dir, home, &full);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let repo = root.join(format!("template{levels}"));
    g(root, &["init", "-q", "-b", "main", repo.to_str().unwrap()]);
    std::fs::write(repo.join("f"), "1\n").unwrap();
    g(&repo, &["add", "f"]);
    g(&repo, &["commit", "-q", "-m", "one"]);
    g(&repo, &["branch", "side"]);
    std::fs::write(repo.join("f"), "2\n").unwrap();
    g(&repo, &["commit", "-q", "-am", "two"]);
    g(&repo, &["symbolic-ref", "refs/heads/alias", "refs/heads/side"]);
    let mut last = "refs/heads/alias".to_string();
    for n in 2..=levels {
        let name = format!("refs/heads/alias{n}");
        g(&repo, &["symbolic-ref", &name, &last]);
        last = name;
    }
    g(&repo, &["symbolic-ref", "HEAD", &last]);
    g(&repo, &["reset", "-q", "--hard", "side"]);
    repo
}

/// The refs, `HEAD`, the index against `HEAD` and every reflog file, read by
/// stock git and the file system so both runs are judged by the same reader.
fn post_state(stock: &str, repo: &Path, home: &Path) -> String {
    let mut s = String::new();
    for args in [
        &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"][..],
        &["status", "--porcelain"][..],
    ] {
        s.push_str(&String::from_utf8_lossy(&run(stock, repo, home, args).stdout));
    }
    s.push_str(&std::fs::read_to_string(repo.join(".git/HEAD")).unwrap());
    let mut logs = Vec::new();
    let mut stack = vec![repo.join(".git/logs")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                logs.push(p);
            }
        }
    }
    logs.sort();
    for p in logs {
        s.push_str(&format!("== {}\n", p.strip_prefix(repo).unwrap().display()));
        s.push_str(&std::fs::read_to_string(&p).unwrap());
    }
    s
}

fn copy_dir(from: &Path, to: &Path) {
    let st = Command::new("cp").arg("-R").arg(from).arg(to).status().unwrap();
    assert!(st.success());
}

#[test]
fn head_through_a_symref_chain_matches_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-branch-symref-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let cases: &[&[&str]] = &[
        &["branch", "-f", "side", "main"],
        &["branch", "-d", "side"],
        &["merge", "main"],
        &["branch", "-m", "side", "newside"],
        &["branch", "-m", "newname"],
        &["branch", "--show-current"],
        &["status"],
        &["commit", "--allow-empty", "-m", "e"],
        &["branch", "-f", "alias", "main"],
        &["branch", "-d", "alias"],
        &["branch", "-vv"],
    ];

    let mut diffs = Vec::new();
    for levels in [1, 2] {
        let template = template(stock, &root, &home, levels);
        for args in cases {
            let case = root.join("case");
            let mut seen = Vec::new();
            for bin in [stock, BIN] {
                let _ = std::fs::remove_dir_all(&case);
                copy_dir(&template, &case);
                let out = run(bin, &case, &home, args);
                seen.push((
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                    out.status.code(),
                    post_state(stock, &case, &home),
                ));
            }
            if seen[0] != seen[1] {
                diffs.push(format!("levels={levels} {args:?}\n  stock {:?}\n  zvcs  {:?}", seen[0], seen[1]));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
