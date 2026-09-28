//! `--max-count-oldest` against `--max-count`, `--skip`, `-<n>` and `-n`.
//!
//! `handle_revision_opt()` runs each `die_for_incompatible_opt2()` test before
//! `parse_count()` reads the value (revision.c:2341-2364), so a conflict is
//! named even when the value is not an integer. And `-<digits>`, `-n <n>` and
//! `-n<n>` only set `revs->max_count` (revision.c:2366-2378): they never test
//! the conflict and leave `max_count_type` alone, so after `--max-count-oldest`
//! they change how many of the oldest commits are kept. zvcs parsed the value
//! first and treated `-<n>`/`-n` as `--max-count`.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-max-count-oldest-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
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

fn three(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f.commit("a", "three\n", "three");
    f
}

fn fatal(message: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: {message}\n"), 128)
}

const MC: &str = "options '--max-count' and '--max-count-oldest' cannot be used together";
const SK: &str = "options '--skip' and '--max-count-oldest' cannot be used together";

#[test]
fn the_conflict_is_named_before_the_value_is_read() {
    let f = three("order");
    for verb in ["log", "rev-list", "shortlog"] {
        assert_eq!(f.run(&[verb, "--skip=1", "--max-count-oldest=x", "main"]), fatal(SK), "{verb}");
        assert_eq!(f.run(&[verb, "--max-count=1", "--max-count-oldest=x", "main"]), fatal(MC), "{verb}");
        assert_eq!(f.run(&[verb, "--max-count-oldest=1", "--skip=x", "main"]), fatal(SK), "{verb}");
        assert_eq!(f.run(&[verb, "--max-count-oldest=1", "--max-count=x", "main"]), fatal(MC), "{verb}");
    }
}

#[test]
fn a_head_count_after_it_resizes_the_oldest_window() {
    let f = three("resize");
    let (one, two, three) = (f.rev("main~2"), f.rev("main~1"), f.rev("main"));
    let ok = |s: String| (s, String::new(), 0);
    assert_eq!(f.run(&["rev-list", "--max-count-oldest=2", "-n1", "main"]), ok(format!("{one}\n")));
    assert_eq!(f.run(&["rev-list", "--max-count-oldest=2", "-n", "1", "main"]), ok(format!("{one}\n")));
    assert_eq!(
        f.run(&["rev-list", "--max-count-oldest=1", "-3", "main"]),
        ok(format!("{three}\n{two}\n{one}\n"))
    );
    assert_eq!(
        f.run(&["log", "--format=%s", "--max-count-oldest=1", "-2", "main"]),
        ok("two\none\n".to_string())
    );
    // Before it, a head count is an ordinary `--max-count` and still conflicts.
    assert_eq!(f.run(&["rev-list", "-n1", "--max-count-oldest=2", "main"]), fatal(MC));
}
