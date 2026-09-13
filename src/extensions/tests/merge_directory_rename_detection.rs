//! merge-ort's directory rename detection, driven through `merge-tree`.
//!
//! Every expectation below was measured from git 2.55.0 on the fixture the test
//! builds. The fixture is the one shape that separates merge-ort's rule from
//! "follow a tree object that moved": `main` moves `z/b` and `z/c` into a `y/`
//! that already exists, so no tree is renamed wholesale and only the file
//! renames say that `z/` went to `y/` (`update_dir_rename_counts()`,
//! diffcore-rename.c:455-570). A port that only followed wholesale tree renames
//! left `z/d` where the other side put it and called the merge clean.
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
    /// base: `z/{b,c}`, `y/x`; `main`: `z/{b,c}` moved to `y/`;
    /// `add`: adds `z/d`; `inway`: adds `z/d` and an unrelated `y/d`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-dirrename-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("z/b", "b\n");
        f.write("z/c", "c\n");
        f.write("y/x", "x\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["branch", "add"]);
        f.git(&["branch", "inway"]);
        f.git(&["mv", "z/b", "y/b"]);
        f.git(&["mv", "z/c", "y/c"]);
        f.git(&["commit", "-qm", "move"]);
        f.git(&["checkout", "-q", "add"]);
        f.write("z/d", "d\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "add"]);
        f.git(&["checkout", "-q", "inway"]);
        f.write("z/d", "d\n");
        f.write("y/d", "other\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "inway"]);
        f.git(&["checkout", "-q", "main"]);
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

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

#[test]
fn an_addition_follows_a_rename_derived_from_file_renames() {
    let f = Fixture::new("conflict");
    let (code, out) = f.run(&["merge-tree", "--write-tree", "main", "add"]);
    assert_eq!(
        out,
        "34a60ce34a731d6a479833fdfa4c687fa451a3f1\n\
         100644 4bcfe98e640c8284511312660fb8709b0afa888e 3\ty/d\n\
         \n\
         CONFLICT (file location): z/d added in add inside a directory that was renamed in main, suggesting it should perhaps be moved to y/d.\n"
    );
    assert_eq!(code, 1);

    let (code, out) = f.run(&["merge-tree", "--write-tree", "-z", "main", "add"]);
    assert_eq!(
        out,
        "34a60ce34a731d6a479833fdfa4c687fa451a3f1\0\
         100644 4bcfe98e640c8284511312660fb8709b0afa888e 3\ty/d\0\
         \0\
         2\0y/d\0z/d\0CONFLICT (directory rename suggested)\0\
         CONFLICT (file location): z/d added in add inside a directory that was renamed in main, suggesting it should perhaps be moved to y/d.\n\0"
    );
    assert_eq!(code, 1);
}

#[test]
fn directory_renames_true_moves_the_addition_without_a_conflict() {
    let f = Fixture::new("true");
    let (code, out) = f.run(&["-c", "merge.directoryRenames=true", "merge-tree", "--write-tree", "main", "add"]);
    assert_eq!(out, "34a60ce34a731d6a479833fdfa4c687fa451a3f1\n");
    assert_eq!(code, 0);
}

#[test]
fn a_path_in_the_way_keeps_the_addition_where_it_was() {
    let f = Fixture::new("inway");
    let (code, out) = f.run(&["merge-tree", "--write-tree", "main", "inway"]);
    assert_eq!(
        out,
        "c7be1b5bab4ae0007afe755f4e52c8288450abce\n\
         \n\
         CONFLICT (implicit dir rename): Existing file/dir at y/d in the way of implicit directory rename(s) putting the following path(s) there: z/d.\n"
    );
    assert_eq!(code, 1);

    let (code, out) = f.run(&["merge-tree", "--write-tree", "-z", "main", "inway"]);
    assert_eq!(
        out,
        "c7be1b5bab4ae0007afe755f4e52c8288450abce\0\
         \0\
         2\0y/d\0z/d\0CONFLICT (file in way of directory rename)\0\
         CONFLICT (implicit dir rename): Existing file/dir at y/d in the way of implicit directory rename(s) putting the following path(s) there: z/d.\n\0"
    );
    assert_eq!(code, 1);
}
