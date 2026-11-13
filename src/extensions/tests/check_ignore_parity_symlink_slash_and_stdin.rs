//! `git check-ignore` against stock git for two gaps the parity corpus does not
//! reach:
//!
//! * `PATHSPEC_SYMLINK_LEADING_PATH` tests `item->match`, which
//!   `normalize_path_copy_len()` leaves with a trailing `/` when the argument
//!   ends in `/`, `.` or `..` (path.c:1106-1109, :1121-1204), so `link/`,
//!   `link/.` and `link/x/..` all die as "beyond a symbolic link".
//! * `check_ignore_stdin_paths()` handles one record at a time
//!   (builtin/check-ignore.c:128-153): the records before a badly quoted line
//!   are answered first, and an empty record dies with the pathspec error
//!   before a later bad quote is ever read.

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
        let root = std::env::temp_dir().join(format!("zvcs-ckign-symslash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo/d")).unwrap();
        std::fs::create_dir_all(root.join("repo/doc")).unwrap();
        let f = Fixture { root };
        let repo = f.root.join("repo");
        assert!(Command::new(stock).args(["init", "-q", "-b", "main"]).current_dir(&repo).status().unwrap().success());
        std::fs::write(repo.join(".gitignore"), "*.o\n!keep.o\nlink/\nbuild/\n").unwrap();
        std::os::unix::fs::symlink("d", repo.join("link")).unwrap();
        f
    }

    fn run(&self, bin: &str, cwd: &str, args: &[&str], input: &[u8]) -> (String, String, i32) {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(self.root.join(cwd))
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
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap(),
        )
    }

    fn same(&self, stock: &str, cwd: &str, args: &[&str], input: &[u8]) -> (String, String, i32) {
        let want = self.run(stock, cwd, args, input);
        assert_eq!(self.run(BIN, cwd, args, input), want, "git {args:?} in {cwd}");
        want
    }
}

#[test]
fn a_trailing_slash_dot_or_dotdot_keeps_the_symlink_a_leading_path() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "slash");
    for path in ["link/", "link/.", "link/x/..", "./link/", "link//"] {
        let (_, err, code) = f.same(stock, "repo", &["check-ignore", "-v", "-n", path], b"");
        assert_eq!((err, code), (format!("fatal: pathspec '{path}' is beyond a symbolic link\n"), 128));
    }
    f.same(stock, "repo/doc", &["check-ignore", "-v", "-n", "../link/", "../link"], b"");
    // `link` itself and a path inside the real directory are matched normally.
    f.same(stock, "repo", &["check-ignore", "-v", "-n", "link", "build/."], b"");
}

#[test]
fn stdin_records_are_answered_one_at_a_time() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "stdin");
    let (out, err, code) = f.same(stock, "repo", &["check-ignore", "--stdin", "-v", "-n"], b"x.o\nkeep.o\n\"bad\nlater.o\n");
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (".gitignore:1:*.o\tx.o\n.gitignore:2:!keep.o\tkeep.o\n", "fatal: line is badly quoted\n", 128)
    );
    let (out, err, _) = f.same(stock, "repo", &["check-ignore", "--stdin", "-v", "-n"], b"x.o\n\n\"bad\n");
    assert_eq!(out, ".gitignore:1:*.o\tx.o\n");
    assert!(err.starts_with("fatal: empty string is not a valid pathspec"), "{err}");
    f.same(stock, "repo", &["check-ignore", "--stdin", "-v", "-n"], b"x.o\nlink/\nkeep.o\n");
}
