//! Editors and hooks see the setup git performed for a gitfile, a linked
//! worktree and a bare repository.
//!
//! * A discovered `.git` file hands `setup_discovered_git_dir()` the path
//!   `read_gitfile_gently()` resolved (setup.c:956-1035, :1599-1600), which
//!   `strcmp(gitdir, ".git")` exports (setup.c:1240-1241); git stands at the top
//!   of the work tree.
//! * A `$GIT_DIR` naming a gitfile is replaced by its target in
//!   `setup_explicit_git_dir()` (setup.c:1121-1125) before `set_git_dir()`
//!   exports it.
//! * `setup_bare_git_dir()` (setup.c:1252-1281) leaves the cwd alone and
//!   exports `.` at the repository, or the path the walk reached from below it.
//!
//! Children run with `p.dir` unset (editor.c:60-139, hook.c) and inherit that.
//! zvcs had no model for these setups: the editor ran in the directory the
//! command was typed in, without `GIT_DIR` (or with the gitfile's path).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `<file>|<GIT_DIR>|<GIT_PREFIX>|<cwd>` on stderr; writes a message.
const EDITOR: &str = "f() { echo \"$1|$GIT_DIR|$GIT_PREFIX|$(pwd)\" >&2; echo msg >\"$1\"; }; f";

/// `hook:<$0>|<GIT_DIR>|<cwd>` on stderr.
const HOOK: &str = "#!/bin/sh\necho \"hook:$0|$GIT_DIR|$(pwd)\" >&2\n";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `wt` with its git directory at `gd`, a linked worktree `lwt`, and a bare
    /// clone `bare.git`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-setup-gitfile-bare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        let r = f.root.clone();
        f.git(&r, &[], &["init", "-q", "-b", "main", "--separate-git-dir=gd", "wt"]);
        std::fs::create_dir_all(r.join("wt/sub")).unwrap();
        std::fs::write(r.join("wt/sub/f"), "a\n").unwrap();
        f.git(&r.join("wt"), &[], &["add", "."]);
        f.git(&r.join("wt"), &[], &["commit", "-q", "-m", "base"]);
        f.git(&r.join("wt"), &[], &["worktree", "add", "-q", "../lwt"]);
        std::fs::create_dir_all(r.join("lwt/sub")).unwrap();
        f.git(&r, &[], &["clone", "-q", "--bare", "wt", "bare.git"]);
        std::fs::create_dir_all(r.join("bare.git/refs/x")).unwrap();
        f
    }

    fn git(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
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
            .env("GIT_EDITOR", EDITOR)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr).replace(self.root.to_str().unwrap(), "<R>"),
            out.status.code().expect("no signal"),
        )
    }

    fn install_pre_commit(&self) {
        use std::os::unix::fs::PermissionsExt;
        let hook = self.root.join("gd/hooks/pre-commit");
        std::fs::write(&hook, HOOK).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn a_separate_git_dir_is_exported_and_the_editor_starts_at_the_top() {
    let f = Fixture::new("separate");
    f.install_pre_commit();
    assert_eq!(
        f.git(&f.root.join("wt/sub"), &[], &["commit", "-q", "--allow-empty"]),
        (
            "hook:<R>/gd/hooks/pre-commit|<R>/gd|<R>/wt\n\
             <R>/gd/COMMIT_EDITMSG|<R>/gd|sub/|<R>/wt\n"
                .into(),
            0
        )
    );
}

#[test]
fn a_linked_worktree_exports_its_own_git_dir_and_shares_the_hooks() {
    let f = Fixture::new("linked");
    f.install_pre_commit();
    assert_eq!(
        f.git(&f.root.join("lwt/sub"), &[], &["commit", "-q", "--allow-empty"]),
        (
            "hook:<R>/gd/hooks/pre-commit|<R>/gd/worktrees/lwt|<R>/lwt\n\
             <R>/gd/worktrees/lwt/COMMIT_EDITMSG|<R>/gd/worktrees/lwt|sub/|<R>/lwt\n"
                .into(),
            0
        )
    );
}

#[test]
fn a_git_dir_naming_a_gitfile_exports_its_target() {
    let f = Fixture::new("explicit");
    assert_eq!(
        f.git(&f.root.join("wt/sub"), &[("GIT_DIR", "../.git")], &["commit", "-q", "--allow-empty"]),
        ("<R>/gd/COMMIT_EDITMSG|<R>/gd||<R>/wt/sub\n".into(), 0)
    );
}

#[test]
fn a_bare_repository_leaves_the_cwd_and_exports_where_the_walk_stopped() {
    let f = Fixture::new("bare");
    assert_eq!(
        f.git(&f.root.join("bare.git/refs/x"), &[], &["tag", "-a", "t1"]),
        ("<R>/bare.git/TAG_EDITMSG|<R>/bare.git||<R>/bare.git/refs/x\n".into(), 0)
    );
    assert_eq!(
        f.git(&f.root.join("bare.git"), &[], &["tag", "-a", "t2"]),
        ("<R>/bare.git/TAG_EDITMSG|.||<R>/bare.git\n".into(), 0)
    );
    assert_eq!(
        f.git(&f.root, &[("GIT_DIR", "bare.git")], &["tag", "-a", "t3"]),
        ("<R>/bare.git/TAG_EDITMSG|bare.git||<R>\n".into(), 0)
    );
}
