//! `am` under an explicit, relative `--work-tree` / `GIT_WORK_TREE` / `GIT_DIR`.
//!
//! git runs `am`'s apply and commit in-process after `setup_git_directory()`
//! has moved to the top of the work tree: `setup_explicit_git_dir()` resolves a
//! relative `GIT_WORK_TREE` against the directory the command was typed in and
//! makes the git directory absolute when it changes directory (setup.c), and
//! `run_apply()` initialises its `apply_state` with no prefix
//! (builtin/am.c:1500), so `cd sub && git --work-tree=.. am` patches the whole
//! tree and commits it.
//!
//! zvcs runs `apply`, `write-tree` and `commit-tree` as children from the work
//! tree root but let them inherit the user's relative values: `..` then named
//! the directory above the repository, `apply` skipped every path as outside it
//! and exited 0, and `am` recorded a commit carrying the *old* tree while
//! printing `Applying:`. With `GIT_DIR=../.git` as well, the child was `not a
//! git repository` and the patch failed.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    mbox: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `side` rewrites line 2 of `f`; `main` stays at the base, and the mailbox
    /// holds `side`'s patch.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-am-explicit-work-tree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let mbox = root.join("mbox");
        let f = Fixture { root, work, mbox };
        f.run(&f.work, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "1\n2\n3\n").unwrap();
        f.run(&f.work, &[], &["add", "f"]);
        f.run(&f.work, &[], &["commit", "-q", "-m", "base"]);
        f.run(&f.work, &[], &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("f"), "1\ntwo\n3\n").unwrap();
        f.run(&f.work, &[], &["commit", "-q", "-am", "change two"]);
        let (patch, _, _) = f.run(&f.work, &[], &["format-patch", "-1", "--stdout"]);
        std::fs::write(&f.mbox, patch).unwrap();
        f.run(&f.work, &[], &["checkout", "-q", "main"]);
        f
    }

    fn run(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .envs(env.iter().copied())
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

    /// The applied commit carries `side`'s tree, and nothing is left staged or
    /// modified.
    fn assert_applied(&self) {
        let top = |args: &[&str]| self.run(&self.work, &[], args).0;
        assert_eq!(top(&["rev-parse", "HEAD^{tree}"]), top(&["rev-parse", "side^{tree}"]));
        assert_eq!(top(&["log", "-1", "--format=%s"]), "change two\n");
        assert_eq!(top(&["status", "--porcelain"]), "");
        assert_eq!(std::fs::read_to_string(self.work.join("f")).unwrap(), "1\ntwo\n3\n");
    }
}

#[test]
fn a_relative_work_tree_option_from_a_subdirectory_commits_the_patched_tree() {
    for threeway in [false, true] {
        let f = Fixture::new(if threeway { "opt-3way" } else { "opt" });
        let mut args = vec!["--work-tree=..", "am"];
        if threeway {
            args.push("-3");
        }
        args.push(f.mbox.to_str().unwrap());
        let (out, err, code) = f.run(&f.work.join("sub"), &[], &args);
        assert_eq!((out.as_str(), err.as_str(), code), ("Applying: change two\n", "", 0));
        f.assert_applied();
    }
}

#[test]
fn relative_git_dir_and_work_tree_variables_from_a_subdirectory() {
    let f = Fixture::new("env");
    let env = [("GIT_DIR", "../.git"), ("GIT_WORK_TREE", "..")];
    let (out, err, code) = f.run(&f.work.join("sub"), &env, &["am", f.mbox.to_str().unwrap()]);
    assert_eq!((out.as_str(), err.as_str(), code), ("Applying: change two\n", "", 0));
    f.assert_applied();
}
