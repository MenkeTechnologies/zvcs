//! `git column` sizes its cells with `item_length()`, which is
//! `utf8_strnwidth(s, strlen(s), 1)` (column.c:27-30): `git_wcwidth()`'s columns,
//! so a CJK glyph is two wide and a combining mark none, SGR sequences skipped,
//! and a cell that is not valid UTF-8 its byte length. zvcs counted one column
//! per character, so a wide or combining cell laid the table out differently.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::process::{Command, Stdio};

use stock_git::stock_git;

fn column(bin: &str, args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let mut child = Command::new(bin)
        .args(["-c", "column.ui=never", "column"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.stdout, out.stderr, out.status.code())
}

#[test]
fn cells_are_measured_in_display_columns() {
    let Some(stock) = stock_git() else { return };
    let zvcs = env!("CARGO_BIN_EXE_git");
    let inputs: [&[u8]; 3] = [
        "héllo\nwörld\n日本語テキスト\nx\tTab\nplain\n".as_bytes(),
        "e\u{301}\u{301}\nwide日本\n\x1b[31mred\x1b[m\nxx\n".as_bytes(),
        b"a\xffb\nok\n\xe6\x97\xa5\nzz\n",
    ];
    for input in inputs {
        for args in [
            &["--mode=column", "--width=20"][..],
            &["--mode=row", "--width=20"][..],
            &["--mode=column,dense", "--width=25"][..],
            &["--mode=column", "--padding=0", "--width=12", "--indent=日本"][..],
        ] {
            assert_eq!(column(zvcs, args, input), column(stock, args, input), "{args:?} {input:?}");
        }
    }
}
