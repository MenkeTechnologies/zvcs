//! `git branch` lists a detached `HEAD` under `get_head_description()`
//! (ref-filter.c:2297-2327), against stock git: a rebase in progress says
//! `(no branch, rebasing <branch>)`, a bisect says
//! `(no branch, bisect started on <branch>)`, and only otherwise does the
//! reflog's `detached at`/`from` wording apply.
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
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `main` and `topic` both rewrite `f`, so rebasing `main` onto `topic` stops.
fn fixture(stock: &str, root: &Path) {
    let git = |args: &[&str]| run(stock, root, args);
    std::fs::create_dir_all(root).unwrap();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "one"]);
    git(&["branch", "topic"]);
    std::fs::write(root.join("f"), "main\n").unwrap();
    git(&["commit", "-qam", "main"]);
    git(&["checkout", "-q", "topic"]);
    std::fs::write(root.join("f"), "topic\n").unwrap();
    git(&["commit", "-qam", "topic"]);
    git(&["checkout", "-q", "main"]);
}

#[test]
fn rebase_and_bisect_name_the_detached_head() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-branch-headdesc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    let setups: [&[&[&str]]; 4] = [
        &[&["rebase", "topic"]],
        &[&["rebase", "--apply", "topic"]],
        &[&["checkout", "-q", "--detach"], &["rebase", "topic"]],
        &[&["bisect", "start", "main", "main~1"]],
    ];
    for (i, steps) in setups.iter().enumerate() {
        let root = base.join(i.to_string());
        fixture(stock, &root);
        for step in *steps {
            run(stock, &root, step);
        }
        for args in [&["branch"][..], &["branch", "-v"], &["branch", "--format=%(refname)"]] {
            assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{steps:?} {args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
