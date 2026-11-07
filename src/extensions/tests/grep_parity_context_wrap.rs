//! `git grep -C <n>`, `--context=<n>` and `-<n>` as `context_callback()` reads them.
//!
//! The callback (builtin/grep.c:920-938) runs `strtol()` into an `int` and stores the
//! result in the `unsigned pre_context`/`post_context` of `struct grep_opt`
//! (grep.h:170-171): an empty value is 0, leading blanks and a sign are taken, a
//! value past `long` saturates and is then narrowed, so `-C -1` and
//! `-C 9999999999999999999999` are `UINT_MAX` and `-C 4294967296` is 0. The line loop
//! adds `post_context` to the last hit in `unsigned` arithmetic (grep.c:1668, :1757),
//! which wraps: `UINT_MAX` of context shows every line before a hit and none after it.
//! zvcs refused the empty and blank-led values, took `-C -1` as unlimited context both
//! ways, and panicked on the overflow under `-W`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-grepctx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("a.txt"), "one\ntwo foo\nthree\nfour\nfive foo\nsix\n").unwrap();
    std::fs::write(
        dir.join("b.c"),
        "int x;\n\nint main(void)\n{\n\tint foo = 1;\n\treturn foo;\n}\n\nstatic void h(int a)\n{\n\tputs(\"foo\");\n}\n",
    )
    .unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    dir
}

#[test]
fn context_values_are_strtol_into_unsigned() {
    let Some(stock) = stock_git() else { return };
    let dir = fixture("context", stock);
    let cases: &[&[&str]] = &[
        &["grep", "-C", "-1", "foo"],
        &["grep", "-n", "--context=-1", "foo"],
        &["grep", "-C", "-2", "foo", "--", "a.txt"],
        &["grep", "-C", "", "foo"],
        &["grep", "-C", " 1", "foo"],
        &["grep", "-C", "+1", "foo"],
        &["grep", "-C", "1 ", "foo"],
        &["grep", "-C", "4294967296", "foo"],
        &["grep", "-C", "-4294967297", "foo"],
        &["grep", "-C", "9999999999999999999999", "foo"],
        &["grep", "-4294967296", "foo"],
        &["grep", "-4294967295", "foo"],
        &["grep", "-A", "4294967295", "foo", "--", "a.txt"],
        &["grep", "-C", "-1", "-m1", "foo"],
        &["grep", "-C", "-1", "-W", "foo"],
        &["grep", "-C", "-1", "-p", "foo"],
        &["grep", "-C", "-1", "--heading", "--break", "foo"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &dir, args), run(stock, &dir, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `compile_pattern_or()`'s `die("not a pattern expression %s", p->pattern)`
/// (grep.c:682-688) names the token the second operand was to start at.
#[test]
fn not_a_pattern_expression_names_the_token() {
    let Some(stock) = stock_git() else { return };
    let dir = fixture("expr", stock);
    let cases: &[&[&str]] = &[
        &["grep", "-e", "foo", "(", ")"],
        &["grep", "-e", "foo", "(", ")", "-e", "x"],
        &["grep", "-e", "foo", "--not", "(", ")"],
        &["grep", "-e", "foo", ")"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &dir, args), run(stock, &dir, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
