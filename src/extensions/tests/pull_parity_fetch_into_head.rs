//! `git pull <url> <branch>:<branch>` into the branch `HEAD` names.
//!
//! `run_fetch()` starts its fetch as `fetch --update-head-ok` (builtin/pull.c:392),
//! so the fetch may write the checked-out branch. `cmd_pull()` then compares
//! `HEAD` with the `orig_head` it read before the fetch (:1068-1069): when both
//! exist and differ it warns and runs `checkout_fast_forward(orig_head,
//! curr_head)`, dying with the `Cannot fast-forward your working tree` recipe if
//! that refuses (:1093-1118). The unborn case is decided on `orig_head` too, and
//! `pull_into_void()` updates `HEAD` from the value the fetch left, `curr_head`
//! (:490). zvcs never passed `--update-head-ok`, so every such pull died in the
//! fetch with `refusing to fetch into branch`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src` with a commit adding `a`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pull-fetch-into-head-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let f = Fixture { root };
        f.run("src", &["init", "-q", "-b", "main", "."]);
        f.commit("a");
        f
    }

    /// Commit a new file `name` (content `name`) in `src`.
    fn commit(&self, name: &str) {
        std::fs::write(self.root.join("src").join(name), format!("{name}\n")).unwrap();
        self.run("src", &["add", name]);
        self.run("src", &["commit", "-q", "-m", name]);
    }

    fn run(&self, dir: &str, args: &[&str]) -> (String, String, i32) {
        let cwd: &Path = &self.root.join(dir);
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
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

#[test]
fn an_unborn_branch_is_filled_from_its_own_fetch() {
    let f = Fixture::new("unborn");
    f.run(".", &["init", "-q", "-b", "main", "w"]);
    let (out, err, code) = f.run("w", &["pull", "../src", "main:main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "From ../src\n * [new branch]      main       -> main\n", 0)
    );
    assert_eq!(f.run("w", &["status", "--short"]).0, "");
    assert_eq!(
        f.run("w", &["reflog", "--format=%gs"]).0,
        "initial pull\npull ../src main:main: storing head\n"
    );
}

#[test]
fn a_moved_head_fast_forwards_the_worktree_first() {
    let f = Fixture::new("ff");
    f.run(".", &["clone", "-q", "src", "c"]);
    let old = f.run("c", &["rev-parse", "HEAD"]).0;
    f.commit("b");
    let new = f.run("src", &["rev-parse", "--short", "HEAD"]).0;
    let (out, err, code) = f.run("c", &["pull", "../src", "main:main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "Already up to date.\n",
            format!(
                "From ../src\n   {}..{}  main       -> main\n\
                 warning: fetch updated the current branch head.\n\
                 fast-forwarding your working tree from\n\
                 commit {}.\n",
                &old[..7],
                new.trim(),
                old.trim()
            )
            .as_str(),
            0
        )
    );
    assert_eq!(f.run("c", &["status", "--short"]).0, "");
    assert_eq!(std::fs::read_to_string(f.root.join("c/b")).unwrap(), "b\n");
}

#[test]
fn a_refused_fast_forward_dies_with_the_recovery_recipe() {
    let f = Fixture::new("refused");
    f.run(".", &["clone", "-q", "src", "c"]);
    let old = f.run("c", &["rev-parse", "HEAD"]).0;
    let old = old.trim();
    f.commit("c");
    std::fs::write(f.root.join("c/c"), "untracked\n").unwrap();
    let (_, err, code) = f.run("c", &["pull", "-q", "../src", "main:main"]);
    assert_eq!(
        (err.as_str(), code),
        (
            format!(
                "warning: fetch updated the current branch head.\n\
                 fast-forwarding your working tree from\n\
                 commit {old}.\n\
                 error: The following untracked working tree files would be overwritten by merge:\n\
                 \tc\n\
                 Please move or remove them before you merge.\n\
                 Aborting\n\
                 fatal: Cannot fast-forward your working tree.\n\
                 After making sure that you saved anything precious from\n\
                 $ git diff {old}\n\
                 output, run\n\
                 $ git reset --hard\n\
                 to recover.\n"
            )
            .as_str(),
            128
        )
    );
    assert_eq!(std::fs::read_to_string(f.root.join("c/c")).unwrap(), "untracked\n");
}
