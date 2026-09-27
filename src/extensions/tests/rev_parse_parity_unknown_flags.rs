//! Three spellings `cmd_rev_parse()` has no arm for, and so echoes through
//! `show_flag()` (builtin/rev-parse.c:1153-1155) like any unknown option:
//! `--help` past the first argument, `--all-objects`, and `-h` past the first
//! argument. `-h` is usage only as `argv[1]`
//! (`if (argc > 1 && !strcmp("-h", argv[1])) usage(…)`, :731-732), which runs
//! before any repository is opened. zvcs refused `--help` and `--all-objects`
//! as "not ported yet", answered a later `-h` with the usage block, and opened
//! the repository before answering a leading one.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-unknown-flags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "a\n");
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args, None)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or_default()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const USAGE: &str = "usage: git rev-parse --parseopt [<options>] -- [<args>...]\n   \
or: git rev-parse --sq-quote [<arg>...]\n   \
or: git rev-parse [<options>] [<arg>...]\n\n\
Run \"git rev-parse --parseopt -h\" for more information on the first usage.\n";

#[test]
fn unknown_spellings_are_echoed_as_flags() {
    let f = Fixture::new("echo");
    let head = f.run(&["rev-parse", "HEAD"]).0;
    assert_eq!(f.run(&["rev-parse", "HEAD", "--help"]), (format!("{head}--help\n"), String::new(), 0));
    assert_eq!(f.run(&["rev-parse", "HEAD", "-h"]), (format!("{head}-h\n"), String::new(), 0));
    assert_eq!(f.run(&["rev-parse", "--all-objects"]), ("--all-objects\n".into(), String::new(), 0));
    // `--verify` takes the flags off the output, and with no revision it fails.
    assert_eq!(
        f.run(&["rev-parse", "--verify", "--all-objects"]),
        (String::new(), "fatal: Needed a single revision\n".into(), 128)
    );
}

#[test]
fn a_leading_h_is_usage_even_outside_a_repository() {
    let f = Fixture::new("usage");
    assert_eq!(f.run(&["rev-parse", "-h", "HEAD"]), (String::new(), USAGE.into(), 129));
    let outside = f.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    assert_eq!(f.run_in(&outside, &["rev-parse", "-h", "HEAD"], None), (String::new(), USAGE.into(), 129));
}
