//! `git rebase` dies on a bad `--attr-source` while checking for a clean work tree.
//!
//! `require_clean_work_tree()` refreshes the index, and a racily clean entry is read
//! back through `ce_compare_data()`, whose first attribute lookup dies with
//! `bad --attr-source or GIT_ATTR_SOURCE`. zvcs judged the tree dirty from its own comparison and
//! printed the unstaged-changes refusal instead (exit 1 where git ends at 128).
//! Expectations measured from stock git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rebase-attr-source-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["branch", "base"]);
        std::fs::write(f.root.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "two"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

#[test]
fn a_racily_clean_entry_reaches_the_attribute_lookup_and_dies() {
    let f = Fixture::new("dirty");
    // Same size, written after the index: stat data alone cannot clear it, so the content is compared.
    std::fs::write(f.root.join("a"), "x\n").unwrap();
    assert_eq!(
        f.run(&["--attr-source=does-not-exist", "rebase", "base"]),
        ("fatal: bad --attr-source or GIT_ATTR_SOURCE\n".to_string(), 128)
    );
    // Without the bad source the same tree is the plain refusal.
    assert_eq!(
        f.run(&["rebase", "base"]),
        (
            "error: cannot rebase: You have unstaged changes.\nerror: Please commit or stash them.\n".to_string(),
            1
        )
    );
}
