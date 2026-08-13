//! A worktree directory the user cannot open must not stop a traversal.
//!
//! `open_cached_dir()` (dir.c:2585-2593) warns when `opendir()` fails and hands
//! `read_directory_recursive()` a `-1`, which it reads as "this directory has no
//! entries" — so stock git prints
//!
//! ```text
//! warning: could not open directory 'blocked/': Permission denied
//! ```
//!
//! on stderr, reports every other path as usual, and exits 0. Every command that
//! walks the worktree goes through that one function, which is why `status`,
//! `ls-files -o`, `clean -nd` and `add -n` are all pinned here.
//!
//! Unix-only, and skipped when running as root: uid 0 opens a `chmod 000`
//! directory just fine, so there is nothing to observe.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The tree, with the blocked directory's bits restored so cleanup can remove it.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ =
            std::fs::set_permissions(self.repo.join("blocked"), PermissionsExt::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn running_as_root() -> bool {
    // SAFETY: `getuid` is always safe; it reads a process property.
    unsafe { libc::getuid() == 0 }
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

/// A repository with one readable untracked file, one readable untracked
/// subdirectory, and one directory that cannot be opened at all.
fn fixture(tag: &str) -> Fixture {
    let root =
        std::env::temp_dir().join(format!("zvcs-unreadable-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));

    let init = run(&repo, &home, &["init", "-q", "-b", "main"]);
    assert!(init.status.success(), "init failed: {init:?}");
    std::fs::write(repo.join("a.txt"), "hi\n").unwrap();
    std::fs::create_dir(repo.join("sub")).unwrap();
    std::fs::write(repo.join("sub/s.txt"), "s\n").unwrap();
    std::fs::create_dir(repo.join("blocked")).unwrap();
    std::fs::write(repo.join("blocked/x"), "x\n").unwrap();
    std::fs::set_permissions(repo.join("blocked"), PermissionsExt::from_mode(0o000)).unwrap();

    Fixture { root, repo, home }
}

/// git's warning, verbatim: the path is worktree-relative and keeps its trailing
/// slash, and the reason is bare `strerror` text with no Rust `(os error N)` tail.
const WARNING: &str = "warning: could not open directory 'blocked/': Permission denied\n";

#[test]
fn worktree_walks_warn_and_continue_over_an_unreadable_directory() {
    if running_as_root() {
        eprintln!("skipped: root can open a `chmod 000` directory");
        return;
    }
    let fx = fixture("walks");

    // Every walker reports the same warning on stderr, exits 0, and still lists
    // everything outside the unreadable directory.
    for (args, want_stdout) in [
        (&["status", "--porcelain"][..], "?? a.txt\n?? sub/\n"),
        (&["status", "-s"][..], "?? a.txt\n?? sub/\n"),
        (&["ls-files", "-o"][..], "a.txt\nsub/s.txt\n"),
        (&["add", "-n", "."][..], "add 'a.txt'\nadd 'sub/s.txt'\n"),
    ] {
        let o = run(&fx.repo, &fx.home, args);
        assert_eq!(o.status.code(), Some(0), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), WARNING, "git {args:?} stderr");
        assert_eq!(String::from_utf8_lossy(&o.stdout), want_stdout, "git {args:?} stdout");
    }

    // The long-form status keeps its own exit code and body too — the unreadable
    // directory simply contributes nothing.
    let o = run(&fx.repo, &fx.home, &["status"]);
    assert_eq!(o.status.code(), Some(0), "status exit: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), WARNING);
    let body = String::from_utf8_lossy(&o.stdout);
    assert!(body.contains("\ta.txt\n"), "{body}");
    assert!(body.contains("\tsub/\n"), "{body}");
    assert!(!body.contains("blocked"), "unreadable directory listed as untracked:\n{body}");

    // `clean -nd` is the exception that still names it: an empty directory is
    // removable, and one that cannot be read looks empty.
    let o = run(&fx.repo, &fx.home, &["clean", "-nd"]);
    assert_eq!(o.status.code(), Some(0), "clean exit: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), WARNING);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "Would remove a.txt\nWould remove blocked/\nWould remove sub/\n"
    );
}

#[test]
fn the_warning_names_the_path_relative_to_the_worktree_root() {
    if running_as_root() {
        eprintln!("skipped: root can open a `chmod 000` directory");
        return;
    }
    let fx = fixture("prefix");

    // Run from a subdirectory: git's warning path is anchored at the worktree root,
    // not at the current directory — as are the porcelain paths beside it.
    let o = run(&fx.repo.join("sub"), &fx.home, &["status", "--porcelain"]);
    assert_eq!(o.status.code(), Some(0), "status exit: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), WARNING);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "?? a.txt\n?? sub/\n");
}
