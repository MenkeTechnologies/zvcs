//! A directory the index knows about is never collapsed into `?? <dir>/`.
//!
//! `treat_directory()` asks `directory_exists_in_index()` first: as soon as any index
//! entry lives *below* the directory it returns `index_directory` and the walk
//! recurses, whatever the worktree looks like. The entries need not be present on
//! disk — deleting the only tracked file, marking it `skip-worktree`, or leaving it
//! outside a sparse checkout all keep the directory expanded, so its untracked
//! siblings are reported one by one.
//!
//! This matters beyond the listing: `git clean -f` removes files but never
//! directories, so a collapsed `d/` gives it nothing to remove, and `git clean -fd`
//! removes the directory git would have kept.
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
    /// `d/tracked.txt` committed, then removed from the worktree, with an untracked
    /// `d/untracked.txt` left beside it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idxdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("d/tracked.txt", b"t\n");
        f.write("root.txt", b"r\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// The worktree file is gone but its index entry is not.
    fn delete_tracked_and_add_untracked(&self) {
        std::fs::remove_file(self.work.join("d/tracked.txt")).unwrap();
        self.write("d/untracked.txt", b"u\n");
    }
}

/// The untracked sibling is named in full, and the deletion is still reported.
#[test]
fn status_names_the_untracked_file_rather_than_the_directory() {
    let f = Fixture::new("status");
    f.delete_tracked_and_add_untracked();
    assert_eq!(
        f.stdout(&["status", "--porcelain"]),
        " D d/tracked.txt\n?? d/untracked.txt\n"
    );
    assert!(
        f.stdout(&["status", "--porcelain=v2"]).ends_with("? d/untracked.txt\n"),
        "v2 collapses too"
    );
}

/// A directory with no index entry below it still collapses — that is the rule this
/// fix must not break.
#[test]
fn a_directory_the_index_never_heard_of_still_collapses() {
    let f = Fixture::new("plain");
    std::fs::create_dir_all(f.work.join("fresh")).unwrap();
    f.write("fresh/x.txt", b"x\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "?? fresh/\n");
}

/// `git clean -f` removes files only: it has to see the file to remove it, and it
/// must not be handed the directory instead.
#[test]
fn clean_removes_the_file_and_keeps_the_directory() {
    let f = Fixture::new("clean");
    f.delete_tracked_and_add_untracked();
    assert_eq!(f.stdout(&["clean", "-n"]), "Would remove d/untracked.txt\n");

    f.git(&["clean", "-f"]);
    assert!(!f.work.join("d/untracked.txt").exists(), "the file must be gone");
    assert!(f.work.join("d").is_dir(), "the directory must survive");
}

/// `-d` does not change which paths are reported, only whether directories may be
/// removed — and this directory is not one of them.
#[test]
fn clean_with_directories_still_spares_an_indexed_directory() {
    let f = Fixture::new("cleand");
    f.delete_tracked_and_add_untracked();
    assert_eq!(f.stdout(&["clean", "-nd"]), "Would remove d/untracked.txt\n");

    f.git(&["clean", "-fd"]);
    assert!(f.work.join("d").is_dir(), "the directory must survive");
}

/// `skip-worktree` removes the file from the worktree without removing the index
/// entry, which is the shape a sparse checkout leaves behind.
#[test]
fn a_skip_worktree_entry_keeps_its_directory_expanded() {
    let f = Fixture::new("skip");
    f.git(&["update-index", "--skip-worktree", "d/tracked.txt"]);
    std::fs::remove_file(f.work.join("d/tracked.txt")).unwrap();
    f.write("d/untracked.txt", b"u\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "?? d/untracked.txt\n");
    assert_eq!(f.stdout(&["clean", "-n"]), "Would remove d/untracked.txt\n");
}
