//! `git receive-pack .` run from inside a repository's own directory.
//!
//! `enter_repo()` (path.c) accepts the relative operand `.` when the current
//! directory is a non-bare repository's `.git`, a bare repository, and refuses
//! it elsewhere. zvcs used to resolve `.` inside `.git` to a bogus work tree
//! (`./.git`) and answered "does not appear to be a git repository".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn fixture(tag: &str) -> Root {
    let root = std::env::temp_dir().join(format!("zvcs-receive-pack-dot-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main", "work"]);
    git(&root.join("work"), &["commit", "-q", "--allow-empty", "-m", "a"]);
    git(&root, &["clone", "-q", "--bare", "work", "bare.git"]);
    std::fs::create_dir(root.join("plain")).unwrap();
    Root(root)
}

/// Both spellings of the current directory, which stock resolves identically.
const DOTS: [&str; 2] = [".", "./"];

#[test]
fn dot_inside_dot_git_of_non_bare_repo() {
    let r = fixture("dotgit");
    for dot in DOTS {
        let out = git(&r.0.join("work/.git"), &["receive-pack", dot]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(String::from_utf8_lossy(&out.stdout).contains(" refs/heads/main\0"), "{dot}: {stderr}");
        assert!(!stderr.contains("does not appear"), "{dot}: {stderr}");
    }
}

#[test]
fn dot_inside_bare_repo() {
    let r = fixture("bare");
    for dot in DOTS {
        let out = git(&r.0.join("bare.git"), &["receive-pack", dot]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(String::from_utf8_lossy(&out.stdout).contains(" refs/heads/main\0"), "{dot}: {stderr}");
        assert!(!stderr.contains("does not appear"), "{dot}: {stderr}");
    }
}

#[test]
fn dot_in_non_repository() {
    let r = fixture("plain");
    let out = git(&r.0.join("plain"), &["receive-pack", "."]);
    assert_eq!(out.status.code(), Some(128));
    assert!(out.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&out.stderr), "fatal: '.' does not appear to be a git repository\n");
}
