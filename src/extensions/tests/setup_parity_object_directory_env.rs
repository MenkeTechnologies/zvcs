//! `$GIT_OBJECT_DIRECTORY` replaces the repository's object database.
//!
//! `odb_new(repo, ODB_NEW_HONOR_ENV)` (odb.c:1076-1080, setup.c:2104, v2.56.0)
//! takes the variable as the primary object source, verbatim. An existing but
//! empty directory is therefore an empty object database: `HEAD` still
//! resolves, because a ref is read without opening its object, and every
//! command that then opens the object reports it missing. zvcs opened
//! `<git dir>/objects` whatever the variable said.
//!
//! A relative value is opened after setup's `chdir()`: from the top of the work
//! tree when the current directory is inside it, from the current directory
//! otherwise.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const HEAD: &str = "14d5db65fc01e7fd0b3577571213e1725e5e53f0";
/// `new\n`, which is not in the repository's own object database.
const NEW_BLOB: &str = "3e757656cf36eca53338e520d134963a44f793f8";
/// `base\n`, the committed `file`.
const FILE_BLOB: &str = "df967b96a579e45a18b8251732d16804b2e56a55";

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
        let root = std::env::temp_dir().join(format!("zvcs-objdir-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&f.work, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        std::fs::write(f.work.join("new"), "new\n").unwrap();
        f.ok(&f.work, &[], &["add", "file"]);
        f.ok(&f.work, &[], &["commit", "-q", "-m", "base"]);
        for dir in ["empty", "x", "sub/deep/x"] {
            std::fs::create_dir_all(f.work.join(dir)).unwrap();
        }
        f
    }

    fn run(&self, cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
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
            .env("TZ", "UTC");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> String {
        let (out, err, code) = self.run(cwd, env, args);
        assert_eq!(code, 0, "`git {args:?}` failed: {err}");
        out
    }

    fn loose(&self, dir: &str, id: &str) -> bool {
        self.work.join(dir).join(&id[..2]).join(&id[2..]).is_file()
    }
}

#[test]
fn an_empty_object_directory_is_an_empty_object_database() {
    let f = Fixture::new("empty");
    let empty = f.work.join("empty");
    let env = [("GIT_OBJECT_DIRECTORY", empty.as_path())];
    let cases: &[(&[&str], &str, &str, i32)] = &[
        (&["cat-file", "-t", "HEAD"], "", "fatal: git cat-file: could not get object info\n", 128),
        (&["log", "-1", "--format=%s"], "", "fatal: bad object HEAD\n", 128),
        (&["status", "--porcelain"], "", "fatal: bad object HEAD\n", 128),
        (
            &["show-ref"],
            "",
            "fatal: git show-ref: bad ref refs/heads/main (14d5db65fc01e7fd0b3577571213e1725e5e53f0)\n",
            128,
        ),
    ];
    for (args, want_out, want_err, want_code) in cases {
        let (out, err, code) = f.run(&f.work, &env, args);
        assert_eq!((out.as_str(), err.as_str(), code), (*want_out, *want_err, *want_code), "git {args:?}");
    }
    // Resolving a ref opens no object.
    assert_eq!(f.ok(&f.work, &env, &["rev-parse", "HEAD"]), format!("{HEAD}\n"));
}

#[test]
fn objects_are_written_to_the_object_directory_named() {
    let f = Fixture::new("write");
    let empty = f.work.join("empty");
    let out = f.ok(&f.work, &[("GIT_OBJECT_DIRECTORY", empty.as_path())], &["hash-object", "-w", "new"]);
    assert_eq!(out, format!("{NEW_BLOB}\n"));
    assert!(f.loose("empty", NEW_BLOB), "the blob was not written to $GIT_OBJECT_DIRECTORY");
    assert!(!f.loose(".git/objects", NEW_BLOB), "the blob was written to .git/objects");
}

#[test]
fn a_relative_object_directory_is_opened_from_the_top_of_the_work_tree() {
    let f = Fixture::new("relative");
    let deep = f.work.join("sub/deep");
    let out = f.ok(&deep, &[("GIT_OBJECT_DIRECTORY", Path::new("x"))], &["hash-object", "-w", "../../new"]);
    assert_eq!(out, format!("{NEW_BLOB}\n"));
    assert!(f.loose("x", NEW_BLOB), "a discovered work tree resolves the value from its top");
    assert!(!f.loose("sub/deep/x", NEW_BLOB));
}

#[test]
fn a_relative_object_directory_with_an_explicit_git_dir_is_opened_from_the_current_directory() {
    let f = Fixture::new("explicit");
    let deep = f.work.join("sub/deep");
    let git_dir = f.work.join(".git");
    let env = [
        ("GIT_DIR", git_dir.as_path()),
        ("GIT_OBJECT_DIRECTORY", Path::new("x")),
    ];
    // `$GIT_DIR` alone makes the current directory the work tree, so setup never
    // leaves it.
    let out = f.ok(&deep, &env, &["hash-object", "-w", "../../file"]);
    assert_eq!(out, format!("{FILE_BLOB}\n"));
    assert!(f.loose("sub/deep/x", FILE_BLOB));
    assert!(!f.loose("x", FILE_BLOB));
}
