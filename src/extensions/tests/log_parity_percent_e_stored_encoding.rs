//! `%e` printed nothing for a commit recorded in another encoding.
//!
//! `c->commit_encoding` is the `encoding` header of the commit as stored:
//! `repo_logmsg_reencode()` hands it back before it re-codes (pretty.c:724-726),
//! and a user format is always expanded against the commit re-coded to UTF-8
//! (pretty.c:1734), a buffer whose header that re-coding dropped. zvcs read the
//! header from the re-coded buffer.
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
        let root = std::env::temp_dir().join(format!("zvcs-log-percent-e-{tag}-{}", std::process::id()));
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
fn percent_e_names_the_stored_encoding() {
    let f = Fixture::new("e");
    assert_eq!(
        f.run(&["log", "--format=%e|%s|%an"]),
        (b"ISO-8859-1|latin1 caf\xc3\xa9|J\xc3\xb6rg\n|base|A U Thor\n".to_vec(), String::new(), 0)
    );
    assert_eq!(f.run(&["log", "--format=%e", "--encoding=none", "-1"]), (b"ISO-8859-1\n".to_vec(), String::new(), 0));
}
