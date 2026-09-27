//! Where `git bundle verify`'s stdout report lands against its `is okay` line.
//!
//! `verify_bundle()` prints `The bundle contains …` and the rest of its verbose
//! block with `printf_ln()`, into stdio's stdout buffer (bundle.c:265-286);
//! `cmd_bundle_verify()` then writes `<file> is okay` with
//! `fprintf(stderr, …)` (builtin/bundle.c:161). Off a terminal the buffer is
//! only flushed by `exit()`, so both streams captured into one file read
//! `is okay` first. zvcs wrote the report straight to fd 1 and so put it first.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::fs::File;
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
    /// Two commits on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-bundle-verify-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for i in 1..=2 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        f
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
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
            .env("TZ", "UTC");
        cmd
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    /// stdout and stderr into one file, the way `2>&1 >file` captures them.
    fn run_merged(&self, args: &[&str]) -> (String, i32) {
        let sink = self.root.join("merged.out");
        let file = File::create(&sink).unwrap();
        let status = self
            .command(args)
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .status()
            .unwrap();
        (std::fs::read_to_string(&sink).unwrap(), status.code().expect("no signal"))
    }
}

#[test]
fn a_complete_bundle_says_is_okay_before_its_report() {
    let f = Fixture::new("complete");
    f.run(&["bundle", "create", "../all.bundle", "main"]);
    let tip = f.run(&["rev-parse", "main"]);
    let tip = tip.trim_end();
    let (out, code) = f.run_merged(&["bundle", "verify", "../all.bundle"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "../all.bundle is okay\n\
             The bundle contains this ref:\n\
             {tip} refs/heads/main\n\
             The bundle records a complete history.\n\
             The bundle uses this hash algorithm: sha1\n"
        )
    );
}

#[test]
fn a_bundle_with_prerequisites_says_is_okay_before_its_report() {
    let f = Fixture::new("prereq");
    f.run(&["bundle", "create", "../inc.bundle", "main~1..main"]);
    let tip = f.run(&["rev-parse", "main"]);
    let base = f.run(&["rev-parse", "main~1"]);
    let (out, code) = f.run_merged(&["bundle", "verify", "../inc.bundle"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "../inc.bundle is okay\n\
             The bundle contains this ref:\n\
             {} refs/heads/main\n\
             The bundle requires this ref:\n\
             {} \n\
             The bundle uses this hash algorithm: sha1\n",
            tip.trim_end(),
            base.trim_end()
        )
    );
}
