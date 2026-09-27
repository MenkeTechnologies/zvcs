//! `git log --stdin` read its lines as bare operands.
//!
//! `read_revisions_from_stdin()` (revision.c:2937-2983) runs inside
//! `setup_revisions()`'s loop at the `--stdin` position and:
//!
//! - hands a line starting with `-` to `handle_revision_pseudo_opt()`, so
//!   `--all`, `--not`, `--branches=<glob>` work there, and dies
//!   `invalid option '<line>' in --stdin mode` for anything else;
//! - passes every other line with `REVARG_CANNOT_BE_FILENAME`, so a line that
//!   does not resolve is `bad revision '<line>'` even when a file of that name
//!   exists, never a pathspec or the three-line `ambiguous argument` advice;
//! - stops at the first empty line;
//! - honours its own `--end-of-options`, past which `--all` is a revision.
//!
//! zvcs's `log` appended every non-empty line to the operands after the scan.
//! `rev-list` already did all of this; both now share one reader.
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
    /// `main`: A then B; `topic` forks at A and adds T.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-stdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], "");
        for (branch, file, msg) in [("main", "a", "A"), ("topic", "t", "T"), ("main", "b", "B")] {
            if branch == "topic" {
                f.run(&["checkout", "-q", "-b", "topic"], "");
            } else {
                f.run(&["checkout", "-q", "main"], "");
            }
            std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
            f.run(&["add", file], "");
            f.run(&["commit", "-q", "-m", msg], "");
        }
        f
    }

    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
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
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn log(&self, stdin: &str) -> (String, String, i32) {
        self.run(&["log", "--format=%s", "--stdin"], stdin)
    }
}

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

fn fatal(msg: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: {msg}\n"), 128)
}

#[test]
fn pseudo_options_on_stdin() {
    let f = Fixture::new("pseudo");
    assert_eq!(f.log("--all\n"), ok("B\nT\nA\n"));
    assert_eq!(f.log("topic\n--not\n--branches=m*\n"), ok("T\n"));
    assert_eq!(f.log("--oneline\n"), fatal("invalid option '--oneline' in --stdin mode"));
    // Past the block's own `--end-of-options`, `--all` is a revision name.
    assert_eq!(f.log("--end-of-options\n--all\n"), fatal("bad revision '--all'"));
}

#[test]
fn a_line_is_never_a_pathspec_and_an_empty_one_ends_the_input() {
    let f = Fixture::new("lines");
    assert_eq!(f.log("nosuch\n"), fatal("bad revision 'nosuch'"));
    // `a` is a file in the worktree; on argv it would become a pathspec.
    assert_eq!(f.log("a\n"), fatal("bad revision 'a'"));
    assert_eq!(f.log("topic\n\nmain\n"), ok("T\nA\n"));
    assert_eq!(f.log("topic\n--\nt\n"), ok("T\n"));
}
