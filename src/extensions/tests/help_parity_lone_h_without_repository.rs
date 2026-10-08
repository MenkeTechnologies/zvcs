//! `git <cmd> -h` against stock git when no repository can be opened.
//!
//! `run_builtin()` demotes `RUN_SETUP` to `RUN_SETUP_GENTLY` for a lone `-h`
//! (git.c:474-477), so the usage comes out where setup would otherwise die: with
//! `--git-dir` naming a directory that is no repository, and outside any repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, ceiling: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn a_lone_h_is_answered_without_a_repository() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-help-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("work");
    std::fs::create_dir_all(&root).unwrap();

    let verbs = [
        "status", "log", "show", "rev-parse", "checkout", "blame", "annotate", "mv", "stash", "reset",
        "gc", "worktree", "for-each-ref", "read-tree", "backfill", "multi-pack-index", "diff-pairs",
        "diff", "add", "commit", "shortlog", "unpack-file",
    ];
    for verb in verbs {
        let tails: [&[&str]; 2] = [
            &["--git-dir=no-such", verb, "-h"],
            &[verb, "-h"],
        ];
        for args in tails {
            assert_eq!(
                run(BIN, &root, &base, args),
                run(stock, &root, &base, args),
                "{args:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
