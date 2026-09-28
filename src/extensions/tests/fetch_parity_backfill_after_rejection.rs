//! Automatic tag following's second pass after the first one failed.
//!
//! `do_fetch()` gives up when `fetch_and_consume_refs()` fails — a refused
//! update is enough — and never reaches `backfill_tags()`: a tag the second
//! pass would have recovered is neither written, listed nor recorded in
//! `FETCH_HEAD`. When the second pass does run it calls `store_updated_refs()`
//! again, so the `fetch.showForcedUpdates=false` note
//! (builtin/fetch.c:1351-1358) is printed twice. zvcs wrote and listed the
//! backfilled tag after a rejection, and printed the note once.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
    /// `up` has `a-b` on `main`; `work` clones it and copies `origin/main` to
    /// branch `copy`; then `up` rewinds `main` to `a`, commits `c` (tagged `t`)
    /// and `d` on it — `t` is only reachable through the pack, so it is a
    /// second-pass tag.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-backfill-after-reject-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["branch", "copy", "origin/main"]);
        f.run_in(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "c"]);
        f.run_in(&up, &["tag", "t"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "d"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }
}

const NOTE: &str = "warning: fetch normally indicates which branches had a forced update,\n\
but that check has been disabled; to re-enable, use '--show-forced-updates'\n\
flag or run 'git config fetch.showForcedUpdates true'\n";

#[test]
fn a_rejection_keeps_the_second_pass_tag_out() {
    let f = Fixture::new("rejected");
    let (_, err, code) = f.run(&["fetch", "origin", "main:copy"]);
    assert_eq!(code, 1);
    assert!(err.contains(" ! [rejected] main       -> copy  (non-fast-forward)\n"), "{err}");
    assert!(!err.contains("[new tag]"), "{err}");
    assert_eq!(f.run(&["tag"]).0, "");
    let fetch_head = std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap();
    assert!(!fetch_head.contains("tag 't'"), "{fetch_head}");
    // Without the rejection the second pass runs and the tag arrives.
    let (_, err, code) = f.run(&["fetch"]);
    assert_eq!(code, 0);
    assert!(err.ends_with(" * [new tag]         t          -> t\n"), "{err}");
    assert_eq!(f.run(&["tag"]).0, "t\n");
}

#[test]
fn the_forced_update_note_follows_each_pass() {
    let f = Fixture::new("note");
    let (_, err, code) = f.run(&["-c", "fetch.showForcedUpdates=false", "fetch"]);
    assert_eq!(code, 0);
    assert!(err.starts_with(&format!("{NOTE}{NOTE}From ")), "{err}");
    let f = Fixture::new("note-rejected");
    let (_, err, code) =
        f.run(&["-c", "fetch.showForcedUpdates=false", "fetch", "--dry-run", "origin", "main:copy"]);
    assert_eq!(code, 0);
    assert!(err.starts_with(&format!("{NOTE}{NOTE}From ")), "{err}");
}
