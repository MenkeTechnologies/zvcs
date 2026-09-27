//! `%m` named the side of a symmetric range only under `--left-right`.
//!
//! `format_commit_one()` expands `%m` as `get_revision_mark(NULL, commit)`,
//! and with no `rev_info` the function's `!revs || revs->left_right` arm
//! answers `<`/`>` from the commit's `SYMMETRIC_LEFT` flag (revision.c:
//! 4716-4734) — which `A...B` sets whatever options follow. zvcs's `log` only
//! computed the side under `--left-right` and the cherry options, and
//! `rev-list --format` always printed `>`, even for a boundary commit (`-`).
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
    /// A; `side` adds S on A; `main` adds B on A, ten seconds later.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-revision-mark-format-{tag}-{}", std::process::id()));
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

    /// The `%m %s` lines, without `rev-list`'s `commit <oid>` headers.
    fn marks(&self, verb: &str, extra: &[&str]) -> String {
        let mut argv = vec![verb, "--format=%m %s"];
        argv.extend_from_slice(extra);
        let (out, err, code) = self.run(&argv, 0);
        assert_eq!((err.as_str(), code), ("", 0), "{verb} {extra:?}");
        out.lines().filter(|l| !l.starts_with("commit ")).map(|l| format!("{l}\n")).collect()
    }
}

#[test]
fn percent_m_names_the_side_without_left_right() {
    let f = Fixture::new("side");
    for verb in ["log", "rev-list"] {
        assert_eq!(f.marks(verb, &["main...side"]), "< B\n> S\n", "{verb}");
        assert_eq!(f.marks(verb, &["--boundary", "main...side"]), "< B\n> S\n- A\n", "{verb}");
        assert_eq!(f.marks(verb, &["main"]), "> B\n> A\n", "{verb}");
    }
}
