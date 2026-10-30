//! A gitlink whose `<path>/.git` is a directory that is no git directory, as
//! `add`, `commit -a` and `describe --dirty` reach it, and the worktree mode
//! `status --porcelain=2` gives a gitlink whose repository has no commit.
//!
//! `add_files_to_cache()` (read-cache.c:4027-4064) runs `run_diff_files()` with
//! `override_submodule_config` and no ignore flag, so `match_stat_with_submodule()`
//! (diff-lib.c:88-107) hands every unchanged gitlink the pathspec selects to
//! `is_submodule_modified()`, which dies with `'<path>/.git' not recognized as a git
//! repository` (submodule.c:1892-1899) before anything is staged. `git add <path>`,
//! `add -A`, `add -u` and `commit -a` all go through it; zvcs staged the rest and
//! exited 0.
//!
//! `describe --dirty` runs `diff-index --quiet HEAD --` in process
//! (builtin/describe.c:756-779): its walk stops at the first path that queued a
//! change, so the gitlink is only fatal when no change sorts ahead of it. zvcs
//! never died.
//!
//! `check_removed()` keeps a gitlink over a directory whatever is inside it, and
//! `ce_mode_from_stat()` turns the directory into `160000` (diff-lib.c:58-74,
//! read-cache.h:16-29, object.h:140-141); zvcs printed `000000` for a submodule
//! with no commit.
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

/// What `sub/` holds when the command runs.
#[derive(Clone, Copy, Debug)]
enum Sub {
    /// `sub/.git` is a directory holding one stray file.
    JunkGitDir,
    /// `sub/` is a repository with no commit and one untracked file.
    Unborn,
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
    /// Two identical repositories, `stock/` and `zvcs/`, each with `a`, `z` and a
    /// gitlink `sub` committed and tagged `v1`, `sub/` shaped by `sub`, and then
    /// `setup` run by stock git. Stock git builds both.
    fn new(tag: &str, sub: Sub, setup: &[&[&str]]) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-add-not-a-repo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        f.git(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("s"), "s\n").unwrap();
        f.git(&src, &["add", "s"]);
        f.git(&src, &["commit", "-q", "-m", "sub"]);
        let sub_head = f.git(&src, &["rev-parse", "HEAD"]).trim().to_string();
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("a"), "a\n").unwrap();
            std::fs::write(work.join("z"), "z\n").unwrap();
            f.git(&work, &["add", "a", "z"]);
            f.git(&work, &["update-index", "--add", "--cacheinfo", &format!("160000,{sub_head},sub")]);
            f.git(&work, &["commit", "-q", "-m", "top"]);
            f.git(&work, &["tag", "v1"]);
            let dir = work.join("sub");
            match sub {
                Sub::JunkGitDir => {
                    std::fs::create_dir_all(dir.join(".git")).unwrap();
                    std::fs::write(dir.join(".git/junk"), "junk\n").unwrap();
                }
                Sub::Unborn => {
                    std::fs::create_dir_all(&dir).unwrap();
                    f.git(&dir, &["init", "-q", "-b", "main", "."]);
                    std::fs::write(dir.join("u"), "u\n").unwrap();
                }
            }
            f.git(&work, &["update-index", "-q", "--refresh"]);
            for step in setup {
                if step[0] == "write" {
                    std::fs::write(work.join(step[1]), step[2]).unwrap();
                } else {
                    f.git(&work, step);
                }
            }
        }
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = run(STOCK, dir, &self.root, args);
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
        out
    }

    /// Run `args` in both repositories; compare the output, and the index and
    /// refs each run leaves.
    fn check(&self, args: &[&str]) {
        let stock = run(STOCK, &self.root.join("stock"), &self.root, args);
        let zvcs = run(ZVCS, &self.root.join("zvcs"), &self.root, args);
        assert_eq!(zvcs, stock, "git {args:?}");
        for probe in [&["ls-files", "-s"][..], &["for-each-ref"][..], &["log", "--oneline", "--all"][..]] {
            let stock_after = run(STOCK, &self.root.join("stock"), &self.root, probe);
            let zvcs_after = run(STOCK, &self.root.join("zvcs"), &self.root, probe);
            assert_eq!(zvcs_after, stock_after, "{probe:?} after git {args:?}");
        }
    }
}

const MODIFY_A: &[&str] = &["write", "a", "a2\n"];
const MODIFY_Z: &[&str] = &["write", "z", "z2\n"];

