//! Protocol v2 `fetch`: which `have`s are acknowledged.
//!
//! `parse_have()` hands each `have` to `got_oid()`, and `do_got_oid()`
//! (upload-pack.c:522-549) first marks the commit's parents `THEY_HAVE`, then
//! adds the have to `have_obj` — the list `send_acks()` ACKs
//! (upload-pack.c:1686-1699) — only if it was not already marked. A run of
//! haves walking down one ancestry is therefore acknowledged once, at its
//! tip, while the same commits sent oldest first are each acknowledged. zvcs
//! acknowledged every have it had, so `fetch --negotiate-only` against it
//! printed the whole history instead of the common tip.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// Three commits on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-upload-pack-they-have-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main", "."], None);
        for m in ["a", "b", "c"] {
            f.git(&["commit", "-q", "--allow-empty", "-m", m], None);
        }
        f
    }

    fn git(&self, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_PROTOCOL", "version=2")
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
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or_default()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        out.stdout
    }

    /// The commits of `main`, newest first.
    fn history(&self) -> Vec<String> {
        String::from_utf8(self.git(&["rev-list", "main"], None))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// One stateless `command=fetch` request carrying `haves` under
    /// `wait-for-done`, and the server's answer.
    fn acknowledgments(&self, haves: &[String]) -> String {
        fn pkt(out: &mut Vec<u8>, line: &str) {
            out.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
        }
        let mut req = Vec::new();
        pkt(&mut req, "command=fetch\n");
        pkt(&mut req, "object-format=sha1\n");
        req.extend_from_slice(b"0001");
        pkt(&mut req, "wait-for-done\n");
        for have in haves {
            pkt(&mut req, &format!("have {have}\n"));
        }
        req.extend_from_slice(b"0000");
        String::from_utf8(self.git(&["upload-pack", "--stateless-rpc", "."], Some(&req))).unwrap()
    }
}

#[test]
fn a_have_whose_child_came_first_is_not_acknowledged() {
    let f = Fixture::new("newest-first");
    let history = f.history();
    assert_eq!(
        f.acknowledgments(&history),
        format!("0014acknowledgments\n0031ACK {}\n0000", history[0])
    );
}

#[test]
fn haves_sent_oldest_first_are_each_acknowledged() {
    let f = Fixture::new("oldest-first");
    let mut history = f.history();
    history.reverse();
    let acks: String = history.iter().map(|id| format!("0031ACK {id}\n")).collect();
    assert_eq!(f.acknowledgments(&history), format!("0014acknowledgments\n{acks}0000"));
}
