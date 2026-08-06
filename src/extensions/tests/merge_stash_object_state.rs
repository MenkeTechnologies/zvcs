//! What `merge` and `stash` leave in the object database and in `logs/HEAD`.
//!
//! `save_state()` (builtin/merge.c) snapshots a dirty worktree into a `git stash create`
//! commit before a strategy runs, so `restore_state()` can rewind to it — the snapshot
//! stays in the object database as a dangling commit either way. `stash push -S`, by
//! contrast, reverses the staged patch over the worktree files with `git apply -R` and
//! hashes nothing, and a pathspec-limited push runs no `reset --hard`, so it writes no
//! `HEAD` reflog line.
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
        let root = std::env::temp_dir().join(format!("zvcs-mrgobj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// Every object in the database, as `<type> <size>` lines.
    fn objects(&self) -> Vec<String> {
        let out = self.run(&[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objecttype) %(objectsize)",
        ]);
        let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        v.sort();
        v
    }

    fn head_reflog(&self) -> String {
        std::fs::read_to_string(self.work.join(".git/logs/HEAD")).unwrap_or_default()
    }

    /// Two diverged branches over a shared base, left on `main`.
    fn diverge(&self) {
        self.write("a.txt", "base\n");
        self.write("mine.txt", "mine\n");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "seed"]);
        self.git(&["checkout", "-q", "-b", "side"]);
        self.write("a.txt", "base\nside\n");
        self.git(&["commit", "-qam", "side"]);
        self.git(&["checkout", "-q", "main"]);
        self.write("b.txt", "main\n");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "main2"]);
    }
}

/// A merge over a dirty worktree parks a `stash create` snapshot in the object
/// database; a merge over a clean one has nothing to snapshot.
#[test]
fn a_real_merge_snapshots_a_dirty_worktree() {
    let dirty = Fixture::new("dirty");
    dirty.diverge();
    dirty.write("mine.txt", "mine\nlocal edit\n");
    let before = dirty.objects().len();
    dirty.git(&["merge", "side", "-m", "merge"]);
    let after = dirty.objects();
    // The merge itself adds a commit and a tree; the snapshot adds its own commit,
    // tree and the blob holding the local edit.
    assert!(
        after.len() >= before + 5,
        "the snapshot is missing: {before} -> {}",
        after.len()
    );
    assert!(
        after.iter().filter(|l| l.starts_with("commit ")).count() >= 5,
        "expected the snapshot commit alongside the merge: {after:?}"
    );

    let clean = Fixture::new("clean");
    clean.diverge();
    let before = clean.objects().len();
    clean.git(&["merge", "side", "-m", "merge"]);
    assert_eq!(
        clean.objects().len(),
        before + 2,
        "a clean merge writes only its commit and tree"
    );
}

/// A strategy refused because the index does not match HEAD logs the no-op HEAD
/// update; one refused over unstaged worktree changes does not.
#[test]
fn only_the_index_refusal_logs_updating_head() {
    let staged = Fixture::new("staged");
    staged.diverge();
    staged.write("s.txt", "staged\n");
    staged.git(&["add", "s.txt"]);
    let out = staged.run(&["merge", "side", "-m", "x"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        staged.head_reflog().trim_end().ends_with("merge side: updating HEAD"),
        "{}",
        staged.head_reflog()
    );

    let dirty = Fixture::new("clobber");
    dirty.diverge();
    dirty.write("a.txt", "locally rewritten\n");
    let before = dirty.head_reflog();
    let out = dirty.run(&["merge", "side", "-m", "x"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(dirty.head_reflog(), before, "no HEAD line for a clobber refusal");
}

/// `stash push -S` hashes nothing of the worktree, and a pathspec-limited push runs
/// no `reset --hard`, so neither leaves a trace beyond the stash itself.
#[test]
fn staged_and_pathspec_pushes_leave_no_extra_trace() {
    let f = Fixture::new("push");
    f.write("counter.txt", "1\n");
    f.write("notes.txt", "notes\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    f.write("notes.txt", "notes\nstaged\n");
    f.git(&["add", "notes.txt"]);
    f.write("counter.txt", "1\nunstaged\n");

    let before = f.objects().len();
    f.git(&["stash", "push", "-S"]);
    // The stash itself is a `W` commit, an `I` commit and their trees; the worktree's
    // own content is never hashed.
    let after = f.objects();
    assert!(
        after.len() <= before + 5,
        "the worktree was materialised: {before} -> {}\n{after:?}",
        after.len()
    );
    assert!(
        !after.iter().any(|l| l == "blob 12"),
        "the unstaged counter.txt was hashed: {after:?}"
    );

    let p = Fixture::new("pathspec");
    p.write("counter.txt", "1\n");
    p.git(&["add", "-A"]);
    p.git(&["commit", "-q", "-m", "seed"]);
    p.write("counter.txt", "1\nedit\n");
    let before = p.head_reflog();
    p.git(&["stash", "push", "--", "counter.txt"]);
    assert_eq!(p.head_reflog(), before, "a pathspec push runs no reset --hard");

    // Without a pathspec the reset does run, and logs.
    let q = Fixture::new("plain");
    q.write("counter.txt", "1\n");
    q.git(&["add", "-A"]);
    q.git(&["commit", "-q", "-m", "seed"]);
    q.write("counter.txt", "1\nedit\n");
    q.git(&["stash", "push"]);
    assert!(
        q.head_reflog().trim_end().ends_with("reset: moving to HEAD"),
        "{}",
        q.head_reflog()
    );
}

/// `-u` writes the untracked parent whenever it is asked for — with the empty tree
/// when the pathspec left nothing to collect.
#[test]
fn untracked_parent_is_always_written_under_u() {
    let f = Fixture::new("untracked");
    f.write("counter.txt", "1\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    f.write("counter.txt", "1\nedit\n");
    f.write("fresh.txt", "untracked\n");

    f.git(&["stash", "push", "-u", "--", "counter.txt"]);
    let parents = String::from_utf8_lossy(&f.run(&["log", "-1", "--format=%P", "refs/stash"]).stdout)
        .trim_end()
        .to_owned();
    assert_eq!(parents.split(' ').count(), 3, "third parent missing: {parents}");
    let tree = String::from_utf8_lossy(&f.run(&["log", "-1", "--format=%T", "refs/stash^3"]).stdout)
        .trim_end()
        .to_owned();
    assert_eq!(
        tree, "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        "the pathspec matched no untracked file, so the tree is empty"
    );
    // The untracked file the pathspec did not name stays in the worktree.
    assert!(f.work.join("fresh.txt").exists());
}
