//! `log -g` ignored the history-limiting options that are not orderings.
//!
//! `setup_revisions()` dies `cannot combine --walk-reflogs with
//! history-limiting options` whenever `revs->limited` is set under `-g`
//! (revision.c:3180-3181). `--ancestry-path`, `--left-only`, `--right-only`,
//! `--cherry`, `--cherry-mark` and `--cherry-pick` all set it where they are
//! parsed (revision.c:2405-2517). zvcs's `log` refused only the orderings,
//! `--graph`, `--children` and `--simplify-merges`, walked the reflog for the
//! rest, and reached `--ancestry-path`'s own "no bottom commits" die instead.
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
            .join(format!("zvcs-log-walk-reflogs-limited-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
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
fn every_limiting_option_is_refused() {
    let f = Fixture::new("limited");
    let want = (
        String::new(),
        "fatal: cannot combine --walk-reflogs with history-limiting options\n".to_string(),
        128,
    );
    for opt in [
        "--ancestry-path",
        "--left-only",
        "--right-only",
        "--cherry",
        "--cherry-mark",
        "--cherry-pick",
        "--simplify-by-decoration",
    ] {
        assert_eq!(f.run(&["log", "-g", "--oneline", opt, "main"]), want, "{opt}");
    }
    // Not limiting: the walk goes ahead.
    let (_, _, code) = f.run(&["log", "-g", "--oneline", "--first-parent", "main"]);
    assert_eq!(code, 0);
}
