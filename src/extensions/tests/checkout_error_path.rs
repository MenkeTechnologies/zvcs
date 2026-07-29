//! A checkout that cannot write a file must say *which* file.
//!
//! git always names it — `error: unable to unlink old 'f.txt': Operation not
//! permitted` — and without that a failure is nothing to act on. The per-entry
//! error carries the path, but `keep_going` is off for `reset`, so the error was
//! returned rather than collected into an `ErrorRecord` and the path was
//! dropped on the way out. What reached the terminal was a bare
//!
//! ```text
//! zvcs: reset: IO error while writing blob or reading file metadata or changing filetype: Operation not permitted (os error 1)
//! ```
//!
//! against a 9,000-file worktree.
//!
//! The failure itself is correct — git cannot check out over an unwritable file
//! either — so this pins the diagnostic, not a behavior change.
//!
//! Unix-only, and skipped when running as root: the whole setup rests on the
//! permission bits, which uid 0 ignores.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Restore the bits first, or the cleanup cannot remove the tree.
        let _ = std::fs::set_permissions(self.work.join("locked"), PermissionsExt::from_mode(0o700));
        let _ = std::fs::set_permissions(
            self.work.join("locked/f.txt"),
            PermissionsExt::from_mode(0o644),
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
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
}

fn running_as_root() -> bool {
    // SAFETY: `getuid` is always safe; it reads a process property.
    unsafe { libc::getuid() == 0 }
}

#[test]
fn checkout_failure_names_the_offending_path() {
    if running_as_root() {
        eprintln!("skipped: root ignores the permission bits this test relies on");
        return;
    }

    let root = std::env::temp_dir().join(format!("zvcs-ckerr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(work.join("locked")).unwrap();
    let f = Fixture { root, work };

    std::fs::write(f.work.join("locked/f.txt"), "v1\n").unwrap();
    f.git(&["init", "-q", "-b", "main", "."]);
    f.git(&["config", "user.email", "t@e.co"]);
    f.git(&["config", "user.name", "t"]);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "one"]);
    std::fs::write(f.work.join("locked/f.txt"), "v2\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "two"]);

    // A read-only FILE forces the unlink-and-replace path (an in-place write
    // would otherwise succeed, since that needs no permission on the directory).
    // A read-only DIRECTORY then blocks the unlink, so the checkout genuinely
    // cannot proceed.
    std::fs::set_permissions(f.work.join("locked/f.txt"), PermissionsExt::from_mode(0o444)).unwrap();
    std::fs::set_permissions(f.work.join("locked"), PermissionsExt::from_mode(0o500)).unwrap();

    let out = f.cmd(&["reset", "--hard", "HEAD~1"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "expected the checkout to fail, got: {out:?}");
    assert!(
        err.contains("locked/f.txt"),
        "the failure must name the file it could not write, got: {err}"
    );
}

/// The path must survive on the deeper trees this actually shows up in — a
/// single-component name could be right by accident.
#[test]
fn the_named_path_is_repo_relative_and_nested() {
    if running_as_root() {
        eprintln!("skipped: root ignores the permission bits this test relies on");
        return;
    }

    let root = std::env::temp_dir().join(format!("zvcs-ckerr2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(work.join("locked")).unwrap();
    let f = Fixture { root, work };

    std::fs::create_dir_all(f.work.join("a/b")).unwrap();
    std::fs::write(f.work.join("a/b/deep.txt"), "v1\n").unwrap();
    f.git(&["init", "-q", "-b", "main", "."]);
    f.git(&["config", "user.email", "t@e.co"]);
    f.git(&["config", "user.name", "t"]);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "one"]);
    std::fs::write(f.work.join("a/b/deep.txt"), "v2\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "two"]);

    std::fs::set_permissions(f.work.join("a/b/deep.txt"), PermissionsExt::from_mode(0o444)).unwrap();
    std::fs::set_permissions(f.work.join("a/b"), PermissionsExt::from_mode(0o500)).unwrap();

    let out = f.cmd(&["reset", "--hard", "HEAD~1"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);

    // Restore before the assertions, so a failure still leaves a removable tree.
    let _ = std::fs::set_permissions(f.work.join("a/b"), PermissionsExt::from_mode(0o700));
    let _ = std::fs::set_permissions(f.work.join("a/b/deep.txt"), PermissionsExt::from_mode(0o644));

    assert!(!out.status.success(), "expected the checkout to fail, got: {out:?}");
    assert!(
        err.contains("a/b/deep.txt"),
        "the failure must name the full repo-relative path, got: {err}"
    );
}

