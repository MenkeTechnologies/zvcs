//! `rev-list --count --objects` with a marked count was not refused.
//!
//! ```c
//! if (revs.count &&
//!     (revs.tag_objects || revs.tree_objects || revs.blob_objects) &&
//!     (revs.left_right || revs.cherry_mark))
//!         die(_("marked counting and '%s' cannot be used together"), "--objects");
//! ```
//!
//! (builtin/rev-list.c:907-910.) zvcs printed a `<left>\t<right>` count with the
//! objects folded into the right column.
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
            .join(format!("zvcs-rev-list-marked-count-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
        f.run(&["branch", "side"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "B"]);
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
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

#[test]
fn a_marked_count_refuses_objects() {
    let f = Fixture::new("marked");
    let want = (
        String::new(),
        "fatal: marked counting and '--objects' cannot be used together\n".to_string(),
        128,
    );
    for mark in ["--left-right", "--cherry-mark", "--cherry"] {
        assert_eq!(f.run(&["rev-list", "--count", "--objects", mark, "main...side"]), want, "{mark}");
    }
    // Unmarked, the objects are counted.
    assert_eq!(f.run(&["rev-list", "--count", "--objects", "main...side"]).0, "3\n");
}
