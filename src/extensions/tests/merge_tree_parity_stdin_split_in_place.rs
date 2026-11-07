//! `git merge-tree --stdin` against stock git on how an input line is split.
//!
//! builtin/merge-tree.c:628-629 splits each line in place with
//! `string_list_split_in_place_f(&split, buf.buf, " ", -1,
//! STRING_LIST_SPLIT_TRIM)` (string-list.c:297-388): every field starts past
//! leading whitespace, so a run of spaces does not make an empty field, while a
//! trailing space does. The `malformed input line` die() then quotes `buf.buf`,
//! which the split has cut at the first field's end.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), out.stdout, out.stderr)
}

#[test]
fn input_lines_split_as_string_list_split_in_place_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-merge-tree-stdin-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["commit", "-q", "--allow-empty", "-m", "base"],
        &["branch", "side"],
        &["commit", "-q", "--allow-empty", "-m", "main"],
    ] {
        assert_eq!(git(stock, &dir, args, b"").0, 0, "{args:?}");
    }
    for input in [
        "main  side\n",
        " main side\n",
        "main side \n",
        "main\tside\n",
        "x\n",
        "\n",
        "  x\n",
        "main -- side\n",
        "--merge-base=main main side\n",
        "main  --  main  side\n",
        "a b c\n",
    ] {
        let s = git(stock, &dir, &["merge-tree", "--stdin"], input.as_bytes());
        let z = git(ZVCS, &dir, &["merge-tree", "--stdin"], input.as_bytes());
        assert_eq!(z, s, "{input:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
