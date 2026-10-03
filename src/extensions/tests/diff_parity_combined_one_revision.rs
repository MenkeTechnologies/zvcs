//! `git diff -c|--cc <rev>` with the working tree on the other side.
//!
//! `run_diff_index()` walks the tree against the index, and `show_modified()`
//! takes every path the tree, the index and the file all hold out of the queue
//! when a combined mode is set:
//!
//! ```c
//! if (revs->combine_merges && !cached &&
//!     (!oideq(oid, &old_entry->oid) || !oideq(&old_entry->oid, &new_entry->oid))) {
//!         ...
//!         show_combined_diff(p, 2, revs);
//! ```
//! (`diff-lib.c:408-424`, v2.56.0)
//!
//! The index entry is parent 0 and the tree entry parent 1; the result is the
//! file. `show_combined_diff()` prints on the spot — patch, or raw/name records,
//! and nothing for the stat formats — so the section comes ahead of everything
//! `diff_flush()` makes of the remaining pairs, and those pairs alone decide
//! `--exit-code`. The port printed an ordinary patch and refused `-c` outright.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.
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
    /// `f` is `a` in `HEAD`, `a b` in the index and `a b c` on disk; `n` is a new
    /// file staged as `n`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-cc-onerev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("f", "a\n");
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.write("f", "a\nb\n");
        f.git(&["add", "f"]);
        f.write("f", "a\nb\nc\n");
        f.write("n", "n\n");
        f.git(&["add", "n"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@e")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_EXTERNAL_DIFF")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn git(&self, args: &[&str]) {
        let (_, err, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}`: {err}");
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

const COMBINED_BODY: &str = "index 422c2b7,7898192..0000000
--- a/f
+++ b/f
@@@ -1,2 -1,1 +1,3 @@@
  a
 +b
++c
";

const NEW_FILE: &str = "diff --git a/n b/n
new file mode 100644
index 0000000..8ba3a16
--- /dev/null
+++ b/n
@@ -0,0 +1 @@
+n
";

#[test]
fn combined_section_comes_first_in_either_density() {
    let f = Fixture::new("patch");
    for (flag, head) in [
        ("--cc", "diff --cc f\n"),
        ("-c", "diff --combined f\n"),
        ("--diff-merges=dense-combined", "diff --cc f\n"),
        ("--diff-merges=combined", "diff --combined f\n"),
    ] {
        assert_eq!(f.ok(&["diff", flag, "HEAD"]), format!("{head}{COMBINED_BODY}{NEW_FILE}"), "{flag}");
    }
    // `--no-diff-merges` takes the mode off again.
    assert!(f.ok(&["diff", "--cc", "--no-diff-merges", "HEAD"]).starts_with("diff --git a/f b/f\n"));
}

#[test]
fn other_formats_and_exit_code() {
    let f = Fixture::new("formats");
    assert_eq!(
        f.ok(&["diff", "-c", "HEAD", "--raw"]),
        "::100644 100644 100644 422c2b7 7898192 0000000 MM\tf\n:000000 100644 0000000 8ba3a16 A\tn\n"
    );
    assert_eq!(f.ok(&["diff", "--cc", "HEAD", "--name-status"]), "MM\tf\nA\tn\n");
    // The stat formats see only the queue.
    assert_eq!(f.ok(&["diff", "--cc", "HEAD", "--stat"]), " n | 1 +\n 1 file changed, 1 insertion(+)\n");
    // So does `--exit-code`: with `n` unstaged again only the combined path is left.
    f.git(&["rm", "-q", "--cached", "n"]);
    let (out, err, code) = f.run(&["diff", "--cc", "--exit-code", "HEAD"]);
    assert_eq!((out, err.as_str(), code), (format!("diff --cc f\n{COMBINED_BODY}"), "", 0));
    // `--cached` never reads the worktree, so the mode has nothing to combine.
    assert!(f.ok(&["diff", "--cc", "--cached", "HEAD"]).starts_with("diff --git a/f b/f\n"));
}
