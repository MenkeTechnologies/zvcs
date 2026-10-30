//! A gitlink staged over an embedded repository that `.gitmodules` does not name.
//!
//! git judges a gitlink from `<path>/.git` alone: `ce_compare_gitlink()`
//! (read-cache.c:268-284) resolves the nested `HEAD` and compares it with the
//! recorded id, and `is_submodule_modified()` (submodule.c:1880-1902) reads
//! `<path>/.git` as a gitfile or a directory and asks that repository's status.
//! `.gitmodules` only feeds `set_diffopt_flags_from_submodule_config()`, and
//! `submodule_from_path()` returning NULL there leaves the diff-level ignore
//! setting in force (submodule.c:180-200). zvcs's submodule walk skipped every
//! gitlink `.gitmodules` did not list, so `diff-files`, `diff` and `status` called
//! a moved or dirty embedded repository clean.
//!
//! The same fixtures also pin two things that held for registered submodules
//! too: `--ignore-submodules=dirty` (and `diff.ignoreSubmodules=dirty`) keeps
//! `match_stat_with_submodule()` from probing at all, so a gitlink whose `HEAD`
//! did not move is no change in `git diff`; and `diff-index` now runs the same
//! gitlink comparison over the worktree, shows `Subproject commit <id>[-dirty]`
//! (`diff_populate_gitlink()`, diff.c:4449-4459) and drops ignored gitlink pairs
//! (`is_submodule_ignored()`, diff.c:7552).
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr and the exit status.

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

/// What the embedded repository looks like when the command runs.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Its `HEAD` gained a commit the gitlink does not record.
    Moved,
    /// An untracked file inside it.
    Untracked,
    /// A tracked file inside it modified, not staged.
    Modified,
    /// `HEAD` moved, a tracked file modified and an untracked file added.
    All,
    /// Its `.git` moved out of the worktree and replaced by a gitfile, then `HEAD` moved.
    GitfileMoved,
    /// The gitfile layout with only an untracked file inside.
    GitfileUntracked,
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
    /// Two identical trees, `stock/` and `zvcs/`, each a repository with `top`
    /// committed and the embedded repository `inner` staged as a gitlink with no
    /// `.gitmodules`, then reshaped by `shape`. Stock git builds both.
    fn new(tag: &str, shape: Shape) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-unregistered-gitlink-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            f.build(side, shape);
        }
        f
    }

    fn work(&self, side: &str) -> PathBuf {
        self.root.join(side).join("r")
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let (_, err, code) = run(STOCK, dir, &self.root, args);
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
    }

    fn build(&self, side: &str, shape: Shape) {
        let base = self.root.join(side);
        let work = base.join("r");
        std::fs::create_dir_all(&work).unwrap();
        self.git(&work, &["init", "-q", "-b", "main", "."]);
        let inner = work.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        self.git(&inner, &["init", "-q", "-b", "main", "."]);
        std::fs::write(inner.join("f"), "a\n").unwrap();
        std::fs::write(inner.join("g"), "k\n").unwrap();
        self.git(&inner, &["add", "f", "g"]);
        self.git(&inner, &["commit", "-q", "-m", "i"]);
        std::fs::write(work.join("top"), "t\n").unwrap();
        self.git(&work, &["-c", "advice.addEmbeddedRepo=false", "add", "top", "inner"]);
        self.git(&work, &["commit", "-q", "-m", "top"]);
        let advance = || {
            std::fs::write(inner.join("f"), "b\n").unwrap();
            self.git(&inner, &["commit", "-q", "-a", "-m", "i2"]);
        };
        let gitfile = || {
            let moved = base.join("innergd");
            std::fs::rename(inner.join(".git"), &moved).unwrap();
            std::fs::write(inner.join(".git"), format!("gitdir: {}\n", moved.display())).unwrap();
        };
        match shape {
            Shape::Moved => advance(),
            Shape::Untracked => std::fs::write(inner.join("u"), "u\n").unwrap(),
            Shape::Modified => std::fs::write(inner.join("f"), "m\n").unwrap(),
            Shape::All => {
                advance();
                std::fs::write(inner.join("u"), "u\n").unwrap();
                std::fs::write(inner.join("g"), "m\n").unwrap();
            }
            Shape::GitfileMoved => {
                gitfile();
                advance();
            }
            Shape::GitfileUntracked => {
                gitfile();
                std::fs::write(inner.join("u"), "u\n").unwrap();
            }
        }
    }

    /// Run `args` in both trees and require byte-identical results. The trees'
    /// own paths only appear in a gitfile's target, which no compared command
    /// prints, so the outputs are compared as they are.
    fn same(&self, args: &[&str]) {
        let stock = run(STOCK, &self.work("stock"), &self.root, args);
        let zvcs = run(ZVCS, &self.work("zvcs"), &self.root, args);
        assert_eq!(zvcs, stock, "zvcs vs stock for {args:?}");
    }
}

