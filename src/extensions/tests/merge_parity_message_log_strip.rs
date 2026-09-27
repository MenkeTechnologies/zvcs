//! MERGE_MSG when an explicit message meets `--log`.
//!
//! `collect_parents()` builds an `autogen` name list when `!have_message ||
//! shortlog_len` (builtin/merge.c:1293) and hands it to
//! `prepare_merge_message()` (:1318-1319), which appends `fmt_merge_msg()`'s
//! shortlog and then drops the last byte with `strbuf_setlen(merge_msg,
//! merge_msg->len - 1)` (:1226-1227). zvcs only stripped a generated message,
//! so `-m`/`-F` with `--log` kept the shortlog's newline and a conflicted
//! merge's MERGE_MSG had an extra blank line before `# Conflicts:`.
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
    /// `main` and `side` both rewrite `file` from a common base; `side` has
    /// two commits so `--log=1` truncates.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-merge-msg-log-strip-{tag}-{}", std::process::id()));
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
        std::fs::write(f.work.join("other"), "x\n").unwrap();
        f.run(&["add", "other"]);
        f.run(&["commit", "-q", "-m", "side2"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "main\n").unwrap();
        f.run(&["commit", "-q", "-am", "main"]);
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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

impl Fixture {
    fn merge_msg_after(&self, args: &[&str]) -> String {
        let (_, _, code) = self.run(args);
        assert_eq!(code, 1, "{args:?}");
        let msg = std::fs::read_to_string(self.work.join(".git/MERGE_MSG")).unwrap();
        assert_eq!(self.run(&["merge", "--abort"]).2, 0);
        msg
    }
}

#[test]
fn an_explicit_message_under_log_loses_the_shortlog_newline() {
    let f = Fixture::new("log");
    std::fs::write(f.root.join("m.txt"), "msg\n").unwrap();
    let file = f.root.join("m.txt");
    let file = file.to_str().unwrap();
    assert_eq!(
        f.merge_msg_after(&["merge", "-m", "xy", "--log", "side"]),
        "xy\n\n* side:\n  side2\n  side\n\n# Conflicts:\n#\tfile\n"
    );
    assert_eq!(
        f.merge_msg_after(&["merge", "-F", file, "--log=1", "side"]),
        "msg\n\n* side: (2 commits)\n  side2\n  ...\n\n# Conflicts:\n#\tfile\n"
    );
    // No shortlog, no strip: the file's own newline survives.
    assert_eq!(
        f.merge_msg_after(&["merge", "-F", file, "side"]),
        "msg\n\n\n# Conflicts:\n#\tfile\n"
    );
}