#[test]
fn add_dies_before_staging_anything() {
    if !stock_available() {
        return;
    }
    for args in [
        &["add", "-A"][..],
        &["add", "-u"][..],
        &["add", "."][..],
        &["add", "sub"][..],
        &["add", "-n", "-A"][..],
        &["add", "-v", "a", "sub", "z"][..],
    ] {
        let f = Fixture::new("add", Sub::JunkGitDir, &[MODIFY_A, MODIFY_Z]);
        f.check(args);
    }
}

#[test]
fn add_outside_the_gitlink_stages() {
    if !stock_available() {
        return;
    }
    for args in [&["add", "a"][..], &["add", "-u", "z"][..], &["add", "--refresh"][..]] {
        let f = Fixture::new("add-outside", Sub::JunkGitDir, &[MODIFY_A, MODIFY_Z]);
        f.check(args);
    }
}

#[test]
fn add_reports_the_unmatched_pathspec_first() {
    if !stock_available() {
        return;
    }
    let f = Fixture::new("add-unmatched", Sub::JunkGitDir, &[MODIFY_A]);
    f.check(&["add", "-A", "nosuch"]);
}

#[test]
fn commit_all_dies_before_committing() {
    if !stock_available() {
        return;
    }
    for args in [&["commit", "-q", "-a", "-m", "c"][..], &["commit", "--dry-run", "-a"][..]] {
        let f = Fixture::new("commit", Sub::JunkGitDir, &[MODIFY_A]);
        f.check(args);
    }
}

#[test]
fn describe_dirty_dies_unless_a_change_sorts_ahead() {
    if !stock_available() {
        return;
    }
    let cases: &[(&str, &[&[&str]])] = &[
        ("clean", &[]),
        ("worktree-ahead", &[MODIFY_A]),
        ("worktree-behind", &[MODIFY_Z]),
        ("staged-ahead", &[MODIFY_A, &["add", "a"]]),
        ("staged-behind", &[MODIFY_Z, &["update-index", "z"]]),
        ("ignore-dirty", &[&["config", "submodule.sub.ignore", "dirty"]]),
    ];
    for (tag, setup) in cases {
        for args in [&["describe", "--dirty"][..], &["describe", "--dirty=-X", "--always"][..]] {
            let f = Fixture::new(&format!("describe-{tag}"), Sub::JunkGitDir, setup);
            f.check(args);
        }
    }
}

#[test]
fn status_porcelain_v2_unborn_gitlink_mode() {
    if !stock_available() {
        return;
    }
    let f = Fixture::new("status-unborn", Sub::Unborn, &[]);
    f.check(&["status", "--porcelain=2"]);
    f.check(&["status", "--porcelain=2", "-z"]);
    let f = Fixture::new("status-unborn-added", Sub::Unborn, &[&["rm", "-q", "--cached", "sub"]]);
    for side in ["stock", "zvcs"] {
        f.git(
            &f.root.join(side),
            &["update-index", "--add", "--cacheinfo", "160000,1234567890123456789012345678901234567890,sub"],
        );
    }
    f.check(&["status", "--porcelain=2"]);
}

/// `diff-index` reaches the gitlink only through `oneway_diff()`, which turns away a
/// path outside the pathspec first and stops a `--quiet` walk at the first change
/// (diff-lib.c:540-553). `describe --broken` reads that child's exit status.
#[test]
fn diff_index_stops_before_the_gitlink() {
    if !stock_available() {
        return;
    }
    let cases: &[(&str, &[&[&str]])] = &[("ahead", &[MODIFY_A]), ("behind", &[MODIFY_Z])];
    for (tag, setup) in cases {
        for args in [
            &["diff-index", "--quiet", "HEAD", "--"][..],
            &["diff-index", "--exit-code", "HEAD", "--"][..],
            &["diff-index", "HEAD", "--", "a", "z"][..],
            &["diff-index", "--quiet", "HEAD", "--", "z"][..],
            &["diff-index", "--quiet", "--diff-filter=M", "HEAD", "--"][..],
            &["describe", "--broken"][..],
            &["describe", "--broken=-B", "--dirty=-D"][..],
        ] {
            let f = Fixture::new(&format!("diff-index-{tag}"), Sub::JunkGitDir, setup);
            f.check(args);
        }
    }
}
