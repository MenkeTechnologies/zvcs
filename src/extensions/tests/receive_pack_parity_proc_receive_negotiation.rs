//! The version negotiation with the `proc-receive` hook.
//!
//! `run_proc_receive_hook()` (builtin/receive-pack.c:1196-1240) writes
//! `version=1` and a flush, then reads until a flush with
//! `PACKET_READ_GENTLE_ON_EOF`:
//!
//! ```c
//! status = packet_reader_read(&reader);
//! if (status != PACKET_READ_NORMAL) {
//!         /* Check whether proc-receive exited abnormally */
//!         if (status == PACKET_READ_EOF)
//!                 code = -1;
//!         break;
//! }
//! …
//! if (code) {
//!         strbuf_addstr(&errmsg, "fail to negotiate version with proc-receive hook");
//! ```
//!
//! A hook that exits without answering is therefore a negotiation failure.
//! zvcs took the missing answer for a version-0 hook, went on to write the
//! commands, and reported `fail to write commands to proc-receive hook`. A hook
//! that answers with a bare flush is the real version-0 hook and is accepted.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `r.git` diverting `refs/for` to `proc-receive`, and `w` with one commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-proc-receive-negotiation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "--bare", "-b", "main", "r.git"]);
        f.run(&["-C", "r.git", "config", "receive.procReceiveRefs", "refs/for"]);
        f.run(&["init", "-q", "-b", "main", "w"]);
        std::fs::write(f.root.join("w/a"), "a\n").unwrap();
        f.run(&["-C", "w", "add", "a"]);
        f.run(&["-C", "w", "commit", "-q", "-m", "a"]);
        f
    }

    fn hook(&self, body: &str) {
        let path = self.root.join("r.git/hooks/proc-receive");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
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

    fn push(&self) -> (String, String, i32) {
        self.run(&["-C", "w", "push", "../r.git", "HEAD:refs/for/main"])
    }
}

#[test]
fn a_hook_that_exits_without_answering_fails_the_negotiation() {
    for body in ["exit 1", "exit 0"] {
        let f = Fixture::new("exit");
        f.hook(body);
        assert_eq!(
            f.push(),
            (
                String::new(),
                "remote: error: fail to negotiate version with proc-receive hook        \n\
                 To ../r.git\n \
                 ! [remote rejected] HEAD -> refs/for/main (fail to run proc-receive hook)\n\
                 error: failed to push some refs to '../r.git'\n"
                    .to_string(),
                1
            ),
            "{body}"
        );
    }
}

#[test]
fn a_bare_flush_is_a_version_zero_hook() {
    let f = Fixture::new("v0");
    // Swallow `version=1` + flush (18 bytes), answer with a flush, swallow the
    // one command (`<40-hex> <40-hex> refs/for/main` + flush, 103 bytes) and
    // report it.
    f.hook(
        "head -c 18 >/dev/null; printf 0000; head -c 103 >/dev/null; \
         printf '0015ok refs/for/main\\n0000'",
    );
    assert_eq!(
        f.push(),
        (String::new(), "To ../r.git\n * [new reference]   HEAD -> refs/for/main\n".to_string(), 0)
    );
}
