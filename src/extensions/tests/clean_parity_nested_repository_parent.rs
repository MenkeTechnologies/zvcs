//! An untracked directory holding a nested repository is still one untracked
//! directory.
//!
//! `treat_directory()` makes a nested repository `path_none` under
//! `DIR_SKIP_NESTED_GIT` — `clean` without a second `-f` — and a plain untracked
//! entry otherwise (dir.c:2034-2038). Either way it does not stop the directory
//! around it from being `path_untracked`, so `status` says `?? ud/`, `clean -n`
//! (no `-d`) skips the directory entirely, and `clean -d` hands `ud/` to
//! `remove_dirs()`, which is what spares the repository inside
//! (`Would skip repository ud/nest`, builtin/clean.c:176-186) and removes the rest.
//!
//! zvcs's collapse refused any directory holding a repository, so it listed the
//! siblings one by one (`clean -n` removed `ud/f` even without `-d`), never
//! announced the skipped repository, and `status` said `?? ud/f` / `?? ud/nest/`.
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
    /// Tracked `t`. Untracked: `ud/` holding the repository `ud/nest` and the file
    /// `ud/f`; `uo/` holding only the repository `uo/nest2`; `ug/deep/nest3`, a
    /// repository with a file two levels down; and the top-level repository `top`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clean-nested-repo-parent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(".", &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("t"), "a\n").unwrap();
        f.run(".", &["add", "t"]);
        f.run(".", &["commit", "-q", "-m", "a"]);
        for repo in ["ud/nest", "uo/nest2", "ug/deep/nest3", "top"] {
            std::fs::create_dir_all(f.work.join(repo)).unwrap();
            f.run(repo, &["init", "-q"]);
        }
        std::fs::write(f.work.join("ud/f"), "x\n").unwrap();
        std::fs::write(f.work.join("ug/deep/nest3/y"), "x\n").unwrap();
        f
    }

    fn run(&self, cwd: &str, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(self.work.join(cwd))
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

/// `remove_dirs()` reports `ud/`'s two entries in readdir order, which is the
/// filesystem's; everything else here has a single entry per directory.
fn sorted_lines(s: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = s.lines().collect();
    lines.sort_unstable();
    lines
}

#[test]
fn the_parent_is_the_candidate_and_remove_dirs_spares_the_repository() {
    let f = Fixture::new("dry");
    let (out, err, code) = f.run(".", &["clean", "-nd"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(
        sorted_lines(&out),
        [
            "Would remove ud/f",
            "Would skip repository ud/nest",
            "Would skip repository ug/deep/nest3",
            "Would skip repository uo/nest2",
        ]
    );
    // The repository line and the removal line of `ud/` come from one
    // `remove_dirs()` call, ahead of `ug/`'s.
    assert!(out.ends_with("Would skip repository ug/deep/nest3\nWould skip repository uo/nest2\n"));

    // A second `-f` lifts `DIR_SKIP_NESTED_GIT` and `REMOVE_DIR_KEEP_NESTED_GIT`:
    // every directory goes whole.
    let (out, err, code) = f.run(".", &["clean", "-ndff"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("Would remove top/\nWould remove ud/\nWould remove ug/\nWould remove uo/\n", "", 0)
    );
}

#[test]
fn without_d_nothing_inside_the_directory_is_a_candidate() {
    let f = Fixture::new("nod");
    let (out, err, code) = f.run(".", &["clean", "-n"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
}

#[test]
fn status_reports_the_parent_directory() {
    let f = Fixture::new("status");
    let (out, err, code) = f.run(".", &["status", "--porcelain"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("?? top/\n?? ud/\n?? ug/\n?? uo/\n", "", 0)
    );
}

#[test]
fn a_real_run_keeps_only_the_repositories() {
    let f = Fixture::new("real");
    let (out, err, code) = f.run(".", &["clean", "-fd"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(
        sorted_lines(&out),
        [
            "Removing ud/f",
            "Skipping repository ud/nest",
            "Skipping repository ug/deep/nest3",
            "Skipping repository uo/nest2",
        ]
    );
    assert!(!f.work.join("ud/f").exists());
    for kept in ["ud/nest/.git", "uo/nest2/.git", "ug/deep/nest3/y", "top/.git"] {
        assert!(f.work.join(kept).exists(), "{kept}");
    }
}
