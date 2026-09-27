//! `log --show-linear-break` was refused.
//!
//! `track_linear()` (revision.c:4333-4352) runs on every commit
//! `get_revision_1()` hands out — `--skip`ped ones included — and calls it
//! linear when it is a parent of the one before; the first always is.
//! `log_tree_commit()` prints `"\n%s\n"` with the bar (twenty spaces and ten
//! dots unless `=<barrier>` says otherwise) ahead of a commit that is not, or
//! behind it in `--reverse`'s output stage (log-tree.c:1188-1197), outside the
//! record separator. `revision_opts_finish()` refuses it with `--graph`
//! (revision.c:2744-2747), for `rev-list` as well.
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
    /// A; `side` adds S on A; `main` adds B on A, ten seconds after S.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-show-linear-break-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], 0);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"], 0);
        f.run(&["commit", "-q", "-m", "A"], 0);
        f.run(&["checkout", "-q", "-b", "side"], 10);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"], 10);
        f.run(&["commit", "-q", "-m", "S"], 10);
        f.run(&["checkout", "-q", "main"], 20);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"], 20);
        f.run(&["commit", "-q", "-m", "B"], 20);
        f
    }

    fn run(&self, args: &[&str], at: u64) -> (String, String, i32) {
        let date = format!("@{} +0000", 1_700_000_000 + at);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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

    fn log(&self, args: &[&str]) -> String {
        let mut argv = vec!["log"];
        argv.extend_from_slice(args);
        argv.push("--all");
        let (out, err, code) = self.run(&argv, 0);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn a_break_marks_each_jump() {
    let f = Fixture::new("break");
    let bar = "                    ..........";
    assert_eq!(f.log(&["--format=%s", "--show-linear-break"]), format!("B\n\n{bar}\nS\nA\n"));
    assert_eq!(f.log(&["--format=%s", "--show-linear-break=XX", "--reverse"]), "A\nS\n\nXX\nB\n");
    // The skipped B still decides that S follows a jump.
    assert_eq!(f.log(&["--format=%s", "--show-linear-break=XX", "--skip=1"]), "\nXX\nS\nA\n");
    // A separator format puts its blank line after the break.
    assert_eq!(f.log(&["--pretty=format:%s", "--show-linear-break=XX"]), "B\nXX\n\nS\nA");
}

#[test]
fn graph_is_refused() {
    let f = Fixture::new("graph");
    let want = (
        String::new(),
        "fatal: options '--show-linear-break' and '--graph' cannot be used together\n".to_string(),
        128,
    );
    for verb in ["log", "rev-list"] {
        assert_eq!(f.run(&[verb, "--graph", "--show-linear-break", "--all"], 0), want, "{verb}");
    }
}
