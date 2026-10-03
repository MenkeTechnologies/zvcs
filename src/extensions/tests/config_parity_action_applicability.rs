//! `git config`'s legacy option checks, against stock git (builtin/config.c,
//! `cmd_config_actions()`): `--show-origin` only with a reader and `--comment`
//! only with a writer that adds or sets, both refused with exit 129 between
//! `--name-only` and `--fixed-value`; and each writer's `check_argc()` window.
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
    std::fs::create_dir_all(root).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "a.b", "1"],
        &["config", "--add", "a.b", "2"],
        &["config", "s.k", "v"],
    ] {
        run(stock, root, args);
    }
}

/// Runs `args` on a fresh fixture per side and compares the output and the
/// configuration left behind.
fn compare(stock: &str, base: &Path, cases: &[&[&str]]) {
    for (i, args) in cases.iter().enumerate() {
        let mut sides = Vec::new();
        for (side, bin) in [("s", stock), ("z", BIN)] {
            let root = base.join(format!("{i}{side}"));
            fixture(stock, &root);
            let out = run(bin, &root, args);
            let config = std::fs::read_to_string(root.join(".git/config")).unwrap();
            sides.push((out, config));
        }
        assert_eq!(sides[1], sides[0], "{args:?}");
    }
    let _ = std::fs::remove_dir_all(base);
}

fn base(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("zvcs-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

#[test]
fn show_origin_and_comment_are_refused_outside_their_actions() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    compare(
        stock,
        &base("applicable"),
        &[
            &["config", "--show-origin", "a.b", "3"],
            &["config", "--show-origin", "--unset", "a.b"],
            &["config", "--show-origin", "--get-urlmatch", "a.b", "http://x"],
            &["config", "--show-origin", "--get-color", "a.b"],
            &["config", "--show-origin", "--default", "x", "a.b", "1", "2"],
            &["config", "--show-origin", "a.b"],
            &["config", "--comment", "x", "--unset", "a.b"],
            &["config", "--comment", "x", "a.b"],
            &["config", "--comment", "x", "a.c", "3"],
            &["config", "--fixed-value", "--comment", "x", "--get", "a.b"],
        ],
    );
}

#[test]
fn writers_check_their_operand_count() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    compare(
        stock,
        &base("argc"),
        &[
            &["config", "--remove-section", "s", "a"],
            &["config", "--remove-section"],
            &["config", "--unset", "a.b", "1", "2"],
            &["config", "--unset-all", "a.b", "1", "2"],
            &["config", "--add", "a.c"],
            &["config", "--add", "a.c", "1", "2"],
            &["config", "--unset", "a.b", "1"],
            &["config", "remove-section", "s", "a"],
        ],
    );
}
