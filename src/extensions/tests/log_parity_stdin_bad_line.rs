//! `log --stdin`: a line that does not resolve dies as it is read.
//!
//! `read_revisions_from_stdin()` handles every line on the spot —
//! `if (handle_revision_arg(sb.buf, revs, flags, REVARG_CANNOT_BE_FILENAME))
//! die("bad revision '%s'", sb.buf);` (revision.c:2960-2976) — while
//! `setup_revisions()` is still at the `--stdin` argument (revision.c:3047-3057).
//! So a bad line is reported ahead of anything later on the command line: a
//! second `--stdin`, an unknown option, and an `--ignore-missing` that has not
//! been read yet. zvcs read the lines and resolved them only after the whole
//! command line, reporting the later problem instead.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-stdin-bad-line-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], "");
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.run(&["add", "a"], "");
        f.run(&["commit", "-q", "-m", "one"], "");
        f
    }

    fn run(&self, args: &[&str], input: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

fn bad(line: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: bad revision '{line}'\n"), 128)
}

#[test]
fn a_bad_line_beats_a_second_stdin() {
    let f = Fixture::new("twice");
    assert_eq!(f.run(&["log", "--stdin", "--stdin"], "bogus\n"), bad("bogus"));
    assert_eq!(f.run(&["log", "--stdin", "--stdin"], "main\nbogus\n"), bad("bogus"));
    // With every line good, the second `--stdin` is what stops it.
    assert_eq!(
        f.run(&["log", "--stdin", "--stdin"], "main\n"),
        (String::new(), "fatal: --stdin given twice?\n".to_string(), 128)
    );
}

#[test]
fn a_bad_line_beats_a_later_unknown_option() {
    let f = Fixture::new("option");
    assert_eq!(f.run(&["log", "--stdin", "--bogus"], "bogus\n"), bad("bogus"));
}

#[test]
fn ignore_missing_counts_only_when_read_first() {
    let f = Fixture::new("ignore");
    assert_eq!(f.run(&["log", "--stdin", "--ignore-missing"], "bogus\n"), bad("bogus"));
    assert_eq!(
        f.run(&["log", "--ignore-missing", "--stdin"], "bogus\n"),
        (String::new(), String::new(), 0)
    );
}
