//! A deletion of a ref the remote does not have fails the whole push.
//!
//! `match_explicit()` refuses `:nosuch` when the remote does not advertise
//! `nosuch` and the name is not `refs/`-qualified (remote.c:1315-1322,
//! `error(_("unable to delete '%s': remote ref does not exist"))`), which makes
//! `match_push_refs()` fail, and `transport_push()` gives up right there:
//!
//! ```c
//! if (match_push_refs(local_refs, &remote_refs, rs, match_flags))
//!         goto done;
//! ```
//!
//! (transport.c:1466-1467) — before the `pre-push` hook, before anything is
//! sent, and with no status block (so `--porcelain` prints nothing on stdout).
//! zvcs reported the refusal and pushed every other refspec, `:old` included.
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
    /// `r.git` holds `keep`, `main`, `old` at the first commit; `main` is one
    /// commit ahead locally, and a `pre-push` hook leaves a marker.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-unmatched-delete-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "old"]);
        f.run(&["branch", "keep"]);
        f.run(&["remote", "add", "r", "../r.git"]);
        f.run(&["push", "-q", "r", "main", "old", "keep"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        let hook = f.work.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
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

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../r.git", "for-each-ref", "--format=%(refname) %(objectname)"]).0
    }
}

#[test]
fn nothing_is_pushed_and_the_hook_never_runs() {
    let f = Fixture::new("abort");
    let before = f.remote_refs();
    let refused = "error: unable to delete 'nosuch': remote ref does not exist\n\
                   error: failed to push some refs to '../r.git'\n";
    for args in [
        &["push", "r", "main", ":old", ":nosuch"][..],
        &["push", "--porcelain", "r", "main", ":old", ":nosuch"],
        &["push", "-d", "r", "nosuch", "old"],
    ] {
        assert_eq!(f.run(args), (String::new(), refused.to_string(), 1), "{args:?}");
        assert_eq!(f.remote_refs(), before, "{args:?}");
        assert!(!f.work.join("hook-ran").exists(), "{args:?}");
    }
    // Every refusal is reported, in refspec order.
    assert_eq!(
        f.run(&["push", "-d", "r", "nosuch1", "nosuch2"]),
        (
            String::new(),
            "error: unable to delete 'nosuch1': remote ref does not exist\n\
             error: unable to delete 'nosuch2': remote ref does not exist\n\
             error: failed to push some refs to '../r.git'\n"
                .to_string(),
            1
        )
    );
}
