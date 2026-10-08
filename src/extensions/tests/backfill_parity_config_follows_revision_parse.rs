//! `git backfill` against stock git: the order the configuration is read in.
//!
//! `cmd_backfill()` runs `parse_options()` and `setup_revisions()` first, so an
//! unrecognised argument, an unknown revision and `--filter` without `--objects` beat
//! every bad configuration value. It then reads `git_default_config`, loads the sparse
//! patterns, reads the settings block (`prepare_repo_settings()`) and only then reaches
//! `prepare_revision_walk()`'s `--ancestry-path` refusal.
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
fn revision_errors_beat_config_and_config_beats_the_walk() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-backfill-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);

    let configs: &[&[&str]] = &[
        &[],
        &["-c", "core.createObject=bogus"],
        &["-c", "core.packedGitLimit=bogus"],
        &["-c", "core.commitGraph=bogus"],
        &["-c", "core.createObject=bogus", "-c", "core.packedGitLimit=bogus"],
    ];
    let tails: &[&[&str]] = &[
        &["backfill"],
        &["backfill", "--bogus"],
        &["backfill", "--min-batch-size=abc"],
        &["backfill", "extra"],
        &["backfill", "--filter=blob:none"],
        &["backfill", "--ancestry-path"],
        &["backfill", "--sparse"],
        &["backfill", "--no-sparse"],
        &["backfill", "-h"],
    ];
    for config in configs {
        for tail in tails {
            let mut args: Vec<&str> = config.to_vec();
            args.extend_from_slice(tail);
            assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
        }
    }

    // A configuration file value `git_default_config` refuses, as opposed to `-c`.
    std::fs::write(
        root.join(".git/config"),
        std::fs::read_to_string(root.join(".git/config")).unwrap() + "[core]\n\tabbrev\n",
    )
    .unwrap();
    for tail in tails {
        assert_eq!(run(BIN, &root, tail), run(stock, &root, tail), "{tail:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
