//! `git clean -fd` over a directory it can only partly empty.
//!
//! `cmd_clean()` hands every directory candidate to `remove_dirs()`
//! (builtin/clean.c:163-292), which walks the subtree itself. That recursion is
//! not an implementation detail — it is what the output is made of:
//!
//!   * a leaf that refuses to go is named by *its own* path, not by the
//!     candidate it happens to live under;
//!   * everything else in the subtree is still removed, rather than the removal
//!     stopping at the first failure;
//!   * when the directory survives, the paths that did go are listed one by one,
//!     in place of the single line the whole directory would have got.
//!
//! A bulk `remove_dir_all` gives one verdict for the whole subtree and so gets
//! all three wrong at once: it blamed the directory, it left files behind that
//! git removes, and it reported nothing about what it had already deleted — the
//! caller could not tell which half of the tree was gone.
//!
//! The obstacle here is a directory with no write permission, so these tests are
//! skipped when the process can write into it anyway (running as root).
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The fixture deliberately leaves an unwritable directory behind.
        let _ = std::fs::set_permissions(
            self.work.join("top/locked"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One tracked file, and an untracked `top/` holding two removable files and
    /// a `locked/` subdirectory whose single file cannot be unlinked.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cleanpartial-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("top/locked")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("tracked.txt", b"r\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.write("top/a", b"a\n");
        f.write("top/z", b"z\n");
        f.write("top/locked/b", b"b\n");
        std::fs::set_permissions(
            f.work.join("top/locked"),
            std::os::unix::fs::PermissionsExt::from_mode(0o500),
        )
        .unwrap();
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
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }
}

/// Whether the fixture's obstacle is real for this process. A `0o500` directory
/// is no obstacle to root, and CI often runs as root, so the tests that depend on
/// the removal failing announce themselves as skipped instead of failing.
fn permissions_bite(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(ToOwned::to_owned).collect()
}

/// The failure is reported against the file that refused, and every other path in
/// the subtree is still removed and still reported.
#[test]
fn a_directory_that_cannot_be_emptied_names_the_leaf_and_removes_the_rest() {
    let f = Fixture::new("leaf");
    if !permissions_bite(&f.work.join("top/locked")) {
        eprintln!("skipped: this process can write into a 0o500 directory");
        return;
    }

    let out = f.run(&["clean", "-fd"]);
    assert_eq!(out.status.code(), Some(1), "a failed removal is exit 1: {out:?}");
    assert_eq!(
        lines(&out.stderr),
        ["warning: failed to remove top/locked/b: Permission denied"],
        "the leaf that refused is what gets blamed, not `top/`"
    );

    // Walk order is the filesystem's, so compare as a set.
    let mut got = lines(&out.stdout);
    got.sort();
    assert_eq!(got, ["Removing top/a", "Removing top/z"]);

    assert!(!f.exists("top/a"), "the failure did not stop the removal");
    assert!(!f.exists("top/z"), "a file listed after the failing one still goes");
    assert!(f.exists("top/locked/b"), "the unremovable file is still there");
    assert!(f.exists("top"), "and so is the directory that could not be emptied");
    assert!(f.exists("tracked.txt"));
}

/// `-q` drops the per-path lines but keeps the warning: git prints the surviving
/// directory's contents under `!quiet`, and reports failures unconditionally.
#[test]
fn quiet_keeps_the_warning_and_drops_the_listing() {
    let f = Fixture::new("quiet");
    if !permissions_bite(&f.work.join("top/locked")) {
        eprintln!("skipped: this process can write into a 0o500 directory");
        return;
    }

    let out = f.run(&["clean", "-fdq"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        lines(&out.stderr),
        ["warning: failed to remove top/locked/b: Permission denied"]
    );
    assert_eq!(lines(&out.stdout), Vec::<String>::new());
    assert!(!f.exists("top/a"), "`-q` is about output, not about what is removed");
}

/// A dry run removes nothing, so it cannot discover the obstacle: the directory
/// is reported as a whole, exactly as it is when nothing is in the way.
#[test]
fn a_dry_run_reports_the_directory_as_one_line() {
    let f = Fixture::new("dryrun");
    let out = f.run(&["clean", "-nd"]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(lines(&out.stdout), ["Would remove top/"]);
    assert_eq!(lines(&out.stderr), Vec::<String>::new());
    assert!(f.exists("top/a"), "a dry run removes nothing");
}

/// The ordinary case is unchanged by the recursion: a directory that empties
/// cleanly is one line, and its contents are never listed.
#[test]
fn a_directory_that_empties_cleanly_is_still_reported_as_one_line() {
    let f = Fixture::new("clean");
    std::fs::create_dir_all(f.work.join("free/deep")).unwrap();
    f.write("free/one", b"1\n");
    f.write("free/deep/two", b"2\n");

    let out = f.run(&["clean", "-fd", "free"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(lines(&out.stdout), ["Removing free/"]);
    assert_eq!(lines(&out.stderr), Vec::<String>::new());
    assert!(!f.exists("free"), "the whole subtree went");
}
