//! `worktree remove` / `worktree move`, and the hook template set every repository
//! starts with.
//!
//! Both subcommands resolve their argument through the same lookup `lock`/`unlock`
//! use, refuse the main worktree, and refuse a locked one without `--force`. `remove`
//! additionally refuses a checkout with modified or untracked files, and takes the
//! administrative directory down with the checkout. `move` rewrites both halves of the
//! link — `worktrees/<id>/gitdir` and the checkout's own `.git` file.
//!
//! Expectations measured against stock git 2.55.0.
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
        let root = std::env::temp_dir().join(format!("zvcs-wtrm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), "a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }
}

/// A name that is not a worktree is git's `fatal: '<arg>' is not a working tree`, and
/// the main worktree cannot be removed at all.
#[test]
fn removal_refuses_what_it_cannot_remove() {
    let f = Fixture::new("refuse");
    let out = f.run(&["worktree", "remove", "nosuch"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: 'nosuch' is not a working tree\n"
    );

    let out = f.run(&["worktree", "move", "nosuch", "elsewhere"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: 'nosuch' is not a working tree\n"
    );

    let out = f.run(&["worktree", "remove", "."]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is a main working tree"),
        "{out:?}"
    );
}

/// The checkout and its administrative directory both go, and a dirty one needs
/// `--force`.
#[test]
fn removal_takes_the_admin_directory_with_it() {
    let f = Fixture::new("remove");
    f.git(&["worktree", "add", "-q", "wt2", "-b", "side"]);
    let admin = f.work.join(".git/worktrees/wt2");
    assert!(admin.is_dir());

    std::fs::write(f.work.join("wt2/f.txt"), "dirty\n").unwrap();
    let out = f.run(&["worktree", "remove", "wt2"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: 'wt2' contains modified or untracked files, use --force to delete it\n"
    );
    assert!(f.work.join("wt2").is_dir(), "nothing was removed");

    let out = f.run(&["worktree", "remove", "--force", "wt2"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(!f.work.join("wt2").exists());
    assert!(!admin.exists(), "the administrative directory survived");

    // An untracked file counts too, not just a tracked modification.
    f.git(&["worktree", "add", "-q", "wt3", "-b", "third"]);
    std::fs::write(f.work.join("wt3/new.txt"), "untracked\n").unwrap();
    let out = f.run(&["worktree", "remove", "wt3"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("modified or untracked files"));
}

/// `move` renames the checkout and repoints both halves of the link at each other.
#[test]
fn move_rewrites_both_halves_of_the_link() {
    let f = Fixture::new("move");
    f.git(&["worktree", "add", "-q", "wt2", "-b", "side"]);

    let out = f.run(&["worktree", "move", "wt2", "moved"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(!f.work.join("wt2").exists());
    assert!(f.work.join("moved/f.txt").is_file());

    let gitdir = std::fs::read_to_string(f.work.join(".git/worktrees/wt2/gitdir")).unwrap();
    assert!(
        gitdir.trim_end().ends_with("/moved/.git"),
        "gitdir still points at the old path: {gitdir}"
    );
    let dot_git = std::fs::read_to_string(f.work.join("moved/.git")).unwrap();
    assert!(
        dot_git.starts_with("gitdir: /") && dot_git.trim_end().ends_with("/.git/worktrees/wt2"),
        "the checkout's .git file is not absolute: {dot_git}"
    );
    // The listing agrees, which is the whole point of rewriting both files.
    let list = String::from_utf8_lossy(&f.run(&["worktree", "list"]).stdout).into_owned();
    assert!(list.contains("/moved "), "{list}");
    assert!(!list.contains("/wt2 "), "{list}");
}

/// A new repository ships git's hook template set — the names are what a clone
/// carries over, so an extra or missing sample shows up in every cloned tree.
#[test]
fn init_writes_gits_hook_sample_set() {
    let f = Fixture::new("hooks");
    let mut names: Vec<String> = std::fs::read_dir(f.work.join(".git/hooks"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "applypatch-msg.sample",
            "commit-msg.sample",
            "fsmonitor-watchman.sample",
            "post-update.sample",
            "pre-applypatch.sample",
            "pre-commit.sample",
            "pre-merge-commit.sample",
            "pre-push.sample",
            "pre-rebase.sample",
            "pre-receive.sample",
            "prepare-commit-msg.sample",
            "push-to-checkout.sample",
            "sendemail-validate.sample",
            "update.sample",
        ]
    );
}
