//! `git mailinfo <msg> <patch>` opens both files with `fopen(…, "w")` before it
//! reads a byte and lets stdio flush them when `mailinfo()` closes them: the
//! message first, the patch second, each at offset 0 of a file that already
//! exists. Given one name for both, the patch overwrites the front of the message
//! and the message's tail survives; a patch-less mail leaves the whole message.
//!
//! zvcs wrote each file by name, truncating, so the second write destroyed the
//! first — an empty file where git leaves the message.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn mailinfo(bin: &str, label: &str, args: &[&str], mail: &[u8]) -> (i32, Vec<u8>, Vec<u8>, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("zvcs-mailinfo-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let mut child = Command::new(bin)
        .arg("mailinfo")
        .args(args)
        .current_dir(&dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(mail).unwrap();
    let out = child.wait_with_output().unwrap();
    let read = |name: &str| std::fs::read(Path::new(&dir).join(name)).unwrap_or_default();
    let result = (out.status.code().expect("no signal"), out.stdout, read("one"), read("two"));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn same(label: &str, args: &[&str], mail: &[u8]) {
    let Some(stock) = stock_git() else { return };
    let want = mailinfo(stock, &format!("{label}-stock"), args, mail);
    let got = mailinfo(ZVCS, &format!("{label}-zvcs"), args, mail);
    assert_eq!(got, want, "git mailinfo {args:?}: left is zvcs, right is stock");
}

const WITH_PATCH: &[u8] = b"From: A U Thor <author@example.invalid>\nSubject: [PATCH] subject line\n\nbody text\n\nSigned-off-by: A U Thor <author@example.invalid>\n---\n a | 1 +\n\ndiff --git a/a b/a\n";
const NO_PATCH: &[u8] = b"From: A U Thor <author@example.invalid>\nSubject: subject line\n\nbody text\n\nSigned-off-by: A U Thor <author@example.invalid>\n";

#[test]
fn one_name_for_both_leaves_the_tail_of_the_message() {
    same("both-patch", &["one", "one"], WITH_PATCH);
}

#[test]
fn a_mail_without_a_patch_leaves_the_whole_message() {
    same("both-nopatch", &["one", "one"], NO_PATCH);
    same("both-nopatch-flags", &["-u", "-b", "-b", "--", "one", "one"], NO_PATCH);
}

#[test]
fn two_names_are_written_whole() {
    same("two", &["one", "two"], WITH_PATCH);
}
