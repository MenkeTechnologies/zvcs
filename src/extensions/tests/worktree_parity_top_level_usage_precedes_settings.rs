//! `git worktree` against stock git: the top-level usage errors precede the settings
//! block.
//!
//! `cmd_worktree()` reads `git_default_config` first, then runs a top-level
//! `parse_options()` that refuses a missing subcommand, `--`, an option ahead of the
//! subcommand and a name that is not a subcommand. Only a dispatched subcommand reaches
//! `prepare_repo_settings()`, so a bad `core.commitGraph` loses to those usage errors and
//! wins over everything a subcommand says, its own usage errors included.
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
fn only_a_dispatched_subcommand_reads_the_settings() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-wt-top-usage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);

    let tails: &[&[&str]] = &[
        &["worktree"],
        &["worktree", "--"],
        &["worktree", "--guess-remote"],
        &["worktree", "-x"],
        &["worktree", "-"],
        &["worktree", "bogus"],
        &["worktree", "-h"],
        &["worktree", "list"],
        &["worktree", "list", "--bogus"],
        &["worktree", "add"],
        &["worktree", "prune", "-h"],
        &["worktree", "lock"],
    ];
    for key in ["core.commitGraph=bogus", "core.createObject=bogus"] {
        for tail in tails {
            let mut args = vec!["-c", key];
            args.extend_from_slice(tail);
            assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
