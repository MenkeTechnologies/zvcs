//! `git check-mailmap` against stock git for two parsing details:
//!
//! * `--stdin` reads with `strbuf_getline_lf()` (builtin/check-mailmap.c:78),
//!   which strips the LF alone: a CRLF line keeps its CR, so a contact without
//!   `<...>` is printed back as `<plain@example.com\r>`.
//! * `split_ident_line()` trims the name with git's own `isspace()`
//!   (ident.c:292-296), whose `GIT_SPACE` class (ctype.c:20-23) is SP, TAB, LF
//!   and CR only — a VT or FF before the `<` stays part of the name.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ckmailmap-cr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let f = Fixture { root };
        let repo = f.root.join("repo");
        assert!(Command::new(stock).args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());
        std::fs::write(repo.join(".mailmap"), "Proper <proper@example.com> <old@example.com>\n").unwrap();
        f
    }

    fn run(&self, bin: &str, args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(self.root.join("repo"))
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.stdout, out.stderr, out.status.code().unwrap())
    }

    fn same(&self, stock: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
        let want = self.run(stock, args, input);
        assert_eq!(self.run(BIN, args, input), want, "git {args:?} <<< {:?}", String::from_utf8_lossy(input));
        want.0
    }
}

#[test]
fn stdin_keeps_the_carriage_return() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "cr");
    let out = f.same(stock, &["check-mailmap", "--stdin"], b"A <old@example.com>\r\nplain@example.com\r\n\nlast <old@example.com>");
    assert_eq!(out, b"Proper <proper@example.com>\n<plain@example.com\r>\n<>\nProper <proper@example.com>\n");
}

#[test]
fn vertical_tab_and_form_feed_stay_in_the_name() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "space");
    let out = f.same(stock, &["check-mailmap", "VT\x0b<x@y>", "FF\x0c <x@y>", "Tab\t<x@y>", "Proper\x0b<old@example.com>"], b"");
    assert_eq!(out, b"VT\x0b <x@y>\nFF\x0c <x@y>\nTab <x@y>\nProper <proper@example.com>\n");
}
