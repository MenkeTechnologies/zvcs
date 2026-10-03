//! `git merge` with `--` in its argv, against stock git: `parse_options()`
//! without `PARSE_OPT_KEEP_DASHDASH` consumes the `--` and takes every later
//! word as a head (builtin/merge.c), so `merge -- topic` merges and
//! `merge -- --continue` names a head called `--continue`. A `--` inside
//! `branch.<name>.mergeoptions` ends options only within those words.
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

fn fixture(stock: &str, root: &Path) {
    let git = |args: &[&str]| run(stock, root, args);
    std::fs::create_dir_all(root).unwrap();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "one"]);
    git(&["branch", "topic"]);
    git(&["tag", "v1"]);
    git(&["checkout", "-q", "topic"]);
    std::fs::write(root.join("g"), "g\n").unwrap();
    git(&["add", "g"]);
    git(&["commit", "-qm", "topic"]);
    git(&["checkout", "-q", "main"]);
}

#[test]
fn dashdash_ends_options() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-merge-dashdash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    let cases: [(&[&str], &[&str]); 6] = [
        (&[], &["merge", "--", "topic"]),
        (&[], &["merge", "--ff-only", "--", "HEAD"]),
        (&[], &["merge", "--", "HEAD", "--continue", "v1"]),
        (&[], &["merge", "--no-ff", "--", "topic", "-m", "x"]),
        (&["config", "branch.main.mergeoptions", "-- --no-ff"], &["merge", "topic"]),
        (&["config", "branch.main.mergeoptions", "--no-ff --"], &["merge", "--ff", "topic"]),
    ];
    for (i, (setup, args)) in cases.iter().enumerate() {
        let mut sides = Vec::new();
        for (side, bin) in [("s", stock), ("z", BIN)] {
            let root = base.join(format!("{i}{side}"));
            fixture(stock, &root);
            if !setup.is_empty() {
                run(stock, &root, setup);
            }
            let out = run(bin, &root, args);
            let state = run(stock, &root, &["log", "--all", "--format=%H %P %s"]);
            let merging = root.join(".git/MERGE_HEAD").exists();
            sides.push((out, state, merging));
        }
        assert_eq!(sides[1], sides[0], "{setup:?} {args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
