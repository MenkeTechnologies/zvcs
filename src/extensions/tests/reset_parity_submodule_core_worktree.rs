//! `reset --hard --recurse-submodules` (and `-c submodule.recurse=true`) across a
//! commit that drops a submodule must leave the absorbed gitdir without the
//! `core.worktree` that pointed at the directory the reset just removed.
//!
//! `unlink_entry()` (entry.c) calls `submodule_move_head(path, "HEAD", NULL,
//! SUBMODULE_MOVE_HEAD_FORCE)`, whose "no new head" branch (submodule.c:2246-2256)
//! unlinks `sub/.git`, removes the empty directory and then runs
//! `submodule_unset_core_worktree()` (submodule.c:2059-2074), an in-process
//! `repo_config_set_in_file_gently(<gitdir>/config, "core.worktree", NULL)`.
//! zvcs spawned `git --git-dir=<gitdir> config --unset core.worktree` instead,
//! which set the gitdir up, chased the vanished work tree, died with
//! `fatal: cannot chdir to '../../../sub'` and left the key behind — after which
//! every command against `.git/modules/sub` died the same way.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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

fn git(dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
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

impl Fixture {
    /// A superproject whose `HEAD` adds submodule `sub` on top of a commit
    /// without it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reset-sub-wt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let upstream = root.join("upstream");
        let work = root.join("work");
        std::fs::create_dir_all(&upstream).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };

        f.ok(&upstream, &["init", "-q", "-b", "main"]);
        std::fs::write(upstream.join("mod.txt"), "submodule content\n").unwrap();
        f.ok(&upstream, &["add", "."]);
        f.ok(&upstream, &["commit", "-q", "-m", "submodule initial"]);

        f.ok(&f.work, &["init", "-q", "-b", "main"]);
        std::fs::write(f.work.join("r"), "r\n").unwrap();
        f.ok(&f.work, &["add", "."]);
        f.ok(&f.work, &["commit", "-q", "-m", "base"]);
        let url = upstream.to_str().unwrap();
        f.ok(&f.work, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", url, "sub"]);
        f.ok(&f.work, &["commit", "-q", "-m", "add submodule"]);
        f
    }

    fn ok(&self, dir: &Path, args: &[&str]) {
        let (_, err, code) = git(dir, &self.root, args);
        assert_eq!(code, 0, "git {args:?} failed: {err}");
    }

    fn sub_config(&self) -> String {
        std::fs::read_to_string(self.work.join(".git/modules/sub/config")).unwrap()
    }

    fn assert_submodule_detached(&self, args: &[&str]) {
        assert!(self.sub_config().contains("worktree = ../../../sub"), "fixture lacks core.worktree");

        let (out, err, code) = git(&self.work, &self.root, args);
        assert_eq!(code, 0);
        assert_eq!(out, "HEAD is now at e981f00 base\n");
        assert_eq!(err, "");

        assert!(!self.work.join("sub").exists(), "sub/ survived the reset");
        assert!(!self.sub_config().contains("\tworktree = "), "core.worktree left behind:\n{}", self.sub_config());

        // The absorbed gitdir is still a usable repository on its own.
        let (out, err, code) = git(&self.work, &self.root, &["--git-dir=.git/modules/sub", "cat-file", "-t", "HEAD"]);
        assert_eq!((out.as_str(), err.as_str(), code), ("commit\n", "", 0));
    }
}

#[test]
fn recurse_submodules_flag_unsets_core_worktree() {
    let f = Fixture::new("flag");
    f.assert_submodule_detached(&["reset", "--recurse-submodules", "--hard", "HEAD~1"]);
}

#[test]
fn submodule_recurse_config_unsets_core_worktree() {
    let f = Fixture::new("config");
    f.assert_submodule_detached(&["-c", "submodule.recurse=true", "reset", "--hard", "HEAD~1"]);
}
