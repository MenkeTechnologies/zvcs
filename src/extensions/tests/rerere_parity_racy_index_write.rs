//! `rerere` with `rerere.autoUpdate` stages a replayed resolution through
//! `write_locked_index()`, racy smudge included.
//!
//! `update_paths()` ends with `write_locked_index(the_repository->index, &index_lock,
//! COMMIT_LOCK)` (rerere.c:723), and every such write smudges the racy entries whose
//! content moved (`do_write_index()`, read-cache.c:2902-2903). An entry the replay never
//! touched — racily modified in the same second as the last index write — must come out
//! with its recorded size zeroed, or the newer index timestamp makes its stale stat read
//! as clean. The race is forced by stamping the file and `.git/index` with the same past
//! second.
//!
//! Expectations measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PAST: &str = "202009131226.40";

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
        let root = std::env::temp_dir().join(format!("zvcs-rerere-racy-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "rerere.enabled", "true"]);
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
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
            .env("LC_ALL", "C");
        c
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn git(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` failed: {err}");
        out
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.work.join(path), content).unwrap();
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
fn an_untouched_racily_modified_entry_is_smudged_by_the_autoupdate_write() {
    let f = Fixture::new();
    f.write("f", "base\n");
    f.write("z", "z\n");
    f.stamp("z");
    f.git(&["add", "f", "z"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("f", "side\n");
    f.git(&["commit", "-q", "-a", "-m", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("f", "main\n");
    f.git(&["commit", "-q", "-a", "-m", "main"]);

    // Record a resolution, then throw the merge away.
    assert_eq!(f.run(&["merge", "-q", "side"]).2, 1);
    f.write("f", "resolved\n");
    f.git(&["add", "f"]);
    f.git(&["commit", "-q", "-m", "merged"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    // Conflict again without rerere, so the plain `rerere` below does the replay.
    assert_eq!(f.run(&["-c", "rerere.enabled=false", "merge", "-q", "side"]).2, 1);
    f.write("z", "y\n");
    f.stamp("z");
    f.stamp(".git/index");

    let (_, err, code) = f.run(&["-c", "rerere.autoUpdate=true", "rerere"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "Staged 'f' using previous resolution.\n");

    assert_eq!(f.recorded_size("z"), "0");
    assert_eq!(f.git(&["diff-files", "--name-only"]), "z\n");
    assert_eq!(
        f.git(&["ls-files", "--stage", "f"]),
        "100644 2ab19ae607aabda796309682e0448237aab03047 0\tf\n"
    );
}
