//! `git merge-resolve` / `git merge-octopus` against stock git: both source
//! git-sh-setup without `SUBDIRECTORY_OK`, so `rev-parse --show-cdup` must be
//! empty. A `GIT_WORK_TREE` that does not exist is not the current directory, so
//! `--show-cdup` prints a path and the strategy refuses with exit 1 before it
//! looks at its arguments.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, work_tree: Option<&str>, args: &[&str]) -> Outcome {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x");
    if let Some(wt) = work_tree {
        cmd.env("GIT_WORK_TREE", wt);
    }
    let out = cmd.output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn a_missing_work_tree_is_not_the_toplevel() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-merge-strategy-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    run(stock, &root, None, &["init", "-q", "-b", "main"]);
    run(stock, &root, None, &["commit", "-q", "--allow-empty", "-m", "one"]);
    run(stock, &root, None, &["branch", "feature"]);

    let cases: &[(&Path, Option<&str>, &[&str])] = &[
        (&root, Some("no-such-dir"), &["merge-octopus", "feature", "HEAD", "feature"]),
        (&root, Some("no-such-dir"), &["merge-resolve", "main", "--", "HEAD", "feature"]),
        (&root.join("sub"), None, &["merge-octopus", "feature", "HEAD", "feature"]),
        (&root.join("sub"), Some(".."), &["merge-resolve", "main", "--", "HEAD", "feature"]),
    ];
    for (dir, wt, args) in cases {
        let want = run(stock, dir, *wt, args);
        assert_eq!(want.2, Some(1), "stock refuses: {args:?} {wt:?} {want:?}");
        assert_eq!(run(BIN, dir, *wt, args), want, "{args:?} {wt:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
