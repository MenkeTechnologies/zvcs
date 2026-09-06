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
//! The other user cannot be conjured up without root, so the tests pin the
//! mechanism instead - which is what makes the owner irrelevant - through a
//! hardlink to the file being checked out over. A replaced file leaves its old
//! content behind on the link; a file written through carries the new content
//! to both names. The inode number cannot stand in for this: ext4 hands the
//! freed inode straight back to the next create, so the number survives the
//! replacement it is supposed to detect.
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

    /// A second name for `rela`, outside the repository so it is not itself a
    /// checkout target, holding the worktree file as it stands now.
    fn hardlink(&self, rela: &str) -> PathBuf {
        let link = self.root.join(format!("{}.link", rela.replace('/', "_")));
        std::fs::hard_link(self.path(rela), &link).unwrap();
        link
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

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn links(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().nlink()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_executable_entry_replaces_the_file_it_checks_out_over() {
    let f = Fixture::new("ckexe");
    let exe = f.path("exe.sh");
    let link = f.hardlink("exe.sh");

    f.git(&["reset", "--hard", "HEAD~1"]);

    assert_eq!(read(&exe), "v1\n", "the entry must be checked out");
    assert_eq!(
        read(&link),
        "v2\n",
        "the old file must be left behind, not written through: its mode can only be \
         reached by an fchmod, which fails on a file owned by someone else"
    );
    assert_eq!(links(&exe), 1, "the checked out file must be a fresh one");
    assert_eq!(mode(&exe) & 0o111, 0o111, "and it must still be executable");
}

/// The unlink is not the executable path's alone. `checkout_entry_ca()` clears
/// whatever is at the path before `create_file()` opens `O_WRONLY | O_CREAT |
/// O_EXCL` (entry.c:565-577, :89), so a plain entry replaces its file too, and
/// the mode is only the reason the comment there gives for it.
///
/// Measured on stock 2.55.0 over this same fixture — a hard link to each file,
/// then `reset --hard HEAD~1`:
///
/// ```text
/// plain.txt=v1 link=v2 nlink=1
/// exe.sh=v1    link=v2 nlink=1
/// ```
///
/// so both names part company and neither checked-out file keeps the link. The
/// earlier expectation here (link `v1`, nlink 2) was taken from the released
/// zvcs binary rather than from git, which is the one oracle this suite uses.
#[test]
fn a_plain_entry_replaces_its_file_the_same_way() {
    let f = Fixture::new("ckplain");
    let plain = f.path("plain.txt");
    let link = f.hardlink("plain.txt");

    f.git(&["reset", "--hard", "HEAD~1"]);

    assert_eq!(read(&plain), "v1\n", "the entry must be checked out");
    assert_eq!(read(&link), "v2\n", "the old file is left behind, as git leaves it");
    assert_eq!(links(&plain), 1, "the checked out file must be a fresh one");
}
