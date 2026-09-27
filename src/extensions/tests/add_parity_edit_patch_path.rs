//! `git add -e` names `ADD_EDIT.patch` the way `repo_git_path()` spells it.
//!
//! `edit_patch()` takes `file = repo_git_path(repo, "ADD_EDIT.patch")`
//! (builtin/add.c:305-349) — `<gitdir>/ADD_EDIT.patch` on the git directory as
//! setup left it, so `.git/ADD_EDIT.patch` once git has moved to the top of the
//! work tree — and runs `git apply --recount --cached <file>` as a `git_cmd`
//! child with `p.dir` unset, dying with `could not apply '<file>'`.
//!
//! zvcs spelled the file from the directory the command was typed in:
//! `fatal: could not apply '../.git/ADD_EDIT.patch'`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
        let root = std::env::temp_dir().join(format!("zvcs-add-edit-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        let f = Fixture { root, work };
        f.git(&f.work, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        f.git(&f.work, &[], &["add", "."]);
        f.git(&f.work, &[], &["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("sub/f"), "a\nb\n").unwrap();
        f
    }

    fn git(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
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
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).replace(self.root.to_str().unwrap(), "<R>"),
            out.status.code().expect("no signal"),
        )
    }
}

/// An editor that replaces the patch with something `git apply` rejects.
const GARBAGE: &str = "f() { echo garbage >\"$1\"; }; f";

#[test]
fn a_rejected_patch_is_named_from_the_top_of_the_work_tree() {
    let f = Fixture::new("subdir");
    let (_, err, code) = f.git(&f.work.join("sub"), &[("GIT_EDITOR", GARBAGE)], &["add", "-e"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "error: No valid patches in input (allow with \"--allow-empty\")\n\
             fatal: could not apply '.git/ADD_EDIT.patch'\n",
            128
        )
    );
}

/// `setup_work_tree()` moves to the work tree and `reparent_relative_path()`
/// (chdir-notify.c:100-115) keeps `../work/.git` reachable from there as
/// `<old-cwd>/../work/.git`.
#[test]
fn an_explicit_git_dir_is_reparented_when_setup_moves_to_the_work_tree() {
    let f = Fixture::new("explicit");
    let (_, err, code) = f.git(
        &f.root.join("out"),
        &[("GIT_EDITOR", GARBAGE)],
        &["--git-dir=../work/.git", "--work-tree=../work", "add", "-e"],
    );
    assert_eq!(
        (err.as_str(), code),
        (
            "error: No valid patches in input (allow with \"--allow-empty\")\n\
             fatal: could not apply '<R>/out/../work/.git/ADD_EDIT.patch'\n",
            128
        )
    );
}

#[test]
fn an_untouched_patch_from_a_subdirectory_is_applied() {
    let f = Fixture::new("apply");
    let (_, err, code) = f.git(&f.work.join("sub"), &[("GIT_EDITOR", ":")], &["add", "-e"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(f.git(&f.work, &[], &["diff", "--cached", "--name-only"]).0, "sub/f\n");
    assert!(!f.work.join(".git/ADD_EDIT.patch").exists());
}
