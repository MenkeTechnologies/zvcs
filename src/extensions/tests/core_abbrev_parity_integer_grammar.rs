//! `core.abbrev` against stock git: the integer grammar of the value.
//!
//! `git_config_int()` is `strtoimax()` in base 0 plus a `k`/`m`/`g` unit, so `0x10` is
//! sixteen and `010` is eight; a length past the hash width prints the whole name, and
//! `blame` keeps a column for the `^` mark only below it.
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

fn fixture(stock: &str, name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    for (file, body) in [("a", "one\n"), ("b", "two\n")] {
        std::fs::write(root.join(file), body).unwrap();
        run(stock, &root, &["add", file]);
        run(stock, &root, &["commit", "-qm", file]);
    }
    root
}

#[test]
fn core_abbrev_reads_base_zero_integers() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = fixture(stock, "abbrev-grammar");
    let tails: &[&[&str]] = &[
        &["log", "-1", "--oneline"],
        &["rev-parse", "--short", "HEAD"],
        &["branch", "-v"],
        &["diff", "--raw", "HEAD~1"],
        &["blame", "a"],
    ];
    for value in ["0x10", "0X0a", "010", "1k", "99", "0x100", "12", "auto", "no", "0x3", "3"] {
        let config = format!("core.abbrev={value}");
        for tail in tails {
            let mut args = vec!["-c", config.as_str()];
            args.extend_from_slice(tail);
            assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

