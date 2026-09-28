//! `log --no-commit-id` and `log --always`.
//!
//! `revs->no_commit_id` (revision.c:2635-2636) keeps `log_tree_diff_flush()`
//! from calling `show_log()` (log-tree.c:939), so a commit's diff comes out on
//! its own; `opt->loginfo` is never cleared, `log_tree_diff()` reports nothing
//! shown, and `log_tree_commit()` then prints the header *after* the diff while
//! `always_show_header` holds (log-tree.c:1189-1194) — which a pickaxe clears
//! (builtin/log.c:333-335), leaving the diffs alone. `--always` sets
//! `always_show_header` (revision.c:2637-2638), which `git log` already has.
//! zvcs refused both options as unsupported.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-no-commit-id-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
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
            .env("GIT_PAGER", "cat")
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

fn two(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f
}

const PATCH_TWO: &str = "diff --git a/a b/a
index 5626abf..f719efd 100644
--- a/a
+++ b/a
@@ -1 +1 @@
-one
+two
";

const PATCH_ONE: &str = "diff --git a/a b/a
new file mode 100644
index 0000000..5626abf
--- /dev/null
+++ b/a
@@ -0,0 +1 @@
+one
";

fn ok(out: String) -> (String, String, i32) {
    (out, String::new(), 0)
}

#[test]
fn the_header_follows_the_diff() {
    let f = two("after");
    assert_eq!(
        f.run(&["log", "--no-commit-id", "-p", "--format=tformat:%s", "main"]),
        ok(format!("{PATCH_TWO}two\n{PATCH_ONE}one\n"))
    );
    assert_eq!(
        f.run(&["log", "--no-commit-id", "--raw", "--format=%s", "main"]),
        ok(":100644 100644 5626abf f719efd M\ta\ntwo\n:000000 100644 0000000 5626abf A\ta\none\n".to_string())
    );
    // `show_log()`'s separator belongs to the second header, not the diff.
    let (out, _, code) = f.run(&["log", "--no-commit-id", "-p", "main"]);
    assert_eq!(code, 0);
    let two = f.rev("main");
    let one = f.rev("main~1");
    assert!(out.starts_with(&format!("{PATCH_TWO}commit {two}\n")), "{out}");
    assert!(out.contains(&format!("\n    two\n{PATCH_ONE}\ncommit {one}\n")), "{out}");
}

#[test]
fn a_pickaxe_leaves_the_diff_alone() {
    let f = two("pickaxe");
    assert_eq!(f.run(&["log", "--no-commit-id", "-p", "-S", "two", "main"]), ok(PATCH_TWO.to_string()));
}

#[test]
fn without_a_diff_the_header_is_ordinary() {
    let f = two("plain");
    assert_eq!(f.run(&["log", "--no-commit-id", "--format=%s", "main"]), ok("two\none\n".to_string()));
    assert_eq!(f.run(&["log", "--always", "--format=%s", "main"]), ok("two\none\n".to_string()));
}
