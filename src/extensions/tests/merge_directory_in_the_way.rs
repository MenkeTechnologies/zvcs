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

    /// base `p/d/x` and `p/q`; `A` deletes `p/d/x` and adds a file at `p/d`; `B` deletes
    /// `p/d/x` and moves `p/q` to `r/q`, so `p/` went to `r/` and nothing of `B` is at `r/d`.
    fn file_replaces_directory_side1(tag: &str) -> Self {
        let f = Fixture::new(tag);
        let seq = |n: u32| (1..=n).map(|i| format!("{i}\n")).collect::<String>();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("p/d/x", &seq(20));
        f.write("p/q", &seq(30));
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "O"]);
        f.git(&["branch", "A"]);
        f.git(&["branch", "B"]);
        f.git(&["checkout", "-q", "A"]);
        f.git(&["rm", "-q", "p/d/x"]);
        f.write("p/d", "new\n");
        f.git(&["add", "p/d"]);
        f.git(&["commit", "-qm", "A"]);
        f.git(&["checkout", "-q", "B"]);
        f.git(&["rm", "-q", "p/d/x"]);
        std::fs::create_dir_all(f.work.join("r")).unwrap();
        f.git(&["mv", "p/q", "r/q"]);
        f.git(&["commit", "-qm", "B"]);
        f
    }

    /// t6423 7e, with its branch names: base `z/{b,c}` and `x/d`; `A` moves `z/` to `y/`,
    /// deletes `x/d` and adds `x/d/f` and `y/d/g`; `B` moves `x/d` to `z/d`.
    fn transitive_rename_delete_with_directories_in_the_way(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("z/b", "b\n");
        f.write("z/c", "c\n");
        f.write("x/d", "d1\n");
        f.git(&["add", "z", "x"]);
        f.git(&["commit", "-qm", "O"]);
        f.git(&["branch", "A"]);
        f.git(&["branch", "B"]);
        f.git(&["checkout", "-q", "A"]);
        f.git(&["mv", "z", "y"]);
        f.git(&["rm", "-q", "x/d"]);
        f.write("x/d/f", "f\n");
        f.write("y/d/g", "g\n");
        f.git(&["add", "x/d/f", "y/d/g"]);
        f.git(&["commit", "-qm", "A"]);
        f.git(&["checkout", "-q", "B"]);
        f.git(&["mv", "x/d", "z/"]);
        f.git(&["commit", "-qm", "B"]);
        f
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

/// With `symlink` as the first operand the symlink is side 1's, and the copy that
/// follows the directory rename has its side-1 stage cleared (merge-ort.c:2792-2797):
/// the moved-aside entry is recorded as mode 0 with the null id and written into the
/// tree as a `0` entry. It was recorded and written as the symlink.
#[test]
fn a_side1_file_leaving_a_base_directory_loses_its_stage() {
    let f = Fixture::symlink_replaces_renamed_directory("zero");
    for (mode, first_line) in [
        ("conflict", "CONFLICT (file location): dir/subdir added in symlink inside a directory that was renamed in rename, suggesting it should perhaps be moved to renamed-dir/subdir.\n"),
        ("true", "Path updated: dir/subdir added in symlink inside a directory that was renamed in rename; moving it to renamed-dir/subdir.\n"),
    ] {
        let (code, out, err) = f.run(&["-c", &format!("merge.directoryRenames={mode}"), "merge-tree", "--write-tree", "symlink", "rename"]);
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "a004c0a5a4a5ff7f27b048d990d242a4908577b9\n\
                 100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 1\trenamed-dir/subdir/file\n\
                 100644 d00491fd7e5bb6fa28c517a0bb32b8b506539d4d 3\trenamed-dir/subdir/file\n\
                 000000 0000000000000000000000000000000000000000 2\trenamed-dir/subdir~symlink\n\
                 \n\
                 {first_line}\
                 CONFLICT (rename/delete): dir/subdir/file renamed to renamed-dir/subdir/file in rename, but deleted in symlink.\n\
                 CONFLICT (file/directory): directory in the way of renamed-dir/subdir from symlink; moving it to renamed-dir/subdir~symlink instead.\n"
            ),
            "merge.directoryRenames={mode}"
        );
        assert_eq!(code, 1);
    }
}

