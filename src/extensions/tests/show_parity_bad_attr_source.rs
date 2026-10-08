//! `git show` against stock git: the first attribute lookup's refusal of a bad
//! `--attr-source`.
//!
//! The patch and the count formats read each pair's `diff` attribute, which dies on a
//! `GIT_ATTR_SOURCE` naming no tree-ish (`compute_default_attr_source()`,
//! attr.c:1201-1228), while `--raw`, `--summary` and `--dirstat=files` never reach an
//! attribute; the default `--dirstat` reads one only for a pair modified in place, and
//! `--dirstat=lines` for every pair.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_ATTR_SOURCE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn fixture(stock: &str, name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-show-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "one\n").unwrap();
    run(stock, &root, &[], &["add", "a"]);
    run(stock, &root, &[], &["commit", "-qm", "one"]);
    std::fs::write(root.join("a"), "two\n").unwrap();
    std::fs::write(root.join("b"), "new\n").unwrap();
    run(stock, &root, &[], &["add", "a", "b"]);
    run(stock, &root, &[], &["commit", "-qm", "two"]);
    root
}

#[test]
fn a_bad_attr_source_dies_in_the_patch_and_count_formats_only() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = fixture(stock, "attr-source");
    let env = [("GIT_ATTR_SOURCE", "does-not-exist")];
    let cases: &[&[&str]] = &[
        &["show"],
        &["show", "--stat"],
        &["show", "--numstat"],
        &["show", "--shortstat"],
        &["show", "--check"],
        &["show", "--raw"],
        &["show", "--summary"],
        &["show", "--name-only"],
        &["show", "--dirstat"],
        &["show", "--dirstat=lines"],
        &["show", "--dirstat=files"],
        &["show", "--dirstat=cumulative"],
        &["show", "HEAD~1", "--dirstat"],
        &["show", "HEAD~1", "--dirstat=lines"],
        &["show", "HEAD~1", "--dirstat=files"],
        &["show", "--raw", "--stat"],
        &["show", "-s"],
        &["show", "HEAD~2"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, &env, args), run(stock, &root, &env, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
