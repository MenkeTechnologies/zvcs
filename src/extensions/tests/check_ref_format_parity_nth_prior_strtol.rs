//! `git check-ref-format --branch @{-N}` against stock git for the way
//! `interpret_nth_prior_checkout()` reads `N`: with `strtol()`
//! (object-name.c:1289-1293), which skips leading whitespace and one sign and
//! saturates on overflow. `@{- 1}` is therefore the previous branch, while
//! `@{-1 }`, `@{--1}` and `@{-0x1}` are not `@{-N}` at all and are judged as
//! branch names.

#[path = "support/stock_git.rs"]
mod stock_git;

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
    fn command(&self, bin: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(self.root.join("repo"))
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_DIR");
        cmd
    }

    /// `main`, then `other`, then back to `main`: `@{-1}` is `other`.
    fn new(stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-crf-strtol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let f = Fixture { root };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"],
            &["checkout", "-q", "-b", "other"],
            &["checkout", "-q", "main"],
        ] {
            assert!(f.command(stock, args).status().unwrap().success(), "{args:?}");
        }
        f
    }
}

#[test]
fn nth_prior_checkout_reads_n_with_strtol() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock);
    for arg in ["@{- 1}", "@{-+1}", "@{-\t1}x", "@{-1 }", "@{--1}", "@{-0x1}", "@{-99999999999999999999}", "@{-}"] {
        let run = |bin: &str| {
            let out = f.command(bin, &["check-ref-format", "--branch", arg]).output().unwrap();
            (out.stdout, out.stderr, out.status.code())
        };
        let want = run(stock);
        assert_eq!(run(BIN), want, "--branch {arg:?}");
    }
    let out = f.command(BIN, &["check-ref-format", "--branch", "@{- 1}"]).output().unwrap();
    assert_eq!(out.stdout, b"other\n");
}
