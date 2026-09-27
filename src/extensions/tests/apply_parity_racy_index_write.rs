//! `apply --index` writes the index through `do_write_index()`, racy smudge included.
//!
//! `apply_all_patches()` ends with `write_locked_index(state->repo->index,
//! &state->lock_file, COMMIT_LOCK)` (apply.c:5174), and every such write smudges the
//! racy entries whose content moved (`do_write_index()`, read-cache.c:2902-2903).
//! zvcs's `apply` serialised the index directly, so an entry the patch never touched —
//! racily modified in the same second as the last index write — was written back
//! with its old size under a newer index timestamp, and from then on read as clean.
//! The race is forced by stamping the file and `.git/index` with the same past second.
//!
//! Expectations measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PAST: &str = "202009131226.40";

const PATCH: &str = "\
diff --git a/c b/c
new file mode 100644
--- /dev/null
+++ b/c
@@ -0,0 +1 @@
+c
";

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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-apply-racy-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        // The rewrite below moves ctime; keep it out of the stat comparison.
        f.git(&["config", "core.trustctime", "false"]);
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
            .env("LC_ALL", "C");
        c
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = self.cmd(args).output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().expect("no signal"))
    }

    fn git(&self, args: &[&str]) -> String {
        let (out, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` failed");
        out
    }

    fn stamp(&self, path: &str) {
        let out = Command::new("touch").args(["-t", PAST, path]).current_dir(&self.work).output().unwrap();
        assert!(out.status.success(), "touch failed: {out:?}");
    }

    fn recorded_size(&self, path: &str) -> String {
        let out = self.git(&["ls-files", "--debug", path]);
        let line = out.lines().find(|l| l.trim_start().starts_with("size:")).expect("size line");
        line.split_whitespace().nth(1).unwrap().to_owned()
    }
}

#[test]
fn an_untouched_racily_modified_entry_is_smudged_by_the_apply_write() {
    let f = Fixture::new();
    std::fs::write(f.work.join("a"), "a\n").unwrap();
    f.stamp("a");
    f.git(&["add", "a"]);
    std::fs::write(f.work.join("a"), "x\n").unwrap();
    f.stamp("a");
    f.stamp(".git/index");
    std::fs::write(f.root.join("c.patch"), PATCH).unwrap();

    let patch = f.root.join("c.patch");
    f.git(&["apply", "--index", patch.to_str().unwrap()]);

    assert_eq!(f.recorded_size("a"), "0");
    assert_eq!(f.git(&["diff-files", "--name-only"]), "a\n");
    assert_eq!(f.git(&["ls-files", "--stage", "c"]).split_whitespace().nth(3), Some("c"));
}