const SHAPES: [Shape; 6] = [
    Shape::Moved,
    Shape::Untracked,
    Shape::Modified,
    Shape::All,
    Shape::GitfileMoved,
    Shape::GitfileUntracked,
];

fn each_shape(tag: &str, commands: &[&[&str]]) {
    if !stock_available() {
        return;
    }
    for shape in SHAPES {
        let f = Fixture::new(&format!("{tag}-{shape:?}"), shape);
        for args in commands {
            f.same(args);
        }
    }
}

#[test]
fn diff_files_reports_the_unregistered_gitlink() {
    each_shape(
        "diff-files",
        &[
            &["diff-files"],
            &["diff-files", "-p"],
            &["diff-files", "--ignore-submodules=none"],
            &["diff-files", "--ignore-submodules=dirty"],
            &["diff-files", "--ignore-submodules=all"],
        ],
    );
}

#[test]
fn status_reports_the_unregistered_gitlink_in_every_format() {
    each_shape(
        "status",
        &[
            &["status"],
            &["status", "--short"],
            &["status", "--porcelain"],
            &["status", "--porcelain=v2"],
            &["status", "--porcelain=v2", "-uno"],
            &["status", "--short", "--ignore-submodules=none"],
            &["status", "--short", "--ignore-submodules=untracked"],
            &["status", "--short", "--ignore-submodules=dirty"],
            &["status", "--short", "--ignore-submodules=all"],
        ],
    );
}

#[test]
fn diff_reports_the_unregistered_gitlink_and_honours_ignore_dirty() {
    each_shape(
        "diff",
        &[
            &["diff"],
            &["diff", "HEAD"],
            &["diff", "--raw"],
            &["diff", "--stat"],
            &["diff", "--ignore-submodules=none"],
            &["diff", "--ignore-submodules=untracked"],
            &["diff", "--ignore-submodules=dirty"],
            &["diff", "HEAD", "--ignore-submodules=dirty"],
            &["diff", "--ignore-submodules"],
            &["-c", "diff.ignoreSubmodules=dirty", "diff"],
            &["-c", "diff.ignoreSubmodules=none", "diff"],
        ],
    );
}

#[test]
fn diff_index_compares_the_worktree_gitlink() {
    each_shape(
        "diff-index",
        &[
            &["diff-index", "HEAD"],
            &["diff-index", "-p", "HEAD"],
            &["diff-index", "-R", "-p", "HEAD"],
            &["diff-index", "--stat", "HEAD"],
            &["diff-index", "--numstat", "-p", "HEAD"],
            &["diff-index", "--ignore-submodules=none", "-p", "HEAD"],
            &["diff-index", "--ignore-submodules=dirty", "HEAD"],
            &["diff-index", "--ignore-submodules", "HEAD"],
            &["-c", "diff.ignoreSubmodules=dirty", "diff-index", "-p", "HEAD"],
        ],
    );
}

/// `git add -u` restages the gitlink at the nested repository's new `HEAD`;
/// the index each binary leaves is then compared through stock git.
#[test]
fn add_update_restages_the_moved_gitlink() {
    if !stock_available() {
        return;
    }
    for shape in [Shape::Moved, Shape::GitfileMoved, Shape::Modified] {
        let f = Fixture::new(&format!("add-u-{shape:?}"), shape);
        let stock = run(STOCK, &f.work("stock"), &f.root, &["add", "-u"]);
        let zvcs = run(ZVCS, &f.work("zvcs"), &f.root, &["add", "-u"]);
        assert_eq!(zvcs, stock, "add -u for {shape:?}");
        for args in [&["ls-files", "-s"][..], &["diff-index", "--cached", "HEAD"][..]] {
            let s = run(STOCK, &f.work("stock"), &f.root, args);
            let z = run(STOCK, &f.work("zvcs"), &f.root, args);
            assert_eq!(z, s, "index after add -u for {shape:?}: {args:?}");
        }
    }
}
