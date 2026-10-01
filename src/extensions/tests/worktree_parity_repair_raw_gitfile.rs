//! `git worktree repair` judging a worktree's `.git` file by what it records.
//!
//! git 2.56.0's `repair_gitfile()` reads `<wt>/.git` with `read_gitfile_raw()`
//! (worktree.c:671, setup.c:1005-1060) instead of `read_gitfile_gently()`, so the
//! absolute/relative test `use_relative_paths == is_absolute_path(dotgit_contents)`
//! (worktree.c:691-692) sees the recorded text instead of an always-absolute
//! realpath. A plain `worktree repair` therefore rewrites a relative link as an
//! absolute one, and `--relative-paths` leaves an already-relative link alone.
//! An absolute recording is no longer realpath'd before it is compared with the
//! administrative directory, and an unresolvable one is "broken" because the
//! `is_git_directory()` probe moved into `repair_gitfile()` (:687). zvcs kept
//! 2.55.0's behaviour.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
            .join(format!("zvcs-worktree-repair-raw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        // Every path git prints and writes is a realpath; resolve the temp
        // directory's own symlinks (macOS `/var` -> `/private/var`) up front.
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("repo");
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.work.join(rel)).unwrap()
    }

    fn write(&self, rel: &str, contents: &str) {
        std::fs::write(self.work.join(rel), contents).unwrap();
    }

    /// `<root>/<rel>` as text.
    fn abs(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }
}

#[test]
fn a_plain_repair_rewrites_a_relative_link_as_an_absolute_one() {
    let f = Fixture::new("to-absolute");
    f.run(&["worktree", "add", "-q", "--relative-paths", "../wt"]);
    assert_eq!(f.read("../wt/.git"), "gitdir: ../repo/.git/worktrees/wt\n");
    assert_eq!(f.read(".git/worktrees/wt/gitdir"), "../../../../wt/.git\n");

    assert_eq!(
        f.run(&["worktree", "repair"]),
        (
            String::new(),
            format!("repair: .git file absolute/relative path mismatch: {}\n", f.abs("wt")),
            0
        )
    );
    assert_eq!(f.read("../wt/.git"), format!("gitdir: {}\n", f.abs("repo/.git/worktrees/wt")));
    assert_eq!(f.read(".git/worktrees/wt/gitdir"), format!("{}\n", f.abs("wt/.git")));

    // Now absolute, a second plain run has nothing to say.
    assert_eq!(f.run(&["worktree", "repair"]), (String::new(), String::new(), 0));
}

#[test]
fn relative_paths_leaves_an_already_relative_link_alone() {
    let f = Fixture::new("stay-relative");
    f.run(&["worktree", "add", "-q", "--relative-paths", "../wt"]);
    assert_eq!(f.run(&["worktree", "repair", "--relative-paths"]), (String::new(), String::new(), 0));
    assert_eq!(f.read("../wt/.git"), "gitdir: ../repo/.git/worktrees/wt\n");

    // The path-argument form reports the bad argument and still repairs the rest.
    let (out, err, code) = f.run(&["worktree", "repair", "nosuchpath"]);
    assert_eq!(
        (out, err, code),
        (
            String::new(),
            format!(
                "error: not a valid path: nosuchpath\n\
                 repair: .git file absolute/relative path mismatch: {}\n",
                f.abs("wt")
            ),
            1
        )
    );
    assert_eq!(f.read("../wt/.git"), format!("gitdir: {}\n", f.abs("repo/.git/worktrees/wt")));
}

#[test]
fn an_unresolvable_relative_recording_is_broken() {
    let f = Fixture::new("broken");
    f.run(&["worktree", "add", "-q", "../wt"]);
    f.write("../wt/.git", "gitdir: nowhere\n");
    assert_eq!(
        f.run(&["worktree", "repair"]),
        (String::new(), format!("repair: .git file broken: {}\n", f.abs("wt")), 0)
    );
    assert_eq!(f.read("../wt/.git"), format!("gitdir: {}\n", f.abs("repo/.git/worktrees/wt")));
}
