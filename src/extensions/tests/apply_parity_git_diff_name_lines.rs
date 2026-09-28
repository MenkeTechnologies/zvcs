//! `git apply` holds a `diff --git` header's `---`/`+++` lines to the header.
//!
//! `gitdiff_newfile()`/`gitdiff_delete()` (apply.c:1027-1045) give the side
//! that exists the `diff --git` name, and `gitdiff_verify_name()`
//! (apply.c:929-974) then checks each name line: a side declared absent must
//! be `/dev/null`, a side already named must be named the same again, and a
//! side named nowhere yet takes the line's name through `find_name()` — which
//! knows nothing of `/dev/null`, so `-p1` turns it into `dev/null`. zvcs took
//! the lines at face value, so a contradictory header applied (or failed on the
//! file) instead of being refused at 128.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
        let root = std::env::temp_dir()
            .join(format!("zvcs-apply-git-diff-names-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "1\n2\n3\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f
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
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `git apply ../<name>` over `body`.
    fn apply(&self, name: &str, body: &str) -> (String, String, i32) {
        std::fs::write(self.root.join(name), body).unwrap();
        self.run(&["apply", &format!("../{name}")])
    }
}

#[test]
fn a_side_declared_absent_must_be_dev_null() {
    let f = Fixture::new("absent");
    let new = "diff --git a/a b/a\nnew file mode 100644\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-1\n+one\n";
    assert_eq!(
        f.apply("new.diff", new),
        (String::new(), "error: git apply: bad git-diff - expected /dev/null at ../new.diff:3\n".into(), 128)
    );
    let del = "diff --git a/a b/a\ndeleted file mode 100644\n--- a/a\n+++ b/a\n@@ -1 +0,0 @@\n-1\n";
    assert_eq!(
        f.apply("del.diff", del),
        (String::new(), "error: git apply: bad git-diff - expected /dev/null at ../del.diff:4\n".into(), 128)
    );
    assert_eq!(std::fs::read_to_string(f.work.join("a")).unwrap(), "1\n2\n3\n");
}

#[test]
fn a_named_side_must_keep_its_name() {
    let f = Fixture::new("named");
    let del = "diff --git a/a b/a\ndeleted file mode 100644\n--- a/b\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-1\n-2\n-3\n";
    assert_eq!(
        f.apply("del.diff", del),
        (
            String::new(),
            "error: git apply: bad git-diff - inconsistent old filename at ../del.diff:3\n".into(),
            128
        )
    );
    let rename = "diff --git a/a b/q\nrename from a\nrename to q\n--- a/a\n+++ b/zz\n";
    assert_eq!(
        f.apply("ren.diff", rename),
        (
            String::new(),
            "error: git apply: bad git-diff - inconsistent new filename at ../ren.diff:5\n".into(),
            128
        )
    );
}

#[test]
fn an_undeclared_dev_null_is_just_a_path() {
    let f = Fixture::new("devnull");
    let body = "diff --git a/a b/a\n--- /dev/null\n+++ b/a\n@@ -0,0 +1 @@\n+q\n";
    assert_eq!(
        f.apply("dn.diff", body),
        (String::new(), "error: dev/null: No such file or directory\n".into(), 1)
    );
}
