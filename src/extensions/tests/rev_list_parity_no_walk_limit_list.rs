//! `--ancestry-path`, `--left-only`, `--right-only` and `--cherry-pick` are
//! `limit_list()`'s filters (`revision.c:1456-1503`), and `limit_list()` is never
//! reached under `--no-walk`:
//!
//! ```c
//! if (revs->no_walk)
//!         return 0;
//! if (revs->limited) {
//!         if (limit_list(revs) < 0)
//! ```
//! (`prepare_revision_walk()`, `revision.c:4051-4054`, v2.56.0)
//!
//! So `--no-walk --ancestry-path HEAD~2` lists `HEAD~2` instead of dying with
//! "no bottom commits", and the side filters keep every named commit. A `^<rev>`
//! still clears `revs->no_walk` (`revision.c:304-305`) and brings them back.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    tick: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` is three commits; `side` forks at the second with one of its own.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-nowalk-limit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, tick: 1_700_000_000 };
        f.git(&["init", "-q", "-b", "main", "."]);
        for n in ["1", "2", "3"] {
            f.commit_file("f", n);
        }
        f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
        f.commit_file("g", "s");
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn commit_file(&mut self, name: &str, msg: &str) {
        std::fs::write(self.work.join(name), format!("{msg}\n")).unwrap();
        self.git(&["add", name]);
        self.tick += 100;
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", format!("{} +0000", self.tick))
            .env("GIT_COMMITTER_DATE", format!("{} +0000", self.tick))
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && err.is_empty(), "`git {args:?}`: {err}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn no_walk_skips_the_limit_list_filters() {
    let f = Fixture::new("filters");
    let head2 = f.stdout(&["rev-parse", "HEAD~2"]);
    let both = f.stdout(&["rev-list", "--no-walk", "main", "side"]);
    assert_eq!(both.lines().count(), 2);

    for verb in [&["rev-list"][..], &["log", "--format=%H"][..]] {
        let run = |extra: &[&str]| f.stdout(&[verb, extra].concat());
        assert_eq!(run(&["--no-walk", "--ancestry-path", "HEAD~2"]), head2, "{verb:?}");
        assert_eq!(run(&["--no-walk", "--ancestry-path", "main", "side"]), both, "{verb:?}");
        assert_eq!(run(&["--no-walk", "--ancestry-path=side", "main", "side"]), both, "{verb:?}");
        assert_eq!(run(&["--no-walk", "--left-only", "main", "side"]), both, "{verb:?}");
        assert_eq!(run(&["--no-walk", "--right-only", "main", "side"]), both, "{verb:?}");
        assert_eq!(run(&["--no-walk", "--cherry-pick", "main", "side"]), both, "{verb:?}");
    }
    // The verbose-header path takes the slow walk rather than the streaming one.
    let header = f.stdout(&["rev-list", "--no-walk", "--header", "--ancestry-path", "HEAD~2"]);
    assert_eq!(header.lines().next(), head2.lines().next());
}

/// A negative operand clears `revs->no_walk`, so the walk is limited again and
/// `--ancestry-path` finds its bottom.
#[test]
fn a_negative_operand_restores_the_limited_walk() {
    let f = Fixture::new("negative");
    for verb in [&["rev-list"][..], &["log", "--format=%H"][..]] {
        let walked = f.stdout(&[verb, &["--ancestry-path", "^HEAD~1", "main", "side"]].concat());
        assert_eq!(walked.lines().count(), 2, "{verb:?}");
        let out = f.stdout(&[verb, &["--no-walk", "--ancestry-path", "^HEAD~1", "main", "side"]].concat());
        assert_eq!(out, walked, "{verb:?}");
    }
}
