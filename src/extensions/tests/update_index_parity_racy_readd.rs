//! `update-index <path>` on an entry whose stat cache matches but whose
//! timestamp is racy.
//!
//! ```c
//! /* Was the old index entry already up-to-date? */
//! if (old && !ce_stage(old) && !ie_match_stat(the_repository->index, old, st, 0))
//!         return 0;
//! ```
//!
//! (`add_one_path()`, builtin/update-index.c:288-290.) The short-circuit is only
//! as good as `ie_match_stat()`, which refuses to believe a stat match on an
//! entry whose recorded mtime is not older than the index's own:
//!
//! ```c
//! if (!changed && is_racy_timestamp(istate, ce)) {
//!         if (assume_racy_is_modified)
//!                 changed |= DATA_CHANGED;
//!         else
//!                 changed |= ce_modified_check_fs(istate, ce, st);
//! }
//! ```
//!
//! (read-cache.c:436-441.) Without that fallback a file rewritten inside the
//! same second as the last index write — the ordinary case in a script, and
//! exactly what `update-index --again` does — is taken for unchanged and the new
//! content is silently dropped.
//!
//! The fixture makes the race deterministic instead of waiting for one: the
//! file's mtime is stamped far into the future, so `index->timestamp.sec <=
//! sd_mtime.sec` holds for as long as the test runs, and `core.trustctime=false`
//! keeps the rewrite's new ctime from reporting a change on its own. Both
//! rewrites keep the byte count identical, so `sd_size` matches too and the racy
//! rule really is the only thing left that can notice.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Far enough ahead that no plausible test-run clock catches up with it.
const FUTURE: &str = "203001010000";

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uiracy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        // The rewrite below bumps ctime whatever else it preserves; git only
        // compares it when `core.trustctime` is on.
        f.git(&["config", "core.trustctime", "false"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Write `body` to `name` and stamp it with a fixed future mtime, so every
    /// stat field the index records stays put across rewrites.
    fn write_racy(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
        let out = Command::new("touch")
            .args(["-t", FUTURE, name])
            .current_dir(&self.work)
            .output()
            .unwrap();
        assert!(out.status.success(), "touch failed: {out:?}");
    }

    fn staged_oid(&self, name: &str) -> String {
        let line = self.stdout(&["ls-files", "--stage", name]);
        line.split_whitespace().nth(1).unwrap_or_else(|| panic!("no entry: {line}")).to_owned()
    }

    fn hash_of(&self, body: &str) -> String {
        let mut c = self.cmd(&["hash-object", "--stdin"]);
        c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

#[test]
fn a_racy_rewrite_is_re_read_rather_than_short_circuited() {
    let f = Fixture::new("readd");
    f.write_racy("f", "AAAA\n");
    f.git(&["update-index", "--add", "f"]);
    assert_eq!(f.staged_oid("f"), f.hash_of("AAAA\n"));

    // Same length, same mtime, same inode — only the bytes moved.
    f.write_racy("f", "BBBB\n");
    f.git(&["update-index", "f"]);

    assert_eq!(
        f.staged_oid("f"),
        f.hash_of("BBBB\n"),
        "the stat cache matched, so only ce_modified_check_fs() could have caught this"
    );
}

/// `--again` reaches `update_one()`/`add_one_path()` by the same route, and is
/// where the dropped rewrite actually bit: a script that edits and re-stages
/// inside one second got the stale blob.
#[test]
fn again_picks_up_a_racy_rewrite() {
    let f = Fixture::new("again");
    f.write_racy("f", "AAAA\n");
    f.git(&["add", "f"]);
    f.git(&["commit", "-q", "-m", "one"]);

    // Make the entry differ from HEAD so `--again` considers it at all.
    f.write_racy("f", "CCCC\n");
    f.git(&["update-index", "f"]);
    assert_eq!(f.staged_oid("f"), f.hash_of("CCCC\n"));

    f.write_racy("f", "DDDD\n");
    f.git(&["update-index", "--again"]);

    assert_eq!(f.staged_oid("f"), f.hash_of("DDDD\n"));
}

/// An assume-unchanged entry is the one case where the short-circuit is
/// unconditional: `ie_match_stat()` returns 0 on `CE_VALID` before it ever looks
/// at a timestamp (read-cache.c:405-406).
#[test]
fn assume_unchanged_still_short_circuits() {
    let f = Fixture::new("assume");
    f.write_racy("f", "AAAA\n");
    f.git(&["update-index", "--add", "f"]);
    f.git(&["update-index", "--assume-unchanged", "f"]);

    f.write_racy("f", "BBBB\n");
    f.git(&["update-index", "f"]);

    assert_eq!(f.staged_oid("f"), f.hash_of("AAAA\n"));
}
