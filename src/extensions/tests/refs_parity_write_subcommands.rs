//! `git refs create`/`delete`/`update`/`rename`, new in git 2.56
//! (builtin/refs.c:190-375, v2.56.0).
//!
//! `create` and `update` are `refs_update_ref(…, UPDATE_REFS_MSG_ON_ERR)`, so a
//! refused write is `error: update_ref failed for ref …` and exit 1 — where
//! `update-ref` dies with 128. `delete` is `refs_delete_ref()`, `update-ref -d`'s
//! own call. `rename` is `refs_rename_ref()`: the ref and its reflog move, and
//! nothing else does — `HEAD` keeps naming the old branch. Every expectation was
//! measured against stock git 2.56.0 on this exact fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const ONE: &str = "57ef6b409b72f867c799347c562551b1a76536d6";
const TWO: &str = "635eb4d81d8cc414e6b86e5ff6d78a6d22d97f78";
const ID: &str = "A <a@x> 1700000000 +0000";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` at two empty commits, `HEAD` on it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-refs-write-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "one"],
            &["commit", "-q", "--allow-empty", "-m", "two"],
        ] {
            assert!(f.run(args).status.success(), "{args:?}");
        }
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap()
    }

    /// Exit code and stderr; stdout must be empty for every write.
    fn err(&self, args: &[&str]) -> (i32, String) {
        let out = self.run(args);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{args:?}");
        (out.status.code().unwrap(), String::from_utf8(out.stderr).unwrap())
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root.join(".git").join(path)).unwrap_or_default()
    }
}

#[test]
fn create_writes_a_new_ref_and_reports_refusals_as_errors() {
    let f = Fixture::new("create");
    assert_eq!(f.err(&["refs", "create", "--message=made", "refs/heads/c", "HEAD~1"]), (0, String::new()));
    assert_eq!(f.read("refs/heads/c"), format!("{ONE}\n"));
    assert_eq!(f.read("logs/refs/heads/c"), format!("{}{ONE} {ID}\tmade\n", "0".repeat(40) + " "));

    assert_eq!(
        f.err(&["refs", "create", "refs/heads/c", "HEAD"]),
        (
            1,
            "error: update_ref failed for ref 'refs/heads/c': cannot lock ref 'refs/heads/c': reference already exists\n"
                .into()
        )
    );
    assert_eq!(
        f.err(&["refs", "create", "refs/heads/c/d", "HEAD"]),
        (
            1,
            "error: update_ref failed for ref 'refs/heads/c/d': cannot lock ref 'refs/heads/c/d': 'refs/heads/c' exists; cannot create 'refs/heads/c/d'\n"
                .into()
        )
    );
    assert_eq!(
        f.err(&["refs", "create", "refs/heads/z", &"0".repeat(40)]),
        (128, "fatal: cannot create reference with null new object ID\n".into())
    );
    assert_eq!(
        f.err(&["refs", "create", "refs/heads/z", "nope"]),
        (128, "fatal: invalid object ID: 'nope'\n".into())
    );
    assert_eq!(
        f.err(&["refs", "create", "refs/heads/z"]),
        (129, "usage: create requires reference name and an object ID\n".into())
    );
    assert_eq!(
        f.err(&["refs", "create", "--message=", "refs/heads/z", "HEAD"]),
        (128, "fatal: refusing to perform update with empty message\n".into())
    );
}

#[test]
fn update_checks_the_old_value_and_a_null_new_value_deletes() {
    let f = Fixture::new("update");
    assert_eq!(f.err(&["refs", "create", "refs/heads/c", "HEAD~1"]).0, 0);
    assert_eq!(
        f.err(&["refs", "update", "refs/heads/c", "HEAD", "HEAD"]),
        (
            1,
            format!(
                "error: update_ref failed for ref 'refs/heads/c': cannot lock ref 'refs/heads/c': is at {ONE} but expected {TWO}\n"
            )
        )
    );
    assert_eq!(f.err(&["refs", "update", "--message=moved", "refs/heads/c", "HEAD", "HEAD~1"]), (0, String::new()));
    assert!(f.read("logs/refs/heads/c").ends_with(&format!("{ONE} {TWO} {ID}\tmoved\n")));
    assert_eq!(f.err(&["refs", "update", "refs/heads/c", &"0".repeat(40)]), (0, String::new()));
    assert!(!f.root.join(".git/refs/heads/c").exists());
    assert_eq!(
        f.err(&["refs", "update", "refs/heads/c", "HEAD", "nope"]),
        (128, "fatal: invalid old object ID: 'nope'\n".into())
    );
    assert_eq!(
        f.err(&["refs", "update", "refs/heads/c", ""]),
        (128, "fatal: invalid new object ID: ''\n".into())
    );
}

#[test]
fn delete_is_update_ref_dash_d() {
    let f = Fixture::new("delete");
    assert_eq!(
        f.err(&["refs", "delete", "refs/heads/main", "HEAD~1"]),
        (1, format!("error: cannot lock ref 'refs/heads/main': is at {TWO} but expected {ONE}\n"))
    );
    assert_eq!(f.err(&["refs", "delete", "refs/heads/nope"]), (0, String::new()));
    assert_eq!(
        f.err(&["refs", "delete", "one"]),
        (1, "error: refusing to update ref with bad name 'one'\n".into())
    );
    assert_eq!(
        f.err(&["refs", "delete", "refs/heads/main", &"0".repeat(40)]),
        (128, "fatal: cannot delete reference with null old object ID\n".into())
    );
    assert_eq!(
        f.err(&["refs", "delete", "a", "b", "c"]),
        (129, "usage: delete requires reference name and an optional old object ID\n".into())
    );
}

#[test]
fn rename_moves_the_ref_and_its_log_but_not_head() {
    let f = Fixture::new("rename");
    assert!(f.run(&["branch", "side", "HEAD~1"]).status.success());
    assert!(f.run(&["branch", "o/p"]).status.success());

    // File to directory: the old name is out of the way before the new one is made.
    assert_eq!(f.err(&["refs", "rename", "--message=mv", "refs/heads/side", "refs/heads/side/deeper"]), (0, String::new()));
    assert_eq!(
        f.read("logs/refs/heads/side/deeper"),
        format!(
            "{} {ONE} {ID}\tbranch: Created from HEAD~1\n{ONE} {ONE} {ID}\tmv\n",
            "0".repeat(40)
        )
    );
    assert_eq!(
        f.err(&["refs", "rename", "refs/heads/main", "refs/heads/o"]),
        (1, "error: 'refs/heads/o/p' exists; cannot create 'refs/heads/o'\n".into())
    );

    // No message: the new entry has no tab. HEAD still names `main`, and its log
    // records the deletion `refs_delete_ref()` mirrored onto it.
    assert_eq!(f.err(&["refs", "rename", "refs/heads/main", "refs/tags/m"]), (0, String::new()));
    assert_eq!(f.read("HEAD"), "ref: refs/heads/main\n");
    assert!(f.read("logs/HEAD").ends_with(&format!("{TWO} {} {ID}\n", "0".repeat(40))));
    assert!(f.read("logs/refs/tags/m").ends_with(&format!("{TWO} {TWO} {ID}\n")));
    assert_eq!(f.read("refs/tags/m"), format!("{TWO}\n"));
    assert!(!f.root.join(".git/refs/heads/main").exists());

    assert_eq!(
        f.err(&["refs", "rename", "refs/heads/nope", "refs/heads/q"]),
        (128, "fatal: reference does not exist: 'refs/heads/nope'\n".into())
    );
    assert_eq!(
        f.err(&["refs", "rename", "refs/tags/m", "refs/heads/o/p"]),
        (128, "fatal: reference already exists: 'refs/heads/o/p'\n".into())
    );
    assert_eq!(f.err(&["refs", "rename", "HEAD", "refs/heads/h"]), (128, "fatal: invalid ref format: 'HEAD'\n".into()));
    assert_eq!(
        f.err(&["refs", "rename", "refs/tags/m"]),
        (129, "usage: rename requires old and new reference name\n".into())
    );
}

#[test]
fn option_tables_are_per_subcommand() {
    let f = Fixture::new("opts");
    let usage = "usage: git refs rename [--message=<reason>] <old-ref> <new-ref>\n\n    --[no-]message <reason>\n                          reason of the update\n\n";
    assert_eq!(f.err(&["refs", "rename", "--no-deref", "a", "b"]), (129, format!("error: unknown option `no-deref'\n{usage}")));
    let out = f.run(&["refs", "rename", "-h"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), usage);

    assert_eq!(f.err(&["refs", "create", "--message"]), (129, "error: option `message' requires a value\n".into()));
    assert_eq!(f.err(&["refs", "create", "--deref=x", "a", "b"]), (129, "error: option `no-no-deref' takes no value\n".into()));

    let top = f.run(&["refs", "-h"]);
    let top = String::from_utf8(top.stdout).unwrap();
    assert!(top.ends_with(
        "   or: git refs create [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value>\n\
         \x20  or: git refs delete [--message=<reason>] [--no-deref] <ref> [<old-value>]\n\
         \x20  or: git refs update [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value> [<old-value>]\n\
         \x20  or: git refs rename [--message=<reason>] <old-ref> <new-ref>\n\n"
    ));
}
