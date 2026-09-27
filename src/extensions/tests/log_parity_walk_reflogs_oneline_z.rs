//! `log -g --oneline -z` ended each record with NUL instead of the reflog's newline.
//!
//! Under `-g`, `show_log()` prints a oneline record through
//! `show_reflog_message(…, oneline = 1, …)` — `printf("%s: %s", selector,
//! info->message)`, the message as the reflog stored it, newline included
//! (reflog-walk.c:324-326) — and then `return`s (log-tree.c:835-846), before
//! the record terminator `-z` would have made NUL. A user format keeps its
//! terminator, so `--format=%gs -z` still separates with NUL.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-walk-reflogs-oneline-z-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (file, msg) in [("a", "A"), ("b", "B")] {
            std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
            f.run(&["add", file]);
            f.run(&["commit", "-q", "-m", msg]);
        }
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
            .env("GIT_PAGER", "cat")
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

#[test]
fn the_reflog_message_keeps_its_newline_under_z() {
    let f = Fixture::new("z");
    let b = f.run(&["rev-parse", "--short", "main"]).0;
    let a = f.run(&["rev-parse", "--short", "main~1"]).0;
    let want = format!(
        "{} main@{{0}}: commit: B\n{} main@{{1}}: commit (initial): A\n",
        b.trim_end(),
        a.trim_end()
    );
    assert_eq!(f.run(&["log", "-g", "-z", "--oneline", "main"]), (want, String::new(), 0));
    let out = f.run(&["log", "-g", "-z", "--format=%gs", "main"]);
    assert_eq!(out, ("commit: B\0commit (initial): A\0".to_string(), String::new(), 0));
}
