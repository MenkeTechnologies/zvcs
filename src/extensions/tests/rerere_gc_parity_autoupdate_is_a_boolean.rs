//! `git rerere` and the `rerere gc` child of `git gc` against stock git: an unreadable
//! `rerere.autoUpdate`.
//!
//! `git_rerere_config()` reads `rerere.enabled` and `rerere.autoupdate` through
//! `git_config_bool()` in configuration order, so the first value of either that is not a
//! boolean (including an integer past `int` range) dies with `bad boolean config value`.
//! When `gc` is the caller, `run_command(&rerere)` returning non-zero adds
//! `fatal: failed to run rerere` and the run ends at 128.
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
fn a_non_boolean_autoupdate_or_enabled_dies_in_every_rerere_verb() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-rerere-bool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);
    std::fs::create_dir_all(root.join(".git/rr-cache")).unwrap();

    let configs: &[&[&str]] = &[
        &[],
        &["-c", "rerere.autoUpdate=bogus"],
        &["-c", "rerere.autoUpdate=99999999999999999999999999"],
        &["-c", "rerere.autoUpdate=true"],
        &["-c", "rerere.autoUpdate=2"],
        &["-c", "rerere.enabled=bogus"],
        &["-c", "rerere.enabled=true", "-c", "rerere.autoUpdate=bogus"],
        &["-c", "rerere.autoUpdate=bogus", "-c", "rerere.autoUpdate=true"],
    ];
    let tails: &[&[&str]] = &[
        &["rerere", "status"],
        &["rerere", "gc"],
        &["rerere", "diff"],
        &["rerere", "clear"],
        &["rerere", "remaining"],
        &["rerere"],
        &["gc", "--quiet"],
    ];
    for config in configs {
        for tail in tails {
            let mut args: Vec<&str> = config.to_vec();
            args.extend_from_slice(tail);
            assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
        }
    }

    // `gc` runs the `rerere gc` child whether or not `rr-cache` exists.
    std::fs::remove_dir_all(root.join(".git/rr-cache")).unwrap();
    for config in configs {
        let mut args: Vec<&str> = config.to_vec();
        args.extend_from_slice(&["gc", "--quiet"]);
        assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
