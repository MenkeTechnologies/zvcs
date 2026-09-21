//! `git symbolic-ref <name> <ref>` format-checks the *target* and nothing else.
//!
//! ```c
//! case 2:
//!         if (!strcmp(argv[0], "HEAD") &&
//!             !starts_with(argv[1], "refs/"))
//!                 die("Refusing to point HEAD outside of refs/");
//!         if (check_refname_format(argv[1], REFNAME_ALLOW_ONELEVEL) < 0)
//!                 die("Refusing to set '%s' to invalid ref '%s'", argv[0], argv[1]);
//!         ret = !!refs_update_symref(get_main_ref_store(the_repository),
//!                                    argv[0], argv[1], msg);
//! ```
//!
//! (builtin/symbolic-ref.c:85-93, v2.55.0). The name reaches the ref store with
//! no `check_refname_format()` of its own, and a symbolic update carries no new
//! object id, so `ref_transaction_update()` gates it on `refname_is_safe()`
//! rather than the format check. Names like `refs/heads/bad name`,
//! `refs/heads/bad~1` and `refs/heads/..bad` are therefore written — exit 0,
//! `ref: <target>` on disk, one reflog line — even though nothing can resolve
//! them afterwards, which is why reading one back is `No such ref`.
//!
//! The port put every name through gitoxide's `FullName`, which refuses all of
//! them, and answered with gitoxide's own complaint at exit 1. `refs/heads/..bad`
//! failed a second way even once that was routed to the direct write: gix-lock
//! built the lock path with `Path::with_extension()`, whose split of a file name
//! made only of dots turned `refs/heads/..bad` into `refs/heads/..` — the parent
//! directory — so the lock reported the reference as permanently locked.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
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
        let root =
            std::env::temp_dir().join(format!("zvcs-symref-name-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "rev-parse {spec}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.work.join(".git").join(relative))
            .unwrap_or_else(|e| panic!("{relative}: {e}"))
    }
}

/// Each of these is written, silently, with the file and the reflog line stock
/// leaves behind.
#[test]
fn a_name_only_refname_is_safe_accepts_is_still_written() {
    let f = Fixture::new("written");
    let head = f.oid("HEAD");
    let zero = "0".repeat(head.len());

    for name in [
        "refs/heads/bad name",
        "refs/heads/bad~1",
        "refs/heads/bad^",
        "refs/heads/bad:",
        "refs/heads/.bad",
        "refs/heads/bad.lock",
        "refs/heads/bad@{1}",
        // The one gix-lock's `with_extension()` split turned into `refs/heads/..`.
        "refs/heads/..bad",
    ] {
        assert_eq!(
            f.run(&["symbolic-ref", name, "refs/heads/main"]),
            (String::new(), String::new(), 0),
            "symbolic-ref {name}"
        );
        assert_eq!(f.read(name), "ref: refs/heads/main\n", "{name} on disk");
        assert_eq!(
            f.read(&format!("logs/{name}")),
            format!(
                "{zero} {head} C O Mitter <committer@example.com> 1700000000 +0000\n"
            ),
            "{name} reflog"
        );
    }
}

/// Reading one back is still `No such ref`: `refs_resolve_ref_unsafe()` refuses
/// a name `check_refname_format()` will not spell, so the reference that was
/// just written cannot be resolved, shortened or deleted through this command.
#[test]
fn such_a_name_cannot_be_read_back_or_deleted() {
    let f = Fixture::new("readback");
    f.git(&["symbolic-ref", "refs/heads/bad name", "refs/heads/main"]);

    assert_eq!(
        f.run(&["symbolic-ref", "refs/heads/bad name"]),
        (String::new(), "fatal: No such ref: refs/heads/bad name\n".to_string(), 128)
    );
    assert_eq!(
        f.run(&["symbolic-ref", "-d", "refs/heads/bad name"]),
        (String::new(), "fatal: No such ref: refs/heads/bad name\n".to_string(), 128)
    );
    assert!(f.work.join(".git/refs/heads/bad name").exists(), "the file survives");
}

/// The names `refname_is_safe()` does refuse — a path that normalises to
/// something else, and a one-level name that is not all upper case — are the
/// `error: refusing to update ref with bad name` branch of
/// `ref_transaction_update()`, at exit 1 rather than 128.
#[test]
fn an_unsafe_name_is_refused_with_gits_wording_and_exit_one() {
    let f = Fixture::new("unsafe");
    for name in ["refs/heads/bad//x", "oneLEVEL", "lower"] {
        assert_eq!(
            f.run(&["symbolic-ref", name, "refs/heads/main"]),
            (
                String::new(),
                format!("error: refusing to update ref with bad name '{name}'\n"),
                1
            ),
            "symbolic-ref {name}"
        );
    }
}

/// The target keeps its own check, which outranks everything above: it is a
/// `fatal:` at 128, and it names both operands.
#[test]
fn the_target_is_the_one_operand_that_is_format_checked() {
    let f = Fixture::new("target");
    assert_eq!(
        f.run(&["symbolic-ref", "refs/heads/s", "refs/heads/..bad"]),
        (
            String::new(),
            "fatal: Refusing to set 'refs/heads/s' to invalid ref 'refs/heads/..bad'\n".to_string(),
            128
        )
    );
    assert!(!f.work.join(".git/refs/heads/s").exists());
}
