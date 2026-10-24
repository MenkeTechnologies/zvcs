//! `reflog write` in a reftable repository: `cmd_reflog_write`
//! (builtin/reflog.c:409-454) queues one `ref_transaction_update_reflog()`
//! update, which the reftable backend writes as a log record carrying the two
//! ids exactly as given (refs/reftable-backend.c:1090-1100) — the reference is
//! neither created nor moved, and a ref with no log gains one.
//!
//! Expectations measured against stock git 2.56.0.
#![cfg(unix)]

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
    /// Two commits on `main` in a reftable repository.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reflogw-reftable-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "--ref-format=reftable", "-b", "main", "."]);
        for n in ["1", "2"] {
            std::fs::write(f.root.join("f.txt"), n).unwrap();
            f.git(&["add", "f.txt"]);
            f.git(&["commit", "-q", "-m", &format!("c{n}")]);
        }
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn reflog_write_logs_the_given_ids_without_touching_refs() {
    let f = Fixture::new("ids");
    let c1 = f.stdout(&["rev-parse", "HEAD~1"]).trim().to_owned();
    let c2 = f.stdout(&["rev-parse", "HEAD"]).trim().to_owned();

    f.git(&["reflog", "write", "refs/heads/main", &c2, &c1, "  by   hand "]);
    f.git(&["reflog", "write", "refs/heads/unlogged", &c1, &c2, "new log"]);

    // Neither reference moved, and `unlogged` was not created.
    assert_eq!(f.stdout(&["rev-parse", "refs/heads/main"]).trim(), c2);
    assert_eq!(f.stdout(&["for-each-ref", "--format=%(refname)"]), "refs/heads/main\n");
    assert_eq!(f.stdout(&["reflog", "list"]), "HEAD\nrefs/heads/main\nrefs/heads/unlogged\n");

    // The entries, with the message normalized as `copy_reflog_msg()` does,
    // read back with both ids exactly as they were given.
    f.git(&["refs", "migrate", "--ref-format=files"]);
    let main = std::fs::read_to_string(f.root.join(".git/logs/refs/heads/main")).unwrap();
    assert_eq!(
        main.lines().last().unwrap(),
        format!("{c2} {c1} t <t@e.co> 1700000000 +0000\tby hand")
    );
    let unlogged = std::fs::read_to_string(f.root.join(".git/logs/refs/heads/unlogged")).unwrap();
    assert_eq!(unlogged, format!("{c1} {c2} t <t@e.co> 1700000000 +0000\tnew log\n"));
}
