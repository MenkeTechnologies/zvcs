//! When the `pre-push` hook runs and what it reads.
//!
//! `transport_push()` runs the hook after `match_push_refs()` and
//! `set_ref_status_for_push()` (transport.c:1466-1481), so its stdin is built
//! from `remote_refs`: `<peer> <new> <ref> <old>` with `old` the value the
//! remote *advertised*, in `remote_refs` order, and without the refs that will
//! not be pushed —
//!
//! ```c
//! case REF_STATUS_REJECT_NONFASTFORWARD:
//! case REF_STATUS_REJECT_REMOTE_UPDATED:
//! case REF_STATUS_REJECT_STALE:
//! case REF_STATUS_UPTODATE:
//!         return 0; /* skip refs which won't be pushed */
//! ```
//!
//! (`pre_push_hook_feed_stdin()`, transport.c:1340-1375.) zvcs ran the hook
//! before connecting, so it listed every request in command-line order, took
//! `old` from the local remote-tracking ref (null when there was none), and
//! included the up-to-date and non-fast-forward refs.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::os::unix::fs::PermissionsExt;
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
    /// `r.git` holds `keep`, `main`, `nf`, `old` at the first commit. Locally
    /// `main` is one commit ahead, `nf` was rewritten, and the remote-tracking
    /// `refs/remotes/r/main` is gone. The hook copies its arguments and stdin to
    /// `hook.out` and exits `$HOOKRC`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-pre-push-stdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"], "0");
        f.run(&["init", "-q", "-b", "main", "."], "0");
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"], "0");
        f.run(&["commit", "-q", "-m", "a"], "0");
        for b in ["old", "keep", "nf"] {
            f.run(&["branch", b], "0");
        }
        f.run(&["remote", "add", "r", "../r.git"], "0");
        f.run(&["push", "-q", "r", "main", "old", "keep", "nf"], "0");
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"], "0");
        f.run(&["checkout", "-q", "--orphan", "tmp"], "0");
        f.run(&["commit", "-q", "-m", "orphan"], "0");
        f.run(&["branch", "-f", "nf", "tmp"], "0");
        f.run(&["checkout", "-q", "main"], "0");
        f.run(&["branch", "-q", "-D", "tmp"], "0");
        f.run(&["update-ref", "-d", "refs/remotes/r/main"], "0");
        let hook = f.work.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\necho \"$*\" >hook.out\ncat >>hook.out\nexit $HOOKRC\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn run(&self, args: &[&str], hookrc: &str) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOOKRC", hookrc)
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

    fn oid(&self, rev: &str) -> String {
        self.run(&["rev-parse", rev], "0").0.trim().to_string()
    }

    fn hook_input(&self) -> String {
        std::fs::read_to_string(self.work.join("hook.out")).unwrap()
    }
}

#[test]
fn the_hook_reads_remote_refs_minus_what_will_not_be_pushed() {
    let f = Fixture::new("stdin");
    let (main, base) = (f.oid("main"), f.oid("keep"));
    let null = "0".repeat(40);
    let (_, _, code) = f.run(
        &["push", "r", "main:refs/heads/new", "nf", "keep", "main:refs/heads/other", "main", ":old"],
        "0",
    );
    assert_eq!(code, 1, "nf is rejected");
    assert_eq!(
        f.hook_input(),
        format!(
            "r ../r.git\n\
             refs/heads/main {main} refs/heads/main {base}\n\
             (delete) {null} refs/heads/old {base}\n\
             refs/heads/main {main} refs/heads/new {null}\n\
             refs/heads/main {main} refs/heads/other {null}\n"
        )
    );
}

#[test]
fn a_refusing_hook_stops_the_push_after_the_match() {
    let f = Fixture::new("refuse");
    let (base, main) = (f.oid("keep"), f.oid("main"));
    assert_eq!(
        f.run(&["push", "--porcelain", "r", "main", "keep"], "1"),
        (String::new(), "error: failed to push some refs to '../r.git'\n".to_string(), 1)
    );
    assert_eq!(f.hook_input(), format!("r ../r.git\nrefs/heads/main {main} refs/heads/main {base}\n"));
    assert_eq!(f.run(&["--git-dir=../r.git", "rev-parse", "main"], "0").0.trim(), base);

    // A push with nothing to send still runs the hook, with nothing on stdin.
    f.run(&["push", "-q", "r", "main"], "0");
    let (_, err, code) = f.run(&["push", "--dry-run", "r", "main", "keep"], "0");
    assert_eq!((err.as_str(), code), ("Everything up-to-date\n", 0));
    assert_eq!(f.hook_input(), "r ../r.git\n");
}
