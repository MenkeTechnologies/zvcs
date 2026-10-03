//! A `.git/objects/??` entry that cannot be opened as a directory ends the scan.
//!
//! `for_each_file_in_obj_subdir()` answers an `opendir()` failure other than
//! `ENOENT` with `error_errno(_("unable to open %s"), path)` and a non-zero
//! return, and `for_each_loose_file_in_source()` stops at the first non-zero
//! return (object-file.c:1061-1066, :1121-1126). Every loose object in a later
//! subdirectory is never handed to `fsck_loose()`, so it never gets `HAS_OBJ`:
//! a ref naming one is `missing`, its reflog entries are invalid, and the
//! command exits 2. `--connectivity-only` walks with `odb_for_each_object()`,
//! which stops in the same place — once for the listing, once more for
//! `mark_unreachable_referents()`. zvcs listed the odb through gitoxide, which
//! skips the stray file, and exited 0.
//!
//! The fixture's ids are fixed by its identity and dates: blob `78…`, tree
//! `aa…`, commit `ec…` — so a regular file at `objects/ab` stops the walk
//! between the tree and the commit. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const COMMIT: &str = "ec32ad56ab88b04fc56823378229420887115719";

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-unopenable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "master"]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    git(&root, &["add", "a"]);
    git(&root, &["commit", "-q", "-m", "x"]);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]).0.trim_end(), COMMIT);
    std::fs::write(root.join(".git/objects/ab"), "").unwrap();
    root
}

#[test]
fn objects_past_an_unopenable_subdirectory_are_missing() {
    let root = fixture();
    let missing = format!("missing commit {COMMIT}\n");
    let unopenable = "error: unable to open .git/objects/ab: Not a directory\n";
    let reflogs = format!(
        "error: HEAD: invalid reflog entry {COMMIT}\nerror: refs/heads/master: invalid reflog entry {COMMIT}\n"
    );

    let (out, err, code) = git(&root, &["fsck"]);
    assert_eq!((out.as_str(), code), (missing.as_str(), 2));
    assert_eq!(err, format!("{unopenable}{reflogs}"));

    let (out, err, code) = git(&root, &["fsck", "--connectivity-only"]);
    assert_eq!((out.as_str(), code), (missing.as_str(), 2));
    assert_eq!(err, format!("{unopenable}{reflogs}{unopenable}"));

    let _ = std::fs::remove_dir_all(&root);
}
