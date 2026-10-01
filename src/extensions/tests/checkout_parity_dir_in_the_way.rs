//! A directory standing where a switch removes or writes a file was never
//! looked into.
//!
//! `twoway_merge()` hands a path the switch deletes to `deleted_entry()`, whose
//! `verify_absent_if_directory()` runs before `verify_uptodate()`
//! (unpack-trees.c:2675-2693), and a path it adds to `merged_entry()`'s
//! `verify_absent()` (unpack-trees.c:2566-2584). Both reach
//! `check_ok_to_remove()`, which hands a directory on disk to
//! `verify_clean_subdirectory()` (unpack-trees.c:2320-2393): every tracked entry
//! inside must be up to date, and `read_directory()` must find nothing
//! untracked, or the switch is refused with `ERROR_NOT_UPTODATE_DIR`:
//!
//! ```text
//! error: Updating the following directories would lose untracked files in them:
//!         b
//!
//! Aborting
//! ```
//!
//! A clean directory passes that check but not `verify_uptodate()`: `lstat()`
//! finds a directory where the index has a file, which is not `ENOENT`, so
//! the path is a local change. zvcs treated a directory in the way as a
//! deleted file and switched, leaving the untracked files mixed into the new
//! tree. Expectations measured from stock git 2.56.0 under the same
//! environment.

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
    /// `main` tracks the file `b` and `d/c`; `todir` turns `b` into `b/z`,
    /// `tofile` turns `d/` into the file `d`. `main` is checked out.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-dir-in-the-way-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("b", "b\n");
        f.write("d/c", "c\n");
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["checkout", "-q", "-b", "todir"]);
        f.run(&["rm", "-q", "b"]);
        f.write("b/z", "z\n");
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "todir"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["checkout", "-q", "-b", "tofile"]);
        f.run(&["rm", "-q", "-r", "d"]);
        f.write("d", "f\n");
        f.run(&["add", "d"]);
        f.run(&["commit", "-q", "-m", "tofile"]);
        f.run(&["checkout", "-q", "main"]);
        f
    }

    fn write(&self, path: &str, content: &str) {
        let full = self.work.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    /// Replace the tracked file `b` with a directory.
    fn b_as_dir(&self) {
        std::fs::remove_file(self.work.join("b")).unwrap();
        std::fs::create_dir(self.work.join("b")).unwrap();
    }

    fn head(&self) -> String {
        self.run(&["symbolic-ref", "--short", "HEAD"]).0
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

fn lose_untracked(dir: &str) -> String {
    format!(
        "error: Updating the following directories would lose untracked files in them:\n\
         \t{dir}\n\nAborting\n"
    )
}

const B_IS_A_LOCAL_CHANGE: &str =
    "error: Your local changes to the following files would be overwritten by checkout:\n\
     \tb\n\
     Please commit your changes or stash them before you switch branches.\n\
     Aborting\n";

#[test]
fn untracked_files_in_a_directory_replacing_a_deleted_file_refuse() {
    let f = Fixture::new("deleted");
    f.b_as_dir();
    f.write("b/sub/q", "q\n");
    let (out, err, code) = f.run(&["checkout", "todir"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert_eq!(err, lose_untracked("b"));
    assert_eq!(f.head(), "main\n");
    assert!(f.work.join("b/sub/q").exists());
}

#[test]
fn the_subtree_under_a_refused_directory_is_not_checked() {
    // `b/z` is untracked and the target writes `b/z`, but the refusal of `b`
    // stops the traversal before `verify_absent(b/z)` could name it.
    let f = Fixture::new("subtree");
    f.b_as_dir();
    f.write("b/z", "q\n");
    let (_, err, code) = f.run(&["checkout", "todir"]);
    assert_eq!(code, 1);
    assert_eq!(err, lose_untracked("b"));
}

#[test]
fn a_clean_directory_is_still_a_local_change() {
    let f = Fixture::new("empty");
    f.b_as_dir();
    let (_, err, code) = f.run(&["checkout", "todir"]);
    assert_eq!(code, 1);
    assert_eq!(err, B_IS_A_LOCAL_CHANGE);

    // Ignored files do not count as untracked under the default
    // `--overwrite-ignore`, so this is the same local change.
    f.write("b/x.ign", "x\n");
    f.write(".git/info/exclude", "*.ign\n");
    let (_, err, code) = f.run(&["checkout", "todir"]);
    assert_eq!(code, 1);
    assert_eq!(err, B_IS_A_LOCAL_CHANGE);
}

#[test]
fn a_tracked_directory_replaced_by_a_file_keeps_its_untracked_files() {
    let f = Fixture::new("tofile");
    f.write("d/new", "n\n");
    let (_, err, code) = f.run(&["checkout", "tofile"]);
    assert_eq!(code, 1);
    assert_eq!(err, lose_untracked("d"));
    assert_eq!(f.run(&["status", "--porcelain"]).0, "?? d/new\n");

    // A modified tracked file inside is refused under its own name first.
    std::fs::remove_file(f.work.join("d/new")).unwrap();
    f.write("d/c", "mod\n");
    let (_, err, code) = f.run(&["checkout", "tofile"]);
    assert_eq!(code, 1);
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by checkout:\n\
         \td/c\n\
         Please commit your changes or stash them before you switch branches.\n\
         Aborting\n"
    );

    // Clean, it switches.
    f.write("d/c", "c\n");
    let (_, err, code) = f.run(&["checkout", "tofile"]);
    assert_eq!((err.as_str(), code), ("Switched to branch 'tofile'\n", 0));
}

#[test]
fn untracked_files_in_a_tracked_directory_that_stays_do_not_matter() {
    let f = Fixture::new("stays");
    f.write("d/c2/f", "y\n");
    let (_, err, code) = f.run(&["checkout", "todir"]);
    assert_eq!((err.as_str(), code), ("Switched to branch 'todir'\n", 0));
}

#[test]
fn merge_shares_the_gate() {
    let f = Fixture::new("merge");
    f.b_as_dir();
    f.write("b/sub/q", "q\n");
    let (_, err, code) = f.run(&["merge", "todir"]);
    assert_eq!(code, 1);
    assert_eq!(err, lose_untracked("b"));
}
