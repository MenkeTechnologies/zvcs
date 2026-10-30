//! `update-index --refresh` and `add --refresh` over a gitlink whose `HEAD` does not
//! resolve.
//!
//! `refresh_cache_ent()` compares a gitlink through `ie_match_stat()`, whose
//! `ce_match_stat_basic()` asks `ce_compare_gitlink()` about a directory
//! (read-cache.c:331-337), and that calls a `HEAD` `repo_resolve_gitlink_ref()`
//! cannot read unchanged (read-cache.c:268-284): no `.git`, a `.git` file that is no
//! gitfile, a `.git` directory that is no repository, an unborn branch. Refresh
//! never calls `is_submodule_modified()`, so none of them is `needs update`; zvcs
//! printed `s0: needs update` and exited 1. A `HEAD` at another commit, and anything
//! but a directory, stay stale.
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr, the exit status and the index left behind.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// What `s0/` holds when the refresh runs.
#[derive(Clone, Copy, Debug)]
enum Sub {
    /// `s0/.git` is a file that is no gitfile.
    JunkGitfile,
    /// `s0/.git` is an empty directory.
    EmptyGitDir,
    /// `s0/.git` names a git directory that does not exist.
    DanglingGitfile,
    /// `s0/` is a fresh repository with no commit.
    Unborn,
    /// `s0/` holds no `.git` at all.
    NoGit,
    /// Checked out one commit behind the recorded one.
    OtherCommit,
    /// `s0` is a regular file.
    File,
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two identical repositories, `stock/` and `zvcs/`, each committing `f` and a
    /// gitlink `s0` added from its checked-out submodule, `s0/` then shaped by `sub`.
    fn new(tag: &str, sub: Sub) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-update-index-gitlink-refresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("f"), "x\n").unwrap();
            let s0 = work.join("s0");
            std::fs::create_dir_all(&s0).unwrap();
            f.git(&s0, &["init", "-q", "-b", "main", "."]);
            std::fs::write(s0.join("a"), "a\n").unwrap();
            f.git(&s0, &["add", "a"]);
            f.git(&s0, &["commit", "-q", "-m", "one"]);
            std::fs::write(s0.join("a"), "b\n").unwrap();
            f.git(&s0, &["commit", "-q", "-a", "-m", "two"]);
            f.git(&work, &["add", "f", "s0"]);
            f.git(&work, &["commit", "-q", "-m", "top"]);
            match sub {
                Sub::JunkGitfile => {
                    std::fs::remove_dir_all(s0.join(".git")).unwrap();
                    std::fs::write(s0.join(".git"), "junk\n").unwrap();
                }
                Sub::EmptyGitDir => {
                    std::fs::remove_dir_all(s0.join(".git")).unwrap();
                    std::fs::create_dir(s0.join(".git")).unwrap();
                }
                Sub::DanglingGitfile => {
                    std::fs::remove_dir_all(s0.join(".git")).unwrap();
                    std::fs::write(s0.join(".git"), "gitdir: /nonexistent/zvcs-gitdir\n").unwrap();
                }
                Sub::Unborn => {
                    std::fs::remove_dir_all(&s0).unwrap();
                    std::fs::create_dir(&s0).unwrap();
                    f.git(&s0, &["init", "-q", "-b", "main", "."]);
                }
                Sub::NoGit => std::fs::remove_dir_all(s0.join(".git")).unwrap(),
                Sub::OtherCommit => {
                    f.git(&s0, &["checkout", "-q", "HEAD~1"]);
                }
                Sub::File => {
                    std::fs::remove_dir_all(&s0).unwrap();
                    std::fs::write(&s0, "f\n").unwrap();
                }
            }
        }
        f
    }

    fn home(&self) -> PathBuf {
        let home = self.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = run(STOCK, dir, &self.home(), args);
        assert_eq!(code, 0, "stock git {args:?} failed: {err}");
        out
    }

    fn compare(&self, args: &[&str]) -> (String, String, i32) {
        let home = self.home();
        let stock_dir = self.root.join("stock");
        let zvcs_dir = self.root.join("zvcs");
        let stock = run(STOCK, &stock_dir, &home, args);
        let zvcs = run(ZVCS, &zvcs_dir, &home, args);
        assert_eq!(stock, zvcs, "git {args:?} differs (left stock, right zvcs)");
        let index = |dir: &Path| run(STOCK, dir, &home, &["ls-files", "-s"]).0;
        assert_eq!(index(&stock_dir), index(&zvcs_dir), "index after git {args:?}");
        stock
    }
}

const REFRESHES: &[&[&str]] = &[
    &["update-index", "--refresh"],
    &["update-index", "--really-refresh"],
    &["update-index", "-q", "--refresh"],
    &["update-index", "--refresh", "--ignore-submodules"],
    &["update-index", "--really-refresh", "--ignore-submodules"],
    &["add", "--refresh", "s0"],
    &["add", "--refresh", "."],
];

#[test]
fn an_unresolvable_gitlink_head_is_unchanged() {
    if !stock_available() {
        return;
    }
    let cases = [
        ("junk", Sub::JunkGitfile),
        ("empty-git-dir", Sub::EmptyGitDir),
        ("dangling", Sub::DanglingGitfile),
        ("unborn", Sub::Unborn),
        ("no-git", Sub::NoGit),
    ];
    for (tag, sub) in cases {
        let f = Fixture::new(tag, sub);
        for args in REFRESHES {
            let (out, err, code) = f.compare(args);
            assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "{tag}: git {args:?}");
        }
    }
}

#[test]
fn a_moved_or_replaced_gitlink_still_needs_update() {
    if !stock_available() {
        return;
    }
    for (tag, sub) in [("other-commit", Sub::OtherCommit), ("file", Sub::File)] {
        let f = Fixture::new(tag, sub);
        for args in REFRESHES {
            f.compare(args);
        }
        let (out, _, code) = f.compare(&["update-index", "--refresh"]);
        assert_eq!((out.as_str(), code), ("s0: needs update\n", 1), "{tag}");
    }
}
