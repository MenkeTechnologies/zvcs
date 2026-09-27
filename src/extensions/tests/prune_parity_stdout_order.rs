//! Where `git prune -n`'s stdout listing lands against its stderr diagnostics.
//!
//! `prune_object()` lists `<oid> <type>` with `printf()` into stdio's stdout
//! buffer, while `prune_cruft()` writes `bad sha1 file: <path>` with
//! `fprintf(stderr, …)` (builtin/prune.c:103, :117). Off a terminal the buffer
//! is flushed only by `exit()`, so captured together every stderr line comes
//! first, whatever order the loose scan met them in. zvcs wrote stdout
//! straight to the fd, interleaving the two in scan order.
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
    /// One commit, the dangling blob `dangle\n` (82cef6e…, in `82/`), and the
    /// cruft file `objects/ab/zz`, which the scan meets after the blob.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-prune-stdout-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.root.join("dangle"), "dangle\n").unwrap();
        let blob = f.git(&["hash-object", "-w", f.root.join("dangle").to_str().unwrap()]);
        assert_eq!(blob, "82cef6e227df8e6b387cba755b3ece33efb799bc\n");
        let ab = f.work.join(".git/objects/ab");
        std::fs::create_dir_all(&ab).unwrap();
        std::fs::write(ab.join("zz"), "x\n").unwrap();
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

    fn git(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    /// stdout and stderr into one file.
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
fn a_dry_run_lists_the_objects_after_the_cruft_diagnostic() {
    let f = Fixture::new("dry");
    let (out, code) = f.run_merged(&["prune", "-n"]);
    assert_eq!(
        (out.as_str(), code),
        (
            "bad sha1 file: .git/objects/ab/zz\n\
             82cef6e227df8e6b387cba755b3ece33efb799bc blob\n",
            0
        )
    );
}

#[test]
fn a_verbose_prune_lists_the_objects_after_the_cruft_diagnostic() {
    let f = Fixture::new("verbose");
    let (out, code) = f.run_merged(&["prune", "-v", "--expire=now"]);
    assert_eq!(
        (out.as_str(), code),
        (
            "bad sha1 file: .git/objects/ab/zz\n\
             82cef6e227df8e6b387cba755b3ece33efb799bc blob\n",
            0
        )
    );
    assert!(!f.work.join(".git/objects/82/cef6e227df8e6b387cba755b3ece33efb799bc").exists());
}
