//! `git show --line-prefix` prefixed blobs, trees and tag headers.
//!
//! `cmd_show()` writes those objects straight to stdout: a blob through
//! `stream_blob_to_fd()`, a tree as `fprintf("tree %s\n\n")` plus one line
//! per entry, a tag as `fprintf("tag %s\n")` then `show_tag_object()`'s
//! tagger line and `fwrite()` of the message, and the `putchar('\n')`
//! separator ahead of a tree or tag (builtin/log.c:614-639, 708-743).
//! `--line-prefix` is applied only by `diff_line_prefix()` and `show_log()`,
//! i.e. to a commit's record, so none of those lines carries it. zvcs applied
//! the prefix to the whole page.
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
    /// One commit holding `d/f`, tagged `v1` (annotated, on the commit) and
    /// `vt` (annotated, on the tree `HEAD:d`).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-show-line-prefix-objects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("d/f"), "one\ntwo\n").unwrap();
        f.run(&["add", "d/f"]);
        f.run(&["commit", "-q", "-m", "subject"]);
        f.run(&["tag", "-a", "-m", "on commit", "v1"]);
        f.run(&["tag", "-a", "-m", "on tree", "vt", "HEAD:d"]);
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

    fn show(&self, extra: &[&str]) -> String {
        let mut args = vec!["show", "--line-prefix=> "];
        args.extend_from_slice(extra);
        let (out, err, code) = self.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{extra:?}");
        out
    }
}

#[test]
fn blobs_and_trees_are_not_prefixed() {
    let f = Fixture::new("blob-tree");
    assert_eq!(f.show(&["HEAD:d/f"]), "one\ntwo\n");
    assert_eq!(f.show(&["HEAD:d", "HEAD:"]), "tree HEAD:d\n\nf\n\ntree HEAD:\n\nd/\n");
}

#[test]
fn only_the_commit_record_is_prefixed() {
    let f = Fixture::new("mixed");
    assert_eq!(
        f.show(&["--format=%s", "-s", "HEAD:d", "HEAD"]),
        "tree HEAD:d\n\nf\n> subject\n"
    );
    assert_eq!(
        f.show(&["--format=%s", "-s", "v1"]),
        "tag v1\nTagger: C O Mitter <committer@example.com>\n\non commit\n> subject\n"
    );
    assert_eq!(
        f.show(&["vt"]),
        "tag vt\nTagger: C O Mitter <committer@example.com>\nDate:   Tue Nov 14 22:13:20 2023 +0000\n\non tree\n\ntree vt\n\nf\n"
    );
}
