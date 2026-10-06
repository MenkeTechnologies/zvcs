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

/// A mode-0 fan-out directory under a superuser is still readable; the claims
/// below are about `EACCES` and have nothing to observe there.
fn unreadable(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read_dir(dir).is_err()
}

fn readable_again(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A head or cache-tree id whose loose file exists but cannot be opened is not
/// absent: `parse_object()` reads it with `OBJECT_INFO_DIE_IF_CORRUPT`, each of
/// the two reads prints `unable to open loose object` (odb/source-loose.c:121-129,
/// odb.c:574-585), and the die names the file (odb.c:597-605,
/// odb/source-loose.c:196-198). zvcs took the odb's miss at face value — an
/// `invalid sha1 pointer` line for the ref, or for the cache-tree node — and kept
/// going; for a ref it also failed the whole command on gitoxide's own walk error.
#[test]
fn an_unopenable_head_or_cache_tree_object_dies_as_corrupt() {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-eacces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("d")).unwrap();
    git(&root, &["init", "-q", "-b", "master"]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    git(&root, &["add", "a"]);
    git(&root, &["commit", "-q", "-m", "x"]);
    std::fs::write(root.join("d/b"), "b\n").unwrap();
    git(&root, &["add", "d"]);
    let tree = "c9b801068840df6bd9a0cd4efec5d00fafdbe36a";
    assert_eq!(git(&root, &["write-tree"]).0.trim_end(), tree);

    let corrupt = |id: &str| {
        let open = format!("error: unable to open loose object {id}: Permission denied\n");
        format!("{open}{open}fatal: loose object {id} (stored in .git/objects/{}/{}) is corrupt\n", &id[..2], &id[2..])
    };

    let dir = root.join(".git/objects/ec");
    if unreadable(&dir) {
        let (out, err, code) = git(&root, &["fsck"]);
        readable_again(&dir);
        assert_eq!((out.as_str(), code), ("", 128));
        assert_eq!(err, corrupt(COMMIT));
    }
    readable_again(&dir);

    let dir = root.join(".git/objects/c9");
    if unreadable(&dir) {
        let (out, err, code) = git(&root, &["fsck"]);
        readable_again(&dir);
        assert_eq!((out.as_str(), code), ("", 128));
        assert_eq!(
            err,
            format!(
                "error: unable to open .git/objects/c9: Permission denied\n\
                 error: HEAD: invalid reflog entry {COMMIT}\n\
                 error: refs/heads/master: invalid reflog entry {COMMIT}\n{}",
                corrupt(tree)
            )
        );
    }
    readable_again(&dir);
    let _ = std::fs::remove_dir_all(&root);
}
