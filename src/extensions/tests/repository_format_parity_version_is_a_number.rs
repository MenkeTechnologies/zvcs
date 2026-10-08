//! Verbs against stock git when `core.repositoryformatversion` is not an integer.
//!
//! `read_repository_format()` (setup.c:866-876) reads the version with `git_config_int()`,
//! which dies inside the config reader with `bad numeric config value '<v>' for
//! 'core.repositoryformatversion' in file .git/config: invalid unit` (or `out of range`).
//! That is not a verdict about the format, so it applies to the `RUN_SETUP_GENTLY` verbs as
//! well as to the `RUN_SETUP` ones. A negative version is a number and is accepted by git (not covered: gix opens a repository only for an unsigned one).
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
fn an_unreadable_version_dies_in_the_config_reader() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-repo-version-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "one\n").unwrap();
    run(stock, &root, &["add", "a"]);
    run(stock, &root, &["commit", "-qm", "one"]);
    let config = root.join(".git/config");
    let original = std::fs::read_to_string(&config).unwrap();
    let original = original.replace("repositoryformatversion = 0", "");

    let tails: &[&[&str]] = &[
        &["status"],
        &["log", "-1", "--oneline"],
        &["rev-parse", "HEAD"],
        &["rev-parse", "--git-dir"],
        &["checkout", "main"],
        &["branch"],
        &["stash", "list"],
        &["diff"],
        &["mv", "-n", "a", "b"],
        &["worktree", "list"],
        &["config", "--list"],
        &["var", "GIT_AUTHOR_IDENT"],
    ];
    for value in ["", "bogus", "1x", "99999999999999999999", "0", "1"] {
        std::fs::write(&config, format!("{original}[core]\n\trepositoryformatversion = {value}\n")).unwrap();
        for tail in tails {
            assert_eq!(run(BIN, &root, tail), run(stock, &root, tail), "{value:?} {tail:?}");
        }
        // From a subdirectory the message still names `.git/config`.
        let sub = root.join("src");
        assert_eq!(run(BIN, &sub, &["status"]), run(stock, &sub, &["status"]), "{value:?} from src");
    }
    let _ = std::fs::remove_dir_all(&root);
}
