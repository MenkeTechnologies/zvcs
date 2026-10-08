//! `git jump` against stock git: which configuration files its start-up reads.
//!
//! `jump` is an external command, so git.c reaches the repository configuration only
//! through `read_early_config()`, which never applies `extensions.worktreeConfig`: a
//! malformed `config.worktree` does not stop the usage paths of the script, while a
//! malformed `.git/config` does.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn only_the_common_config_is_read_before_the_script_runs() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-jump-early-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);
    // The stock under test has to ship the `git-jump` script, which is contrib.
    if run(stock, &root, &["jump", "bogus"]).1.contains("is not a git command") {
        return;
    }

    let config = root.join(".git/config");
    let original = std::fs::read_to_string(&config).unwrap();
    let tails: &[&[&str]] = &[
        &["jump"],
        &["jump", "bogus"],
        &["jump", "--bogus"],
        &["jump", "--stdout", "--stdout", "--stdout"],
        &["jump", "--stdout", "ws"],
        &["jump", "-h"],
    ];

    // A malformed `config.worktree` that `extensions.worktreeConfig` makes live.
    std::fs::write(&config, format!("{original}[extensions]\n\tworktreeConfig = true\n")).unwrap();
    std::fs::write(root.join(".git/config.worktree"), "garbage line\n").unwrap();
    for tail in tails {
        assert_eq!(run(BIN, &root, tail), run(stock, &root, tail), "worktree: {tail:?}");
    }

    // A malformed `.git/config` is read by every one of them.
    std::fs::remove_file(root.join(".git/config.worktree")).unwrap();
    std::fs::write(&config, format!("{original}garbage line\n")).unwrap();
    for tail in tails {
        assert_eq!(run(BIN, &root, tail), run(stock, &root, tail), "common: {tail:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
