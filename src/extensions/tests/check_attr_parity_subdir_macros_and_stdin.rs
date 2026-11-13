//! `git check-attr` against stock git for three things the parity corpus does
//! not reach:
//!
//! * `parse_attr_line()` refuses a `[attr]` line in any `.gitattributes` below
//!   the top of the work tree — `READ_ATTR_MACRO_OK` is set only for the root,
//!   info, global and system files (attr.c:266, :348-354, :915) — and says so
//!   on stderr as `<name> not allowed: <src>:<lineno>`, where `<name>` is the
//!   rest of an unquoted line. A bare `[attr]` is not a macro at all.
//! * `check_attr_stdin_paths()` answers and flushes each record before reading
//!   the next (builtin/check-attr.c:82-102): a badly quoted line dies after the
//!   answers to the lines before it, and a driver on a pipe gets each answer
//!   without closing stdin.
//! * the empty path is the work-tree root, answered from the root frames even
//!   after a deeper path was looked up.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::{BufRead, BufReader, Write};
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
        let root = std::env::temp_dir().join(format!("zvcs-ckattr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo/sub/deep")).unwrap();
        let f = Fixture { root };
        let repo = f.repo();
        assert!(Command::new(stock).args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());
        std::fs::write(repo.join(".gitattributes"), "* text\n*.c diff=cpp\n[attr]top -text\n*.t top\n").unwrap();
        std::fs::write(
            repo.join("sub/.gitattributes"),
            "*.c -bad\n\"[attr]q\" y\n[attr]-inv x\n  [attr]sp z \n[attr] onechar\n[attr]m3 -m3\n*.c deep\n",
        )
        .unwrap();
        f
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn command(&self, bin: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(self.repo())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_ATTR_SOURCE")
            .env_remove("GIT_DIR");
        cmd
    }

    fn run(&self, bin: &str, args: &[&str], input: &[u8]) -> (String, String, i32) {
        let mut child = self
            .command(bin, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap(),
        )
    }

    fn same(&self, stock: &str, args: &[&str], input: &[u8]) -> (String, String, i32) {
        let want = self.run(stock, args, input);
        assert_eq!(self.run(BIN, args, input), want, "git {args:?} <<< {:?}", String::from_utf8_lossy(input));
        want
    }
}

#[test]
fn subdirectory_macro_lines_are_refused_with_their_text() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "macro");
    let (out, err, _) = f.same(stock, &["check-attr", "-a", "sub/a.c", "sub/a.t", "sub/a"], b"");
    assert!(err.contains("[attr]sp z  not allowed: sub/.gitattributes:4\n"), "{err}");
    assert!(err.contains("[attr]q not allowed: sub/.gitattributes:2\n"), "{err}");
    // `[attr]` alone is a one-character class pattern, so `sub/a` gets `onechar`.
    assert!(out.contains("sub/a: onechar: set\n"), "{out}");
}

#[test]
fn stdin_answers_before_a_badly_quoted_line_and_the_empty_path() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "stdin");
    let (out, err, code) = f.same(stock, &["check-attr", "--stdin", "text", "diff"], b"sub/deep/x.c\n\n\"bad\nb.c\n");
    assert_eq!(code, 128);
    assert!(out.starts_with("sub/deep/x.c: text: set\n"), "{out}");
    assert!(out.contains("\n: text: set\n: diff: unspecified\n"), "{out}");
    assert!(err.ends_with("fatal: line is badly quoted\n"), "{err}");
    f.same(stock, &["check-attr", "-a", "sub/deep/x.c", "", ".", "sub/.."], b"");
    f.same(stock, &["check-attr", "--stdin", "-z", "-a"], b"sub/deep/x.c\0\0a.c\0");
}

/// A driver that keeps stdin open gets the answer to each line as it asks.
#[test]
fn stdin_answers_each_line_while_stdin_stays_open() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "pipe");
    let mut child = f
        .command(BIN, &["check-attr", "--stdin", "diff"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    for (path, want) in [("x.c", "x.c: diff: cpp"), ("y.h", "y.h: diff: unspecified")] {
        writeln!(stdin, "{path}").unwrap();
        stdin.flush().unwrap();
        // The bound is only ever reached when the answer never comes.
        let line = rx.recv_timeout(std::time::Duration::from_secs(30));
        if line.is_err() {
            let _ = child.kill();
        }
        assert_eq!(line.as_deref(), Ok(want));
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
