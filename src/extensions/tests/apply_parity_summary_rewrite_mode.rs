//! `git apply --summary` lines for renames with a mode change, and rewrites.
//!
//! `summary_patch_list()` (apply.c:4382-4405) prints a rename or copy line and
//! then `show_mode_change(p, 0)` — ` mode change <old> => <new>` with no path —
//! and for a patch carrying a score but no rename (a `-B` rewrite,
//! `dissimilarity index N%`, which `gitdiff_dissimilarity()` stores in
//! `patch->score` when it is at most 100, apply.c:1097-1105) ` rewrite <path>
//! (N%)` followed by the same nameless mode line. zvcs dropped the mode change
//! after a rename and never printed a rewrite line.
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
            .join(format!("zvcs-apply-summary-rewrite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
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
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn summary(&self, name: &str, body: &str) -> (String, String, i32) {
        std::fs::write(self.root.join(name), body).unwrap();
        self.run(&["apply", "--summary", &format!("../{name}")])
    }
}

#[test]
fn a_rename_is_followed_by_its_nameless_mode_change() {
    let f = Fixture::new("rename");
    let body = "diff --git a/m b/m2\nold mode 100644\nnew mode 100755\nsimilarity index 87%\n\
                rename from m\nrename to m2\n";
    assert_eq!(
        f.summary("ren.diff", body),
        (" rename m => m2 (87%)\n mode change 100644 => 100755\n".to_owned(), String::new(), 0)
    );
}

#[test]
fn a_dissimilarity_score_is_a_rewrite_line() {
    let f = Fixture::new("rewrite");
    let with_mode = "diff --git a/big b/big\nold mode 100644\nnew mode 100755\ndissimilarity index 90%\n";
    assert_eq!(
        f.summary("d90.diff", with_mode),
        (" rewrite big (90%)\n mode change 100644 => 100755\n".to_owned(), String::new(), 0)
    );
    let hunk = "diff --git a/big b/big\ndissimilarity index 80%\n--- a/big\n+++ b/big\n@@ -1 +1 @@\n-1\n+one\n";
    assert_eq!(f.summary("d80.diff", hunk), (" rewrite big (80%)\n".to_owned(), String::new(), 0));
    // Over 100 the score is not stored, so it is a plain named mode change.
    let over = "diff --git a/big b/big\ndissimilarity index 150%\nold mode 100644\nnew mode 100755\n";
    assert_eq!(
        f.summary("d150.diff", over),
        (" mode change 100644 => 100755 big\n".to_owned(), String::new(), 0)
    );
}
