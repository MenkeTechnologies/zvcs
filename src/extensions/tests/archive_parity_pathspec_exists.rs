//! `git archive <tree> <path>` where every match is `export-ignore`d.
//!
//! `path_exists()` decides whether a spec "did not match any files" with a
//! plain `read_tree(..., reject_entry, ...)` over the archived tree
//! (archive.c:397-412, 434-452): any file the spec reaches, or a directory it
//! matches as a whole, is a match. Attributes are not consulted there — only
//! `write_archive_entry()` and `queue_or_write_archive_entry()` apply
//! `export-ignore` (archive.c:173-178, 272-281). So a spec naming only ignored
//! paths is accepted and produces an archive without them.
//!
//! zvcs tested existence against the archive walk itself, which had already
//! dropped the ignored entries, and died with `pathspec '<spec>' did not match
//! any files` and exit 128.
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
    /// `a.txt` and `d/s.txt` in one commit, with `info_attributes` written to
    /// `.git/info/attributes`.
    fn new(tag: &str, info_attributes: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-archive-pathspec-exists-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a.txt"), "a\n").unwrap();
        std::fs::write(f.work.join("d/s.txt"), "s\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join(".git/info/attributes"), info_attributes).unwrap();
        f
    }

    /// `archive -v -o <root>/out <args>`, and the size of what it wrote.
    fn archive(&self, args: &[&str]) -> (String, String, i32, Option<u64>) {
        let out = self.root.join("out");
        let _ = std::fs::remove_file(&out);
        let mut all = vec!["archive", "-v", "-o", out.to_str().unwrap()];
        all.extend_from_slice(args);
        let (stdout, stderr, code) = self.run(&all);
        let size = std::fs::metadata(&out).ok().map(|m| m.len());
        (stdout, stderr, code, size)
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
fn an_ignored_directory_still_matches_its_spec() {
    let f = Fixture::new("dir", "d export-ignore\n");
    // Nothing is written but the (commit-carrying) empty tar.
    assert_eq!(f.archive(&["HEAD", "d"]), (String::new(), String::new(), 0, Some(10240)));
    // A file below the ignored directory is found by the tree walk as well.
    assert_eq!(f.archive(&["HEAD", "d/s.txt"]), (String::new(), String::new(), 0, Some(10240)));
    // The zip container: an end-of-central-directory record and its comment.
    assert_eq!(
        f.archive(&["--format=zip", "HEAD", "d/s.txt"]),
        (String::new(), String::new(), 0, Some(62))
    );
}

#[test]
fn ignored_files_still_match_their_specs() {
    let f = Fixture::new("file", "a.txt export-ignore\nd/s.txt export-ignore\n");
    // The directory record is flushed before its only child is dropped.
    assert_eq!(
        f.archive(&["HEAD", "a.txt", "d/s.txt"]),
        (String::new(), "d/\n".into(), 0, Some(10240))
    );
    // A spec that reaches nothing in the tree is still a miss; `-o` was
    // already created, empty, by `create_output_file()`.
    assert_eq!(
        f.archive(&["HEAD", "a.txt", "nothere"]),
        (String::new(), "fatal: pathspec 'nothere' did not match any files\n".into(), 128, Some(0))
    );
}
