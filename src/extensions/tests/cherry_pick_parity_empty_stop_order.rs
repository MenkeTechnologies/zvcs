//! Where the empty-pick advice lands against the status report, captured.
//!
//! A pick that merges to `HEAD`'s tree is handed to a `git commit` child
//! (`run_git_commit()`, sequencer.c:1122; `continue_single_pick()`,
//! sequencer.c:5232). That child refuses with `run_status(stdout, ...)` and
//! then `fputs(_(empty_cherry_pick_advice), stderr)` (builtin/commit.c:
//! 1081-1097). Its stdout is a fully buffered stdio stream once it is not a
//! terminal, so the report reaches the fd at the child's `exit()` — after the
//! advice. A bare `git commit` in the same state is the same single process.
//! zvcs wrote the report first in all three.
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
    /// `one` lands on `main` through a merge of `s1`, so picking `s1` again
    /// merges to `HEAD`'s tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-cp-empty-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "s1"]);
        std::fs::write(f.work.join("one"), "1\n").unwrap();
        f.run(&["add", "one"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["merge", "-q", "--no-ff", "-m", "merge s1", "s1"]);
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

const ADVICE: &str = "\
The previous cherry-pick is now empty, possibly due to conflict resolution.
If you wish to commit it anyway, use:

    git commit --allow-empty

Otherwise, please use 'git cherry-pick --skip'
";

fn report(short: &str) -> String {
    format!(
        "On branch main
You are currently cherry-picking commit {short}.
  (all conflicts fixed: run \"git cherry-pick --continue\")
  (use \"git cherry-pick --skip\" to skip this patch)
  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)

nothing to commit, working tree clean
"
    )
}

#[test]
fn the_advice_precedes_the_report_for_the_pick_continue_and_commit() {
    let f = Fixture::new("all");
    let short = String::from_utf8(
        f.command(&["rev-parse", "--short", "s1"]).output().unwrap().stdout,
    )
    .unwrap();
    let want = format!("{ADVICE}{}", report(short.trim()));
    for args in [&["cherry-pick", "s1"][..], &["cherry-pick", "--continue"], &["commit"]] {
        assert_eq!(f.merged(args), (want.clone(), 1), "{args:?}");
    }
    assert!(f.work.join(".git/CHERRY_PICK_HEAD").exists());
}

#[test]
fn a_plain_status_is_unaffected() {
    let f = Fixture::new("status");
    assert_eq!(
        f.merged(&["status"]),
        ("On branch main\nnothing to commit, working tree clean\n".to_string(), 0)
    );
}
