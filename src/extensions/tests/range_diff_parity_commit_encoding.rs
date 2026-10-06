//! `range-diff` printed the stored bytes of a commit recorded in another
//! encoding.
//!
//! The patches are `git log --pretty=medium` output (range-diff.c:54-70) and the
//! header line is `pp_commit_easy()` (range-diff.c:463); both render the commit
//! re-coded to the log output encoding (pretty.c:2315-2316). zvcs read the
//! message as stored.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

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
    /// `base`, then a commit recorded in ISO-8859-1 — subject `latin1 caf\xe9`,
    /// author `J\xf6rg` — on top of it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-range-diff-encoding-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("f"), "a\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.root.join("f"), "b\n").unwrap();
        std::fs::write(f.root.join("msg"), b"latin1 caf\xe9\n\nbody\n").unwrap();
        let out = f
            .cmd(&["-c", "i18n.commitEncoding=ISO-8859-1", "commit", "-q", "-a", "-F", "msg"])
            .env("GIT_AUTHOR_NAME", std::ffi::OsStr::from_bytes(b"J\xf6rg"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.root)
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
            .env("TZ", "UTC");
        c
    }

    /// Raw stdout bytes, stderr and exit code.
    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn the_subject_is_recoded_to_the_log_output_encoding() {
    let f = Fixture::new("recode");
    let (out, err, code) = f.run(&["range-diff", "HEAD~1..HEAD", "HEAD~1..HEAD"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(out.ends_with(b" latin1 caf\xc3\xa9\n"), "{}", String::from_utf8_lossy(&out));
    let (out, _, _) = f.run(&["-c", "i18n.logOutputEncoding=ISO-8859-1", "range-diff", "HEAD~1..HEAD", "HEAD~1..HEAD"]);
    assert!(out.ends_with(b" latin1 caf\xe9\n"), "{}", String::from_utf8_lossy(&out));
}
