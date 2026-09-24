//! A threaded `git grep` does not decide per file whether a hunk mark is owed.
//! Each file renders into its own buffer with `show_hunk_mark` already set
//! (`if (opt->output != std_output) opt->show_hunk_mark = 1;`, grep.c:1598-1599),
//! and `work_done()` then drops the first line of the whole output:
//!
//! ```c
//! /* Skip the leading hunk mark of the first file. */
//! if (skip_first_line) {
//!         while (len) {
//!                 len--;
//!                 if (*p++ == '\n')
//!                         break;
//!         }
//!         skip_first_line = 0;
//! }
//! ```
//! (builtin/grep.c:163-171, armed at 1347-1350 for context, `--break` and `-W`)
//!
//! That is the single-threaded answer only while the first line really is a
//! mark. A `Binary file … matches` notice carries none, so when it comes first
//! it is the notice that is dropped, and the next file keeps its own `--` / blank
//! line. `--threads 1` is the single-threaded control. Every expectation was
//! measured against git 2.55.0 on this exact fixture.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    /// A matching binary file that sorts ahead of a matching text file.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-grepskip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git(&["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.join("a.bin"), b"\x00needle here\n").unwrap();
        std::fs::write(f.repo.join("b.txt"), "one\nneedle\nthree\n").unwrap();
        f.git(&["add", "a.bin", "b.txt"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, args: &[&str]) {
        assert!(self.cmd(args).status().unwrap().success(), "git {args:?} failed");
    }

    /// `git grep --threads <n> <extra…>`: stdout and exit code.
    fn grep(&self, threads: &str, extra: &[&str]) -> (String, Option<i32>) {
        let mut args = vec!["grep", "--threads", threads];
        args.extend_from_slice(extra);
        let out = self.cmd(&args).output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_leading_binary_notice_is_the_line_a_threaded_run_drops() {
    let f = Fixture::new("notice");
    // The only output is the notice, so nothing is printed — yet the file
    // matched, so the exit code is still 0.
    assert_eq!(f.grep("2", &["-W", "needle", "--", "a.bin"]), (String::new(), Some(0)));
    assert_eq!(f.grep("2", &["-W", "needle"]), ("--\nb.txt:needle\n".into(), Some(0)));
    assert_eq!(
        f.grep("2", &["-C1", "needle"]),
        ("--\nb.txt-one\nb.txt:needle\nb.txt-three\n".into(), Some(0))
    );
    assert_eq!(f.grep("2", &["--break", "needle"]), ("\nb.txt:needle\n".into(), Some(0)));
}

#[test]
fn single_threaded_and_unmarked_modes_keep_the_notice() {
    let f = Fixture::new("control");
    assert_eq!(
        f.grep("1", &["-W", "needle"]),
        ("Binary file a.bin matches\nb.txt:needle\n".into(), Some(0))
    );
    // `-p` alone arms neither `show_hunk_mark` nor `skip_first_line`.
    assert_eq!(
        f.grep("2", &["-p", "needle"]),
        ("Binary file a.bin matches\nb.txt=one\nb.txt:needle\n".into(), Some(0))
    );
}
