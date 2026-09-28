//! `git apply` refuses a creation that removes lines and a deletion that keeps any.
//!
//! The tail of `parse_single_patch()` (apply.c:1967-1970) compares the patch's
//! own creation/deletion claim with its fragments' line counts: `new file %s
//! depends on old contents` when a creation has old lines, `deleted file %s
//! still has contents` when a deletion has new ones — an `error()` during the
//! parse, so exit 128 before anything is checked, written or reported. zvcs
//! parsed such a patch and failed later, at 1, as a hunk that did not apply.
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
            .join(format!("zvcs-apply-new-deleted-{tag}-{}", std::process::id()));
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

    fn apply(&self, extra: &[&str], name: &str, body: &str) -> (String, String, i32) {
        std::fs::write(self.root.join(name), body).unwrap();
        let path = format!("../{name}");
        let mut args = vec!["apply"];
        args.extend_from_slice(extra);
        args.push(&path);
        self.run(&args)
    }
}

#[test]
fn a_creation_with_old_lines_is_refused_at_parse_time() {
    let f = Fixture::new("new");
    let git = "diff --git a/q b/q\nnew file mode 100644\n--- /dev/null\n+++ b/q\n@@ -1 +1 @@\n-x\n+q\n";
    let want = (String::new(), "error: new file q depends on old contents\n".to_owned(), 128);
    assert_eq!(f.apply(&[], "git.diff", git), want);
    // Before `--stat` could report it, and before `-R` could turn it around.
    assert_eq!(f.apply(&["--stat"], "git.diff", git), want);
    assert_eq!(f.apply(&["-R"], "git.diff", git), want);
    let traditional = "--- /dev/null\n+++ b/q\n@@ -1 +1 @@\n-x\n+q\n";
    assert_eq!(f.apply(&[], "trad.diff", traditional), want);
}

#[test]
fn a_deletion_with_new_lines_is_refused_at_parse_time() {
    let f = Fixture::new("del");
    let git = "diff --git a/a b/a\ndeleted file mode 100644\n--- a/a\n+++ /dev/null\n@@ -1,3 +1,3 @@\n-1\n+one\n 2\n 3\n";
    assert_eq!(
        f.apply(&[], "git.diff", git),
        (String::new(), "error: deleted file a still has contents\n".to_owned(), 128)
    );
    assert_eq!(std::fs::read_to_string(f.work.join("a")).unwrap(), "1\n2\n3\n");
}
