//! `git show` against stock git: the options that turn the patch on.
//!
//! `-U<n>`/`--unified=<n>` (`diff_opt_unified()`, diff.c:5961) and `--binary`
//! (`diff_opt_binary()`, diff.c:5564) both end in `enable_patch_output()`, so they ask
//! for a patch even after `--quiet`, which only pre-sets the no-output bit; `-s` after
//! them still wins.
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
fn unified_and_binary_enable_the_patch_after_quiet() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = fixture(stock, "quiet");
    let cases: &[&[&str]] = &[
        &["show", "--quiet"],
        &["show", "--quiet", "-U0"],
        &["show", "-U0", "--quiet"],
        &["show", "--quiet", "--unified=1"],
        &["show", "--quiet", "--binary"],
        &["show", "--stat", "--quiet", "-U0"],
        &["show", "-s", "-U0"],
        &["show", "-U0", "-s"],
        &["show", "--quiet", "-W"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, &[], args), run(stock, &root, &[], args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

