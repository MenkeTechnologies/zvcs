//! `verify_filename()` refuses a path-position token that starts with `-`
//! before it looks at the file system:
//!
//! ```c
//! if (*arg == '-')
//!         die(_("option '%s' must come before non-option arguments"), arg);
//! ```
//!
//! (setup.c:287-288.) `cmd_rev_parse()` calls it after `show_file()` has echoed
//! the token, both for every token after the first path
//! (builtin/rev-parse.c:751-753) and for the first operand that failed to
//! resolve (:1185-1188) — which a `-` token only reaches once
//! `--end-of-options` has stopped it being read as an option. zvcs skipped the
//! check and stat'ed the token instead, so it answered `no such path` /
//! `ambiguous argument`, and exited 0 when a file of that name existed.
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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-dash-path-{tag}-{}", std::process::id()));
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

#[test]
fn an_option_after_a_path_is_refused_as_misplaced() {
    let f = Fixture::new("after");
    assert_eq!(
        f.run(&["rev-parse", "a", "-x"]),
        ("a\n-x\n".into(), "fatal: option '-x' must come before non-option arguments\n".into(), 128)
    );
    // An option rev-parse knows is no exception once a path has been seen.
    assert_eq!(
        f.run(&["rev-parse", "a", "--verify"]),
        (
            "a\n--verify\n".into(),
            "fatal: option '--verify' must come before non-option arguments\n".into(),
            128
        )
    );
}

#[test]
fn end_of_options_makes_a_dash_operand_a_misplaced_option() {
    let f = Fixture::new("eoo");
    assert_eq!(
        f.run(&["rev-parse", "--end-of-options", "--verify", "HEAD"]),
        (
            "--end-of-options\n--verify\n".into(),
            "fatal: option '--verify' must come before non-option arguments\n".into(),
            128
        )
    );
    // The check comes before the stat: a file named `-x` does not rescue it.
    f.write("-x", "");
    assert_eq!(
        f.run(&["rev-parse", "--end-of-options", "-x"]),
        (
            "--end-of-options\n-x\n".into(),
            "fatal: option '-x' must come before non-option arguments\n".into(),
            128
        )
    );
}
