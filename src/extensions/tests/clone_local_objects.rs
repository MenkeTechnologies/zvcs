//! What a local clone leaves on disk, and the reflog a fetch writes.
//!
//! `clone_local()` adopts the source's object store — hardlinking each file where the
//! filesystem allows — instead of packing and unpacking it, so a clone of a repository
//! with loose objects has those same loose objects. The remote-tracking refs it writes
//! go through `initial_ref_transaction_commit()`, which records no reflog for them.
//! A fetch's reflog lines carry `fetch` plus the command line, and a new ref is
//! "storing head" unless it is a tag.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
    /// A source repository with two branches and a tag, all loose objects.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clonelocal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        f.git(&src, &["init", "-q", "-b", "main", "."]);
        f.git(&src, &["config", "user.email", "t@e.co"]);
        f.git(&src, &["config", "user.name", "t"]);
        std::fs::write(src.join("f.txt"), "a\n").unwrap();
        f.git(&src, &["add", "-A"]);
        f.git(&src, &["commit", "-q", "-m", "seed"]);
        f.git(&src, &["branch", "feature"]);
        f.git(&src, &["tag", "v1"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        self.cmd(dir, args).output().unwrap()
    }

    /// Every file below `<git-dir>/objects`, as `/`-joined relative paths.
    fn object_files(&self, git_dir: &Path) -> Vec<String> {
        let objects = git_dir.join("objects");
        let mut out = Vec::new();
        let mut stack = vec![objects.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(rel) = path.strip_prefix(&objects) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }
}

/// A local clone ends up with the source's own object files, not a pack of them.
#[test]
fn a_local_clone_adopts_the_source_object_store() {
    let f = Fixture::new("adopt");
    let src = f.root.join("src");
    let out = f.run(&f.root, &["clone", "-q", "src", "copy"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    let source_objects = f.object_files(&src.join(".git"));
    assert!(
        source_objects.iter().all(|p| !p.starts_with("pack/")),
        "the fixture should have loose objects only: {source_objects:?}"
    );
    let clone_objects = f.object_files(&f.root.join("copy/.git"));
    assert_eq!(
        clone_objects, source_objects,
        "the clone holds a different object store than the source"
    );

    // The clone still works, which is the point of adopting rather than re-packing.
    let log = f.run(&f.root.join("copy"), &["log", "--oneline"]);
    assert_eq!(log.status.code(), Some(0), "{log:?}");
    assert!(String::from_utf8_lossy(&log.stdout).contains("seed"));
}

/// The remote-tracking branches a clone writes carry no reflog; `HEAD` and the
/// checked-out branch do.
#[test]
fn a_clone_logs_head_and_the_branch_only() {
    let f = Fixture::new("reflogs");
    let out = f.run(&f.root, &["clone", "-q", "src", "copy"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    let logs = f.root.join("copy/.git/logs");
    assert!(logs.join("HEAD").is_file());
    assert!(logs.join("refs/heads/main").is_file());
    assert!(
        !logs.join("refs/remotes/origin/main").exists(),
        "initial_ref_transaction_commit writes no reflog for a remote-tracking ref"
    );
    assert!(!logs.join("refs/remotes/origin/feature").exists());
}

/// A fetch's reflog line is `fetch <the command line>: storing head` for a new ref of
/// any kind but a tag.
#[test]
fn fetch_reflog_names_the_command_line() {
    let f = Fixture::new("fetchlog");
    let src = f.root.join("src");
    let dst = f.root.join("work");
    std::fs::create_dir_all(&dst).unwrap();
    f.git(&dst, &["init", "-q", "-b", "main", "."]);
    f.git(&dst, &["config", "user.email", "t@e.co"]);
    f.git(&dst, &["config", "user.name", "t"]);

    let src_arg = src.to_string_lossy().into_owned();
    let out = f.run(
        &dst,
        &["fetch", &src_arg, "refs/heads/feature:refs/remotes/self/feature"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let log = std::fs::read_to_string(dst.join(".git/logs/refs/remotes/self/feature")).unwrap();
    assert!(
        log.trim_end().ends_with(&format!(
            "fetch {src_arg} refs/heads/feature:refs/remotes/self/feature: storing head"
        )),
        "{log}"
    );
}

/// `--multiple` reads its arguments as remote names, so a path is refused before
/// anything is fetched.
#[test]
fn multiple_refuses_a_path() {
    let f = Fixture::new("multiple");
    let out = f.run(&f.root.join("src"), &["fetch", "--multiple", "."]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: no such remote or remote group: .\n"
    );
    assert!(out.stdout.is_empty(), "nothing was announced: {out:?}");
}
