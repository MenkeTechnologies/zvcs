//! Where `git-merge-octopus`'s own lines land against `cmd_merge`'s, captured.
//!
//! `try_merge_command()` (merge.c:22-42, from builtin/merge.c:847) runs the
//! octopus as a `run_command()` child: a shell script whose `echo`s reach the
//! fd as they happen. So a head that will not merge prints `Automated merge
//! did not work.` and `Should not be doing an octopus.` before the parent's
//! `fprintf(stderr, _("Merge with strategy %s failed.\n"))`
//! (builtin/merge.c:1844). zvcs ran the octopus inside `merge`'s armed stdout
//! buffer, which moved those two lines behind the stderr one.
//!
//! Both streams go to ONE file, the only way the interleaving is observable.
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
    /// `main`, `side` and `other` each rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-octopus-output-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("file"), "side\n").unwrap();
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "-b", "other", "main"]);
        std::fs::write(f.work.join("file"), "other\n").unwrap();
        f.run(&["commit", "-q", "-am", "other"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "main\n").unwrap();
        f.run(&["commit", "-q", "-am", "main"]);
        f
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn run(&self, args: &[&str]) {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Both streams into one file, plus the exit code.
    fn merged(&self, args: &[&str]) -> (String, i32) {
        let sink = self.root.join("sink");
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
fn the_script_lines_precede_the_strategy_failure() {
    let f = Fixture::new("octopus");
    let want = "\
Trying simple merge with side
Simple merge did not work, trying automatic merge.
Auto-merging file
ERROR: content conflict in file
fatal: merge program failed
Automated merge did not work.
Should not be doing an octopus.
Merge with strategy octopus failed.
";
    assert_eq!(f.merged(&["merge", "-s", "octopus", "side", "other"]), (want.to_string(), 2));
    assert!(!f.work.join(".git/MERGE_HEAD").exists());
}
