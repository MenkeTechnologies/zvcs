//! Checking out over an executable file must replace it, not `fchmod` it.
//!
//! `open` applies its mode only to a file it creates, so writing into an
//! existing file and setting the executable bit afterwards needs `fchmod` -
//! which returns EPERM for anyone but the file's owner, even when the directory
//! allows replacing the file outright. A Homebrew prefix shared between two
//! accounts is the everyday case: `brew update` reset the repository over a
//! `775` file owned by the other account and died with
//!
//! ```text
//! zvcs: reset: IO error while writing blob or reading file metadata or changing filetype for 'Library/Homebrew/cmd/unalias.rb': Operation not permitted (os error 1)
//! ```
//!
//! while git checked the same tree out without complaint. git never chmods
//! here: `checkout_entry` unlinks the old entry and `create_file` recreates it
//! with the final mode, so the file it writes is always its own.
//!
//! The other user cannot be conjured up without root, so the test pins the
//! mechanism instead - the file is replaced (new inode) rather than written
//! through - which is what makes the owner irrelevant.
#![cfg(unix)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
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

impl Fixture {
    /// A two-commit repository whose worktree holds the second commit.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };

        f.write("exe.sh", "v1\n", 0o755);
        f.write("plain.txt", "v1\n", 0o644);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.write("exe.sh", "v2\n", 0o755);
        f.write("plain.txt", "v2\n", 0o644);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "two"]);
        f
    }

    fn write(&self, rela: &str, content: &str, mode: u32) {
        let path = self.work.join(rela);
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(&path, PermissionsExt::from_mode(mode)).unwrap();
    }

    fn path(&self, rela: &str) -> PathBuf {
        self.work.join(rela)
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }
}

fn inode(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().ino()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_executable_entry_replaces_the_file_it_checks_out_over() {
    let f = Fixture::new("ckexe");
    let exe = f.path("exe.sh");
    let before = inode(&exe);

    f.git(&["reset", "--hard", "HEAD~1"]);

    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "v1\n");
    assert_ne!(
        inode(&exe),
        before,
        "the executable must be unlinked and recreated, or its mode can only be reached \
         through an fchmod that fails on a file owned by someone else"
    );
    assert_eq!(mode(&exe) & 0o111, 0o111, "the checked out file must stay executable");
}

/// Only the executable path pays for the extra unlink - everything else keeps
/// writing through the existing file, which is both cheaper and unaffected by
/// the ownership problem, since it never needs a mode change.
#[test]
fn a_plain_entry_is_still_written_in_place() {
    let f = Fixture::new("ckplain");
    let plain = f.path("plain.txt");
    let before = inode(&plain);

    f.git(&["reset", "--hard", "HEAD~1"]);

    assert_eq!(std::fs::read_to_string(&plain).unwrap(), "v1\n");
    assert_eq!(inode(&plain), before, "a non-executable entry needs no replacement");
}
