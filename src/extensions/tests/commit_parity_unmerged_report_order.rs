//! Where `git commit`'s `U<TAB><path>` lines land against its refusal, captured.
//!
//! `prepare_index()` refreshes the index with `REFRESH_IN_PORCELAIN`, so
//! `refresh_index()` `printf`s `U\t<path>` for each conflicted path
//! (read-cache.c:1518) and `refresh_cache_or_die()` then calls
//! `die_resolve_conflict("commit")`, whose error and hint go to stderr. The
//! `U` lines wait in stdio's buffer until `die()`'s `exit()`, so off a
//! terminal they come after the stderr lines. `git merge --continue` runs
//! `cmd_commit()` in process (builtin/merge.c:1468), and `cherry-pick
//! --continue` a `git commit` child (sequencer.c:5232), so all three agree.
//! zvcs flushed the `U` lines first.
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
    /// `main` and `side` both rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-unmerged-report-order-{tag}-{}", std::process::id()));
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

const REFUSAL: &str = "\
error: Committing is not possible because you have unmerged files.
hint: Fix them up in the work tree, and then use 'git add/rm <file>'
hint: as appropriate to mark resolution and make a commit.
fatal: Exiting because of an unresolved conflict.
U\tfile
";

#[test]
fn the_unmerged_paths_follow_the_refusal() {
    let f = Fixture::new("merge");
    assert_eq!(f.merged(&["merge", "side"]).1, 1);
    for args in [&["commit"][..], &["merge", "--continue"]] {
        assert_eq!(f.merged(args), (REFUSAL.to_string(), 128), "{args:?}");
    }
}

#[test]
fn a_cherry_pick_continue_child_says_the_same() {
    let f = Fixture::new("pick");
    assert_eq!(f.merged(&["cherry-pick", "side"]).1, 1);
    assert_eq!(f.merged(&["cherry-pick", "--continue"]), (REFUSAL.to_string(), 128));
}
