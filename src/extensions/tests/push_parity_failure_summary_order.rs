//! Where `push` prints its failure summary.
//!
//! `push_with_options()` prints `error: failed to push some refs to …` and the
//! advice for the rejections only once `transport_push()` has returned
//! (builtin/push.c:392-409) — after the status block, `set_upstreams()` and
//! the tracking-ref updates (transport.c:1545-1557). zvcs printed both from the
//! status printer, so under `-v` they came before the `updating local tracking
//! ref` lines.
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
    /// `r.git` holds `keep`, `main` and `nf` at the first commit. Locally `main`
    /// is one commit ahead, `nf` was rewritten (a non-fast-forward), and `zz`
    /// and `aa` are new.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-failure-summary-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "keep"]);
        f.run(&["branch", "nf"]);
        f.run(&["remote", "add", "r", "../r.git"]);
        f.run(&["push", "-q", "r", "main", "keep", "nf"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        f.run(&["checkout", "-q", "--orphan", "tmp"]);
        f.run(&["commit", "-q", "-m", "orphan"]);
        f.run(&["branch", "-f", "nf", "tmp"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["branch", "-q", "-D", "tmp"]);
        f.run(&["branch", "zz"]);
        f.run(&["branch", "aa"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
}

#[test]
fn the_summary_and_advice_follow_the_tracking_refs() {
    let f = Fixture::new("verbose");
    let main = f.run(&["rev-parse", "--short", "main"]).0;
    let base = f.run(&["rev-parse", "--short", "keep"]).0;
    let (out, err, code) = f.run(&["push", "-v", "r", "nf", "zz", "main", "keep"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "Pushing to ../r.git\n\
                 To ../r.git\n \
                 = [up to date]      keep -> keep\n   \
                 {}..{}  main -> main\n \
                 * [new branch]      zz -> zz\n \
                 ! [rejected]        nf -> nf (non-fast-forward)\n\
                 updating local tracking ref 'refs/remotes/r/keep'\n\
                 updating local tracking ref 'refs/remotes/r/main'\n\
                 updating local tracking ref 'refs/remotes/r/zz'\n\
                 error: failed to push some refs to '../r.git'\n\
                 hint: Updates were rejected because a pushed branch tip is behind its remote\n\
                 hint: counterpart. If you want to integrate the remote changes, use 'git pull'\n\
                 hint: before pushing again.\n\
                 hint: See the 'Note about fast-forwards' in 'git push --help' for details.\n",
                base.trim(),
                main.trim()
            ),
            1
        )
    );
}
