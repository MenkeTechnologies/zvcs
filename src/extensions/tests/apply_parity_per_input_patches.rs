//! `git apply <patch>...` handles each patch file on its own.
//!
//! `apply_all_patches()` (apply.c:5092-5190) runs a separate `apply_patch()` per
//! argument: it parses, checks, writes the worktree and stages into the in-core
//! index before the next file is opened. A failing input stops the run there
//! (`goto end`), leaving what earlier inputs wrote in the worktree and never
//! writing the index; the whitespace tally and its squelch count the whole run,
//! and the index is written once at the end. zvcs concatenated every input into
//! one patch, so a later failure undid the earlier inputs, an earlier failure
//! still let later inputs be checked, and `--reject` reported them out of order.
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
    /// `a` committed; next to the worktree, a patch creating `q`, one deleting a
    /// `nope` that does not exist, and two that add lines with trailing blanks.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-apply-per-input-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "1\n2\n3\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        let patch = |name: &str, body: &str| std::fs::write(f.root.join(name), body).unwrap();
        patch(
            "newq.diff",
            "diff --git a/q b/q\nnew file mode 100644\n--- /dev/null\n+++ b/q\n@@ -0,0 +1 @@\n+q\n",
        );
        patch(
            "delnope.diff",
            "diff --git a/nope b/nope\ndeleted file mode 100644\n--- a/nope\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n",
        );
        patch("ws.diff", "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,4 @@\n 1\n-2\n+two \n+x \n 3\n");
        patch(
            "ws2.diff",
            "diff --git a/w b/w\nnew file mode 100644\n--- /dev/null\n+++ b/w\n@@ -0,0 +1,4 @@\n+a \n+b \n+c \n+d \n",
        );
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
}

#[test]
fn a_later_failure_keeps_earlier_worktree_writes_but_not_the_index() {
    let f = Fixture::new("later");
    assert_eq!(
        f.run(&["apply", "--index", "../newq.diff", "../delnope.diff"]),
        (String::new(), "error: nope: does not exist in index\n".to_owned(), 1)
    );
    assert_eq!(std::fs::read_to_string(f.work.join("q")).unwrap(), "q\n");
    assert_eq!(f.run(&["status", "--short"]).0, "?? q\n");
}

#[test]
fn an_earlier_failure_stops_before_the_next_input() {
    let f = Fixture::new("earlier");
    assert_eq!(
        f.run(&["apply", "--check", "-v", "../delnope.diff", "../newq.diff"]),
        (
            String::new(),
            "Checking patch nope...\nerror: nope: No such file or directory\n".to_owned(),
            1
        )
    );
    // `--reject` checks and writes one input before it looks at the next.
    assert_eq!(
        f.run(&["apply", "--reject", "../newq.diff", "../newq.diff"]),
        (
            String::new(),
            "Checking patch q...\nApplied patch q cleanly.\nChecking patch q...\n\
             error: q: already exists in working directory\n"
                .to_owned(),
            1
        )
    );
}

#[test]
fn whitespace_is_squelched_over_the_whole_run() {
    let f = Fixture::new("ws");
    let (out, err, code) = f.run(&["apply", "--index", "../ws.diff", "../ws2.diff"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "../ws.diff:7: trailing whitespace.\ntwo \n../ws.diff:8: trailing whitespace.\nx \n\
             ../ws2.diff:6: trailing whitespace.\na \n../ws2.diff:7: trailing whitespace.\nb \n\
             ../ws2.diff:8: trailing whitespace.\nc \n\
             warning: squelched 1 whitespace error\nwarning: 6 lines add whitespace errors.\n",
            0
        )
    );
    assert_eq!(f.run(&["status", "--short"]).0, "M  a\nA  w\n");
}
