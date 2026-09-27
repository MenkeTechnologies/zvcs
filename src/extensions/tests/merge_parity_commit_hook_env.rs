//! `git merge`'s commit hooks get what `run_commit_hook()` exports.
//!
//! `prepare_to_commit()` (builtin/merge.c:923-977) runs `pre-merge-commit`,
//! `prepare-commit-msg` and `commit-msg` through `run_commit_hook(0 < option_edit,
//! repo_get_index_file(), …)` (commit.c:1994-2016), which pushes
//! `GIT_INDEX_FILE=<index_file>` and, when no editor will run, `GIT_EDITOR=:`.
//! The message file is `git_path_merge_msg()`, spelled on the git directory setup
//! left — `.git/MERGE_MSG` from any subdirectory.
//!
//! zvcs ran them through the plain hook runner: no `GIT_INDEX_FILE`, the caller's
//! `GIT_EDITOR`, and `../.git/MERGE_MSG` from a subdirectory.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `<hook> <args>|<GIT_INDEX_FILE>|<GIT_EDITOR>` on stderr.
const HOOK: &str = "#!/bin/sh\necho \"${0##*/} $*|${GIT_INDEX_FILE-unset}|${GIT_EDITOR-unset}\" >&2\n";

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
        let root = std::env::temp_dir().join(format!("zvcs-merge-commit-hook-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        f.git(&[], &["add", "."]);
        f.git(&[], &["commit", "-q", "-m", "base"]);
        f.git(&[], &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.git(&[], &["add", "s"]);
        f.git(&[], &["commit", "-q", "-m", "side"]);
        f.git(&[], &["checkout", "-q", "main"]);
        std::fs::write(f.work.join("m"), "m\n").unwrap();
        f.git(&[], &["add", "m"]);
        f.git(&[], &["commit", "-q", "-m", "main"]);
        for hook in ["pre-merge-commit", "prepare-commit-msg", "commit-msg"] {
            let path = f.work.join(".git/hooks").join(hook);
            std::fs::write(&path, HOOK).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f
    }

    fn git(&self, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
        self.git_in(&self.work, env, args)
    }

    fn git_in(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_EDITOR")
            .env_remove("GIT_MERGE_AUTOEDIT")
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
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn no_edit_merge_hooks_see_the_index_and_the_no_op_editor() {
    let f = Fixture::new("no-edit");
    assert_eq!(
        f.git_in(&f.work.join("sub"), &[("GIT_EDITOR", "vi")], &["merge", "-q", "--no-edit", "side"]),
        (
            "pre-merge-commit |.git/index|:\n\
             prepare-commit-msg .git/MERGE_MSG merge|.git/index|:\n\
             commit-msg .git/MERGE_MSG|.git/index|:\n"
                .into(),
            0
        )
    );
}

#[test]
fn edited_merge_hooks_keep_the_callers_editor() {
    let f = Fixture::new("edit");
    assert_eq!(
        f.git_in(&f.work.join("sub"), &[("GIT_EDITOR", "true")], &["merge", "-q", "--edit", "side"]),
        (
            "pre-merge-commit |.git/index|true\n\
             prepare-commit-msg .git/MERGE_MSG merge|.git/index|true\n\
             commit-msg .git/MERGE_MSG|.git/index|true\n"
                .into(),
            0
        )
    );
}
