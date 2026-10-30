//! The `<sub>` column and `<mW>` of a `u` line for a gitlink conflict, and the
//! `is_submodule_modified()` refusal an unmerged gitlink reaches.
//!
//! `run_diff_files()` queues `diff_unmerge()`'s pair for an unmerged path and then
//! compares the stage-2 entry as an ordinary one (diff-lib.c:154-226), so ours gets
//! `ce_compare_gitlink()` and `is_submodule_modified()` like any gitlink, and
//! `wt_status_collect_changed_cb()` folds that second pair's `dirty_submodule`,
//! `new_submodule_commits` and modes into the path's record (wt-status.c:470-525).
//! `wt_porcelain_v2_submodule_state()` reads those, so `u AA` over a submodule with
//! untracked content is `S..U`, with another commit checked out `SC..`; zvcs printed
//! `S...` for every one. With no stage 2 and the directory gone, no field holds a
//! gitlink mode and the token is `N...` (`u DU`), where zvcs tested the stage modes.
//! A `<path>/.git` that is no git directory dies there in every format.
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

/// What `s0/` holds when the command runs.
#[derive(Clone, Copy, Debug)]
enum Sub {
    /// Checked out at stage 2's commit, with an untracked file.
    Untracked,
    /// Checked out at stage 2's commit, with a tracked file changed.
    Modified,
    /// Checked out at stage 1's commit, with an untracked file.
    OtherCommitUntracked,
    /// The directory is gone.
    Removed,
    /// `s0/.git` is a file that is no gitfile.
    JunkGitfile,
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
    /// Two identical repositories, `stock/` and `zvcs/`: a committed `f`, a
    /// submodule `s0` with commits `one` and `two`, and the conflict `stages` (from
    /// `1`, `2`, `3`) recorded for `s0`, stage 1 and 3 at `one`, stage 2 at `two`.
    fn new(tag: &str, stages: &[u8], sub: Sub) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-status-unmerged-gitlink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("f"), "x\n").unwrap();
            f.git(&work, &["add", "f"]);
            f.git(&work, &["commit", "-q", "-m", "base"]);
            let s0 = work.join("s0");
            std::fs::create_dir_all(&s0).unwrap();
            f.git(&s0, &["init", "-q", "-b", "main", "."]);
            std::fs::write(s0.join("a"), "a\n").unwrap();
            f.git(&s0, &["add", "a"]);
            f.git(&s0, &["commit", "-q", "-m", "one"]);
            std::fs::write(s0.join("a"), "b\n").unwrap();
            f.git(&s0, &["commit", "-q", "-a", "-m", "two"]);
            let two = f.git(&s0, &["rev-parse", "HEAD"]).trim().to_string();
            let one = f.git(&s0, &["rev-parse", "HEAD~1"]).trim().to_string();
            let info: String = stages
                .iter()
                .map(|st| {
                    let id = if *st == 2 { &two } else { &one };
                    format!("160000 {id} {st}\ts0\n")
                })
                .collect();
            f.git_stdin(&work, &["update-index", "--index-info"], &info);
            match sub {
                Sub::Untracked => std::fs::write(s0.join("u"), "u\n").unwrap(),
                Sub::Modified => std::fs::write(s0.join("a"), "m\n").unwrap(),
                Sub::OtherCommitUntracked => {
                    f.git(&s0, &["checkout", "-q", "HEAD~1"]);
                    std::fs::write(s0.join("u"), "u\n").unwrap();
                }
                Sub::Removed => std::fs::remove_dir_all(&s0).unwrap(),
                Sub::JunkGitfile => {
                    std::fs::remove_dir_all(s0.join(".git")).unwrap();
                    std::fs::write(s0.join(".git"), "junk\n").unwrap();
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

    fn git_stdin(&self, dir: &Path, args: &[&str], input: &str) {
        use std::io::Write;
        let mut child = Command::new(STOCK)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stock git");
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "stock git {args:?} failed");
    }

    fn compare(&self, args: &[&str]) -> (String, String, i32) {
        let home = self.home();
        let stock = run(STOCK, &self.root.join("stock"), &home, args);
        let zvcs = run(ZVCS, &self.root.join("zvcs"), &home, args);
        assert_eq!(stock, zvcs, "git {args:?} differs (left stock, right zvcs)");
        stock
    }
}

#[test]
fn both_added_gitlink_carries_the_stage_two_dirt() {
    if !stock_available() {
        return;
    }
    let cases: &[(&str, Sub, &str)] = &[
        ("aa-untracked", Sub::Untracked, " S..U "),
        ("aa-modified", Sub::Modified, " S.M. "),
        ("aa-other-commit", Sub::OtherCommitUntracked, " SC.U "),
    ];
    for (tag, sub, token) in cases {
        let f = Fixture::new(tag, &[2, 3], *sub);
        let (out, _, _) = f.compare(&["status", "--porcelain=2"]);
        assert!(out.starts_with(&format!("u AA{token}")), "{tag}: {out}");
        f.compare(&["status", "--porcelain=2", "-z"]);
        f.compare(&["status", "--porcelain=2", "--ignore-submodules=dirty"]);
        f.compare(&["status", "--porcelain=2", "--ignore-submodules=all"]);
        f.compare(&["status", "--porcelain=2", "-uno"]);
        f.compare(&["status"]);
    }
}

#[test]
fn every_conflict_shape_with_ours_reads_ours() {
    if !stock_available() {
        return;
    }
    // UU, UD and AU hold a stage 2; UA does not, so its dirt stays unseen.
    for (tag, stages) in [("uu", &[1u8, 2, 3][..]), ("ud", &[1, 2]), ("au", &[2]), ("ua", &[3])] {
        let f = Fixture::new(tag, stages, Sub::OtherCommitUntracked);
        f.compare(&["status", "--porcelain=2"]);
        f.compare(&["status", "--porcelain=2", "--ignore-submodules=untracked"]);
    }
}

#[test]
fn a_removed_conflict_without_ours_is_no_gitlink() {
    if !stock_available() {
        return;
    }
    let f = Fixture::new("du-removed", &[1, 3], Sub::Removed);
    let (out, _, _) = f.compare(&["status", "--porcelain=2"]);
    assert!(out.starts_with("u DU N... "), "{out}");
    // Ours removed is a deletion pair, which a gitlink under `ignore=all` drops.
    let f = Fixture::new("aa-removed", &[2, 3], Sub::Removed);
    f.compare(&["status", "--porcelain=2"]);
    f.compare(&["status", "--porcelain=2", "--ignore-submodules=all"]);
}

#[test]
fn an_unusable_gitfile_under_ours_dies_in_every_format() {
    if !stock_available() {
        return;
    }
    let f = Fixture::new("junk", &[2, 3], Sub::JunkGitfile);
    let (_, err, code) = f.compare(&["status", "--porcelain=2"]);
    assert_eq!(code, 128, "{err}");
    f.compare(&["status"]);
    f.compare(&["status", "-s"]);
    f.compare(&["status", "--porcelain=2", "--ignore-submodules=dirty"]);
    // Without a stage 2 nothing asks `is_submodule_modified()`.
    let f = Fixture::new("junk-du", &[1, 3], Sub::JunkGitfile);
    let (_, _, code) = f.compare(&["status", "--porcelain=2"]);
    assert_eq!(code, 0);
}
