//! `git var` with the wrong number of operands, against stock git:
//! `cmd_var()` answers `argc != 2` with `usage()` before it reads any
//! configuration (builtin/var.c:225-234), so a value `git_default_config()`
//! would refuse cannot pre-empt the usage line. `-l` prints each value before
//! `show_config()` hands it to `git_default_config()` (builtin/var.c:207-215),
//! so the listing stops right after the value that is refused.
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

#[test]
fn wrong_arity_is_usage_before_config() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let root = std::env::temp_dir().join(format!("zvcs-var-usage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q"]);
    let bad = ["-c", "push.default=bad", "-c", "core.createObject=bogus", "var"];
    for tail in [&[][..], &["-l", "x"], &["-h", "x"], &["-l"], &["GIT_EDITOR"]] {
        let args: Vec<&str> = bad.iter().chain(tail).copied().collect();
        assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
    }
    std::fs::write(
        root.join(".git/config"),
        std::fs::read_to_string(root.join(".git/config")).unwrap()
            + "[core]\n\tcreateObject = bogus\n[x]\n\ty = 1\n",
    )
    .unwrap();
    let args = ["-c", "a.b=c", "var", "-l"];
    assert_eq!(run(BIN, &root, &args), run(stock, &root, &args), "{args:?}");
    let _ = std::fs::remove_dir_all(&root);
}
