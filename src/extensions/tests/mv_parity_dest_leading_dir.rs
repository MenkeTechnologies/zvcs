//! What git 2.56's `cmd_mv()` checks about a destination's leading directories
//! before it renames anything, and how it names a `rename()` that fails anyway.
//!
//! * `has_symlink_leading_path(dst)` → `destination is beyond a symbolic link`
//!   (builtin/mv.c:453-456).
//! * `needs_worktree_rename()` moves → the leading directory is `lstat()`ed:
//!   missing is `destination directory does not exist`, a non-directory is
//!   `destination is not a directory` (builtin/mv.c:458-487). Both are `bad`, so
//!   `-n` reports them and `-k` skips the source, where 2.55 only found out when
//!   `rename()` failed.
//! * `die_errno(_("renaming '%s' to '%s' failed"), src, dst)` names both ends,
//!   and under `-k` a failed rename drops that entry only — the entries a
//!   directory source expanded to are still remapped in the index
//!   (builtin/mv.c:585-594).
//!
//! Expectations measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    /// `a`, `b`, `d/c`, `d/e` tracked, plus `l`, a tracked symlink to `d`.
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-mvlead-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("d")).unwrap();
        let fx = Fixture { dir };
        for (p, body) in [("a", "a\n"), ("b", "b\n"), ("d/c", "c\n"), ("d/e", "e\n")] {
            std::fs::write(fx.dir.join(p), body).unwrap();
        }
        std::os::unix::fs::symlink("d", fx.dir.join("l")).unwrap();
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx.ok(&["add", "a", "b", "d", "l"]);
        fx
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("HOME", &self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn tracked(&self) -> String {
        self.ok(&["ls-files"])
    }
}

const START: &str = "a\nb\nd/c\nd/e\nl\n";

fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap()
}

#[test]
fn a_missing_leading_directory_is_a_checking_phase_bad() {
    let fx = Fixture::new("missing");
    let out = fx.run(&["mv", "a", "nodir/a"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "fatal: destination directory does not exist, source=a, destination=nodir/a\n"
    );

    // `-n` reaches it in the checking loop, right after announcing the pair.
    let out = fx.run(&["mv", "-n", "a", "nodir/a"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(text(&out.stdout), "Checking rename of 'a' to 'nodir/a'\n");
    assert_eq!(
        text(&out.stderr),
        "fatal: destination directory does not exist, source=a, destination=nodir/a\n"
    );

    // `-k` skips the source like any other `bad`.
    let out = fx.run(&["mv", "-k", "a", "nodir/a"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stderr), "");

    // A leading component that is a file: missing below it, not-a-directory at it.
    let out = fx.run(&["mv", "a", "b/y/x"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "fatal: destination directory does not exist, source=a, destination=b/y/x\n"
    );
    let out = fx.run(&["mv", "a", "b/x"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(text(&out.stderr), "fatal: destination is not a directory, source=a, destination=b/x\n");

    assert_eq!(fx.tracked(), START);
    assert!(fx.dir.join("a").exists());
}

#[test]
fn a_destination_through_a_symlink_is_refused() {
    let fx = Fixture::new("symlink");
    let out = fx.run(&["mv", "a", "l/x"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "fatal: destination is beyond a symbolic link, source=a, destination=l/x\n"
    );
    // `l/` is a directory to `lstat()` with the slash, so the basename goes under
    // it — and the result lies beyond the link.
    let out = fx.run(&["mv", "a", "l/"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "fatal: destination is beyond a symbolic link, source=a, destination=l/a\n"
    );
    assert_eq!(fx.tracked(), START);
    assert!(!fx.dir.join("d/x").exists() && !fx.dir.join("d/a").exists());
}

#[test]
fn a_failed_directory_rename_names_both_ends() {
    let fx = Fixture::new("dir");
    // A directory source is not run through the leading-directory check, so the
    // missing parent surfaces from `rename()` itself.
    let out = fx.run(&["mv", "d", "nodir/d"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "fatal: renaming 'd' to 'nodir/d' failed: No such file or directory\n"
    );
    assert_eq!(fx.tracked(), START);
}

#[test]
fn keep_going_still_remaps_a_directorys_entries() {
    let fx = Fixture::new("keep");
    let out = fx.run(&["mv", "-v", "-k", "d", "nodir/d"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Renaming d to nodir/d\nRenaming d/c to nodir/d/c\nRenaming d/e to nodir/d/e\n"
    );
    assert_eq!(text(&out.stderr), "");
    // The directory stayed where it was on disk; the index moved its entries.
    assert_eq!(fx.tracked(), "a\nb\nl\nnodir/d/c\nnodir/d/e\n");
    assert!(fx.dir.join("d/c").exists());
}
