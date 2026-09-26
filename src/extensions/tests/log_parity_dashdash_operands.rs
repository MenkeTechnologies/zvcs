//! An operand in front of `--` that names a file is still a revision.
//!
//! `setup_revisions()` scans the whole argv for `--` before it resolves
//! anything, and when an operand fails `handle_revision_arg()` it checks
//!
//! ```c
//! if (seen_dashdash || *arg == '^')
//!         die("bad revision '%s'", arg);
//! ```
//!
//! (revision.c:3081-3082) ahead of its filename fallback. So `git log file
//! HEAD --` is `fatal: bad revision 'file'` even though `file` exists, and so is
//! `git log -g file HEAD --`. zvcs took `file` as the start of the pathspec and
//! then died on `HEAD: no such path in the working tree`, or, with `file` last,
//! walked HEAD limited to it.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// One commit on `main` that adds `file`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-dashdash-revs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "x\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

const BAD: (&str, &str, i32) = ("", "fatal: bad revision 'file'\n", 128);

#[test]
fn a_file_operand_before_dashdash_is_a_bad_revision() {
    let f = Fixture::new("before");
    for args in [
        &["log", "--format=%s", "file", "HEAD", "--"][..],
        &["log", "--format=%s", "HEAD", "file", "--"][..],
        &["log", "--format=%s", "file", "--", "file"][..],
        &["log", "-g", "--format=%gs", "file", "HEAD", "--"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), BAD, "{args:?}");
    }
}

#[test]
fn without_dashdash_the_file_operand_is_a_pathspec() {
    let f = Fixture::new("without");
    assert_eq!(f.stdout(&["log", "--format=%s", "file"]), "one\n");
    assert_eq!(f.stdout(&["log", "--format=%s", "HEAD", "file"]), "one\n");
}
