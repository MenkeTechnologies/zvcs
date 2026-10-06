//! `shortlog` printed and grouped by the bytes a commit recorded in another
//! encoding stores.
//!
//! `shortlog_add_commit()` renders every record through
//! `repo_format_commit_message()` with `ctx.output_encoding =
//! get_log_output_encoding()` (builtin/shortlog.c:248-258): the commit is
//! re-coded to UTF-8, expanded, and the result converted to the log output
//! encoding (pretty.c:1734, 2026-2046). zvcs took the subject and the author name
//! from the stored bytes.
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
        let root = std::env::temp_dir().join(format!("zvcs-shortlog-encoding-{tag}-{}", std::process::id()));
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
fn records_and_keys_are_recoded_to_the_log_output_encoding() {
    let f = Fixture::new("recode");
    assert_eq!(
        f.run(&["shortlog", "HEAD"]),
        (b"A U Thor (1):\n      base\n\nJ\xc3\xb6rg (1):\n      latin1 caf\xc3\xa9\n\n".to_vec(), String::new(), 0)
    );
    assert_eq!(f.run(&["shortlog", "-s", "HEAD"]), (b"     1\tA U Thor\n     1\tJ\xc3\xb6rg\n".to_vec(), String::new(), 0));
    assert_eq!(
        f.run(&["-c", "i18n.logOutputEncoding=ISO-8859-1", "shortlog", "HEAD"]),
        (b"A U Thor (1):\n      base\n\nJ\xf6rg (1):\n      latin1 caf\xe9\n\n".to_vec(), String::new(), 0)
    );
}
