//! A push that matched nothing against a remote that has nothing.
//!
//! `send_pack()` returns at once when `remote_refs` is empty — the advertisement
//! plus every ref `match_push_refs()` created — printing "No refs in common and
//! none specified; doing nothing." (send-pack.c:542-547). It writes no command
//! list and no flush, so the receive-pack on the other end dies "the remote end
//! hung up unexpectedly", `finish_connect()` fails (transport.c:957,
//! builtin/send-pack.c:330), and the push fails. zvcs reported "Everything
//! up-to-date" at exit 0.
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
    /// `work` on `main` with one commit and an annotated tag; `r.git` an empty bare repository.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-no-refs-in-common-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run(&["tag", "-a", "-m", "t", "v1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const NOTHING: &str = "No refs in common and none specified; doing nothing.
Perhaps you should specify a branch.
fatal: the remote end hung up unexpectedly
";

#[test]
fn matching_and_tagless_pushes_into_an_empty_remote_fail() {
    let f = Fixture::new("push");
    let want = format!("{NOTHING}error: failed to push some refs to '../r.git'\n");
    for args in [
        &["push", "../r.git", ":"][..],
        &["push", "-n", "../r.git", ":"][..],
        &["push", "--porcelain", "../r.git", ":"][..],
        &["push", "--prune", "../r.git", ":"][..],
        &["push", "--follow-tags", "../r.git", ":"][..],
        &["-c", "push.default=matching", "push", "../r.git"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 1), "{args:?}");
    }
    let (refs, _, _) = f.run(&["ls-remote", "../r.git"]);
    assert_eq!(refs, "");
    // Once there is a tag to push, `--tags` has a ref and goes through.
    let (_, err, code) = f.run(&["push", "--tags", "../r.git"]);
    assert_eq!((err.as_str(), code), ("To ../r.git\n * [new tag]         v1 -> v1\n", 0));
}

#[test]
fn send_pack_exits_with_the_receive_pack_status() {
    let f = Fixture::new("send-pack");
    let (out, err, code) = f.run(&["send-pack", "../r.git"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", NOTHING, 128));
}
