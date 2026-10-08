//! Verbs against stock git when the global pathspec settings contradict each other.
//!
//! `init_pathspec_magic()` dies at the first pathspec element a command parses: `literal`
//! with `glob` or `icase` is incompatible, and so are `glob` with `noglob`. `literal` with
//! `noglob` is accepted. A command that parses no pathspec element never reaches it.
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
        .env_remove("GIT_LITERAL_PATHSPECS")
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS")
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

#[test]
fn contradictory_global_settings_die_at_the_first_pathspec() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-pathspec-global-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "one\n").unwrap();
    run(stock, &root, &[], &["add", "a"]);
    run(stock, &root, &[], &["commit", "-qm", "one"]);

    let combos: &[&[(&str, &str)]] = &[
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_GLOB_PATHSPECS", "1")],
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_ICASE_PATHSPECS", "1")],
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")],
        &[("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")],
        &[("GIT_GLOB_PATHSPECS", "1"), ("GIT_ICASE_PATHSPECS", "1")],
        &[("GIT_LITERAL_PATHSPECS", "0"), ("GIT_ICASE_PATHSPECS", "1")],
    ];
    let tails: &[&[&str]] = &[
        &["checkout", "nonesuch"],
        &["checkout", "--", "a"],
        &["checkout", "main"],
        &["reset", "a"],
        &["reset"],
        &["show", "--", "a"],
        &["show"],
        &["log", "-1", "--", "a"],
        &["log", "-1"],
        &["stash", "push", "--", "a"],
        &["stash", "list"],
        &["diff", "--", "a"],
        &["rm", "-n", "a"],
        &["clean", "-n", "a"],
    ];
    for envs in combos {
        for tail in tails {
            assert_eq!(
                run(BIN, &root, envs, tail),
                run(stock, &root, envs, tail),
                "{envs:?} {tail:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