/// The cleared stage without a file/directory conflict: `A`'s `p/d` follows `p/ -> r/`
/// and lands as a `0` entry with the null id. As the second operand it keeps its blob.
#[test]
fn a_cleared_side1_stage_is_written_without_a_conflict_too() {
    let f = Fixture::file_replaces_directory_side1("zero-clean");
    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=conflict", "merge-tree", "--write-tree", "A", "B"]);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "8e683f82d485e319440a4c96356c60d68f697da9\n\
         000000 0000000000000000000000000000000000000000 2\tr/d\n\
         \n\
         CONFLICT (file location): p/d added in A inside a directory that was renamed in B, suggesting it should perhaps be moved to r/d.\n"
    );
    assert_eq!(code, 1);

    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=true", "merge-tree", "--write-tree", "A", "B"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "8e683f82d485e319440a4c96356c60d68f697da9\n", ""));

    let (code, out, err) = f.run(&["-c", "merge.directoryRenames=true", "merge-tree", "--write-tree", "B", "A"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "ec21e7e59289f085e95dbd651305a6eb44b4bcb4\n", ""));
}

/// B's `x/d -> z/d` follows A's `z/ -> y/` to `y/d`, meets A's deletion of `x/d`, and
/// the rename/delete's `y/d` is moved aside for A's `y/d/g`. The base the rename brought
/// along goes with it: stage 1 at `y/d~B` next to B's version. It was left out.
#[test]
fn a_rename_delete_moved_aside_keeps_the_base_stage() {
    let f = Fixture::transitive_rename_delete_with_directories_in_the_way("base");
    for (mode, first_line) in [
        ("conflict", "CONFLICT (file location): x/d renamed to z/d in B, inside a directory that was renamed in A, suggesting it should perhaps be moved to y/d.\n"),
        ("true", "Path updated: x/d renamed to z/d in B, inside a directory that was renamed in A; moving it to y/d.\n"),
    ] {
        let (code, out, err) = f.run(&["-c", &format!("merge.directoryRenames={mode}"), "merge-tree", "--write-tree", "A", "B"]);
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "79bf2ef24bff2af81ebd7ad7542eda37f1cd8fc3\n\
                 100644 6f1852975b9306ae5d8dfdf0d4cb1f5cb36ac229 1\ty/d~B\n\
                 100644 6f1852975b9306ae5d8dfdf0d4cb1f5cb36ac229 3\ty/d~B\n\
                 \n\
                 {first_line}\
                 CONFLICT (rename/delete): x/d renamed to y/d in B, but deleted in A.\n\
                 CONFLICT (file/directory): directory in the way of y/d from B; moving it to y/d~B instead.\n"
            ),
            "merge.directoryRenames={mode}"
        );
        assert_eq!(code, 1);
    }
}

/// The same merge with `B` first: A's `y/d/g` lies beneath B's renamed `y/d`, which is a
/// file/directory conflict at `y/d` and no content merge. The two were merged as an
/// add/add at `y/d` ("Auto-merging y/d") into a different tree.
#[test]
fn a_path_added_beneath_a_rename_destination_is_not_merged_into_it() {
    let f = Fixture::transitive_rename_delete_with_directories_in_the_way("beneath");
    for (mode, first_line) in [
        ("conflict", "CONFLICT (file location): x/d renamed to z/d in B, inside a directory that was renamed in A, suggesting it should perhaps be moved to y/d.\n"),
        ("true", "Path updated: x/d renamed to z/d in B, inside a directory that was renamed in A; moving it to y/d.\n"),
    ] {
        let (code, out, err) = f.run(&["-c", &format!("merge.directoryRenames={mode}"), "merge-tree", "--write-tree", "B", "A"]);
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "79bf2ef24bff2af81ebd7ad7542eda37f1cd8fc3\n\
                 100644 6f1852975b9306ae5d8dfdf0d4cb1f5cb36ac229 1\ty/d~B\n\
                 100644 6f1852975b9306ae5d8dfdf0d4cb1f5cb36ac229 2\ty/d~B\n\
                 \n\
                 {first_line}\
                 CONFLICT (rename/delete): x/d renamed to y/d in B, but deleted in A.\n\
                 CONFLICT (file/directory): directory in the way of y/d from B; moving it to y/d~B instead.\n"
            ),
            "merge.directoryRenames={mode}"
        );
        assert_eq!(code, 1);
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
