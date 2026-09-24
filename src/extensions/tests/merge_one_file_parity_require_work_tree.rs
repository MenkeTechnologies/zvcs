//! `git merge-one-file`'s `require_work_tree` after `cd_to_toplevel`.
//!
//! The script sources `git-sh-setup`, runs `cd_to_toplevel` and then
//! `require_work_tree` (git-merge-one-file.sh:25-27), which asks a *fresh*
//! `git rev-parse --is-inside-work-tree` from the directory it just entered
//! (git-sh-setup.sh:186-191). `git --work-tree=src` exports the relative
//! `GIT_WORK_TREE=src` untouched, so the second question resolves `src` against
//! `src` itself, names nothing, and the script dies with exit 1 before its
//! argument count is looked at — no usage block on stdout.
//!
//! Expectations measured from stock git 2.55.0 under the same pinned
//! environment; only the exec-path prefix of `$0` differs by installation.
#![cfg(unix)]

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
            .join(format!("zvcs-merge-one-file-rwt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("src/lib.rs"), "fn main() {}\n").unwrap();
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "init"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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

/// A relative `--work-tree` is re-resolved after `cd_to_toplevel`, so the work
/// tree check fails even though the directory was entered.
#[test]
fn relative_work_tree_fails_require_work_tree_before_the_argument_count() {
    let f = Fixture::new("rel");
    let (out, err, code) = f.run(&["--work-tree=src", "merge-one-file", "--", "040000", "100644"]);
    assert_eq!((out.as_str(), code), ("", 1), "{err}");
    assert!(err.starts_with("fatal: "), "{err:?}");
    assert!(
        err.ends_with("/git-merge-one-file cannot be used without a working tree.\n"),
        "{err:?}"
    );
    assert_eq!(err.lines().count(), 1, "{err:?}");
}

/// With an ordinary work tree the same call still reaches the argument count:
/// the doubled usage block on stdout, exit 1.
#[test]
fn ordinary_work_tree_still_reports_the_wrong_argument_count() {
    let f = Fixture::new("ok");
    let (out, err, code) = f.run(&["merge-one-file", "--", "040000", "100644"]);
    assert_eq!((err.as_str(), code), ("", 1));
    assert!(out.ends_with("Blob ids and modes should be empty for missing files.\n"), "{out:?}");
    assert_eq!(out.matches("usage: git merge-one-file").count(), 2, "{out:?}");
}
