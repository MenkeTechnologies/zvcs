//! `git show` on a clean merge whose combined path set is empty.
//!
//! `diff_tree_combined()` (combine-diff.c) writes the raw block, `needsep` and the
//! patch separator only when the combined diff has a surviving path. A clean merge
//! has none, so `--raw -p` or `--stat -p` ends at the message (or the stat) with
//! no extra blank line; an evil merge still gets the separator.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// `main` and `side` both fork from `base`; `merge` joins them cleanly.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-show-empty-combined-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "side"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("c"), "c\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "main"]);
        f.run(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
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

#[test]
fn no_separator_after_the_message_when_nothing_is_combined() {
    let f = Fixture::new("raw");
    for args in [
        &["show", "--format=%s", "--raw", "-p"][..],
        &["show", "--format=%s", "--raw", "--unified=0"][..],
        &["show", "--format=%s", "--quiet", "--raw", "--unified=0", "--find-renames"][..],
        &["show", "--format=%s", "--quiet", "--patch-with-raw"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("merge\n\n", "", 0), "{args:?}");
    }
}

#[test]
fn no_separator_after_the_count_formats_when_nothing_is_combined() {
    let f = Fixture::new("stat");
    let (out, err, code) = f.run(&["show", "--format=%s", "--numstat", "-p"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("merge\n\n1\t0\tb\n", "", 0));
    let (out, err, code) = f.run(&["show", "--format=%s", "--stat", "-p"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("merge\n\n b | 1 +\n 1 file changed, 1 insertion(+)\n", "", 0)
    );
}

#[test]
fn a_surviving_path_still_gets_the_separator() {
    let f = Fixture::new("evil");
    std::fs::write(f.work.join("b"), "b2\n").unwrap();
    f.run(&["commit", "-q", "-a", "--amend", "-m", "merge"]);
    let (out, err, code) = f.run(&["show", "--format=%s", "--raw", "-p"]);
    assert_eq!(err, "");
    assert_eq!(code, 0);
    let (raw, patch) = out.split_once("\n\n").unwrap().1.split_once("\n\n").expect("separator before patch");
    assert!(raw.starts_with("::"), "{raw}");
    assert!(patch.starts_with("diff --cc b"), "{patch}");
}
