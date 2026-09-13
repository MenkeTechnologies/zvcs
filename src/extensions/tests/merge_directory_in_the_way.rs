//! merge-ort's file/directory handling where a directory rename or a rename
//! decides what is in the way, driven through `merge-tree`.
//!
//! Every expectation below was measured from git 2.55.0 on the fixture the test
//! builds, which is t6423 12m: `dir/subdir/file` in the base, `rename` moves
//! `dir/` to `renamed-dir/`, `symlink` deletes the file and puts a symlink at
//! `dir/subdir`.
#![cfg(unix)]

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-dir-in-way-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Fixture { root, work }
    }

    /// t6423 12m.
    fn symlink_replaces_renamed_directory(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("dir/subdir/file", "1\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "O"]);
        f.git(&["branch", "rename"]);
        f.git(&["branch", "symlink"]);
        f.git(&["checkout", "-q", "rename"]);
        f.git(&["mv", "dir", "renamed-dir"]);
        f.git(&["commit", "-qm", "A"]);
        f.git(&["checkout", "-q", "symlink"]);
        f.git(&["rm", "-q", "dir/subdir/file"]);
        std::fs::create_dir_all(f.work.join("dir")).unwrap();
        std::os::unix::fs::symlink("/dev/null", f.work.join("dir/subdir")).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "B"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@e.com")
            .env("GIT_AUTHOR_DATE", "@1700000000+0000")
            .env("GIT_COMMITTER_DATE", "@1700000000+0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

/// Without directory renames the symlink stays at `dir/subdir`, whose directory
/// held nothing but the rename's source and so merges to nothing: the symlink
/// is placed there without a conflict in either operand order. With `rename`
/// first it was moved aside to `dir/subdir~symlink` as if the directory stayed.
#[test]
fn a_directory_holding_only_a_rename_source_is_not_in_the_way() {
    let f = Fixture::symlink_replaces_renamed_directory("gone");
    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=false", "merge-tree", "--write-tree", "rename", "symlink"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "13ab88ed1ecbc999c1e3c4b567977553ef27e948\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 1\trenamed-dir/subdir/file\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 2\trenamed-dir/subdir/file\n\
         \n\
         CONFLICT (rename/delete): dir/subdir/file renamed to renamed-dir/subdir/file in rename, but deleted in symlink.\n"
    );
    assert_eq!(code, 1);

    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=false", "merge-tree", "--write-tree", "symlink", "rename"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "13ab88ed1ecbc999c1e3c4b567977553ef27e948\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 1\trenamed-dir/subdir/file\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 3\trenamed-dir/subdir/file\n\
         \n\
         CONFLICT (rename/delete): dir/subdir/file renamed to renamed-dir/subdir/file in rename, but deleted in symlink.\n"
    );
    assert_eq!(code, 1);
}

/// The symlink follows the directory rename into `renamed-dir/subdir`, where the
/// rename's `renamed-dir/subdir/file` keeps a directory, so it is moved aside.
/// That file/directory notice was refused as an unported message class.
#[test]
fn a_file_moved_into_a_surviving_directory_is_reported_as_file_directory() {
    let f = Fixture::symlink_replaces_renamed_directory("msg");
    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=conflict", "merge-tree", "--write-tree", "rename", "symlink"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "a144b1a71526cfe108198812d622947ca6811cbf\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 1\trenamed-dir/subdir/file\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 2\trenamed-dir/subdir/file\n\
         120000 dc1dc0cde0f7dff7b7f7c9347fff75936d705cb8 3\trenamed-dir/subdir~symlink\n\
         \n\
         CONFLICT (file location): dir/subdir added in symlink inside a directory that was renamed in rename, suggesting it should perhaps be moved to renamed-dir/subdir.\n\
         CONFLICT (rename/delete): dir/subdir/file renamed to renamed-dir/subdir/file in rename, but deleted in symlink.\n\
         CONFLICT (file/directory): directory in the way of renamed-dir/subdir from symlink; moving it to renamed-dir/subdir~symlink instead.\n"
    );
    assert_eq!(code, 1);

    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=true", "merge-tree", "--write-tree", "rename", "symlink"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "a144b1a71526cfe108198812d622947ca6811cbf\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 1\trenamed-dir/subdir/file\n\
         100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 2\trenamed-dir/subdir/file\n\
         120000 dc1dc0cde0f7dff7b7f7c9347fff75936d705cb8 3\trenamed-dir/subdir~symlink\n\
         \n\
         Path updated: dir/subdir added in symlink inside a directory that was renamed in rename; moving it to renamed-dir/subdir.\n\
         CONFLICT (rename/delete): dir/subdir/file renamed to renamed-dir/subdir/file in rename, but deleted in symlink.\n\
         CONFLICT (file/directory): directory in the way of renamed-dir/subdir from symlink; moving it to renamed-dir/subdir~symlink instead.\n"
    );
    assert_eq!(code, 1);
}
