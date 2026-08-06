//! `show -z`, the `rev-parse` format queries, and a cherry-pick that used to abort.
//!
//! `-z` is `diffopt.line_termination = 0`: the record terminator and the field
//! separator become NUL and the paths stop going through `write_name_quoted()`. It
//! reaches the commit header too, and — for the combined record a merge produces —
//! the separator that follows it, while an ordinary commit keeps the blank line.
//!
//! `--show-object-format` and `--show-ref-format` are what a client reads to learn
//! how the repository is stored; both are answered here, and an unknown mode is
//! git's own fatal rather than an echoed argument.
//!
//! The cherry-pick is the case where one side renamed a file and the other modified
//! it to the same content: the merge reaches a pair whose ids and modes both match,
//! which an assertion used to call impossible.
//!
//! Expectations measured against stock git.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `master` renames the file while `feature` edits it, then `master` merges.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-nulrec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git(&["init", "-q", "-b", "master", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);
        f.git(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(f.repo.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        f.git(&["commit", "-qam", "on feature"]);
        f.git(&["checkout", "-q", "master"]);
        f.git(&["mv", "a.txt", "renamed.txt"]);
        f.git(&["commit", "-qam", "rename a"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }

    fn stdout(&self, args: &[&str]) -> Vec<u8> {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        out.stdout
    }
}

/// `-z` NUL-terminates the header and every name record, and stops quoting paths.
#[test]
fn show_z_terminates_records_with_nul() {
    let f = Fixture::new("showz");
    let head = String::from_utf8(f.stdout(&["rev-parse", "HEAD~1"])).unwrap();
    let head = head.trim_end().to_owned();

    // An ordinary commit: `<id>\0` then the blank line, then `R100\0a.txt\0renamed.txt\0`.
    let got = f.stdout(&["show", "--name-status", "-M", "-z", "--format=%H", &head]);
    let want = format!("{head}\0\nR100\0a.txt\0renamed.txt\0");
    assert_eq!(got, want.as_bytes(), "{:?}", String::from_utf8_lossy(&got));

    // `--name-only` drops the status column but keeps the NULs.
    let got = f.stdout(&["show", "--name-only", "-z", "--format=%H", &head]);
    assert_eq!(got, format!("{head}\0\nrenamed.txt\0").as_bytes());

    // Without `-z` the same records are tab-separated and newline-terminated.
    let got = f.stdout(&["show", "--name-status", "-M", "--format=%H", &head]);
    assert_eq!(got, format!("{head}\n\nR100\ta.txt\trenamed.txt\n").as_bytes());
}

/// A merge's name record is the combined one, and `-z` makes its separator a NUL too.
#[test]
fn a_merge_shows_a_combined_name_record() {
    let f = Fixture::new("mergez");
    let head = String::from_utf8(f.stdout(&["rev-parse", "HEAD"])).unwrap();
    let head = head.trim_end().to_owned();

    // One status letter per parent: modified against the first, added against the
    // second (which has the file only under its old name).
    let got = f.stdout(&["show", "--no-renames", "--name-status", "--format=%H", &head]);
    assert_eq!(got, format!("{head}\n\nMA\trenamed.txt\n").as_bytes());

    // Under `-z` the separator after the header is a NUL, unlike the ordinary
    // single-parent record above, which keeps its blank line.
    let got = f.stdout(&["show", "--no-renames", "--name-status", "-z", "--format=%H", &head]);
    assert_eq!(got, format!("{head}\0\0MA\0renamed.txt\0").as_bytes());
}

/// How the repository stores objects and refs.
#[test]
fn format_queries_answer_and_reject() {
    let f = Fixture::new("formats");

    assert_eq!(f.stdout(&["rev-parse", "--show-object-format"]), b"sha1\n");
    for mode in ["storage", "input", "output"] {
        let arg = format!("--show-object-format={mode}");
        assert_eq!(f.stdout(&["rev-parse", &arg]), b"sha1\n", "{mode}");
    }
    assert_eq!(f.stdout(&["rev-parse", "--show-ref-format"]), b"files\n");

    // A mode git does not have is a fatal, not an echoed argument.
    let out = f.run(&["rev-parse", "--show-object-format=compat"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: unknown mode for --show-object-format: compat\n"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// Cherry-picking a change the branch already carries: both sides hold the same blob
/// under different names, which is a resolution rather than a conflict.
#[test]
fn cherry_picking_an_already_merged_change_succeeds() {
    let f = Fixture::new("cherry");
    let out = f.run(&["cherry-pick", "--allow-empty", "--no-commit", "feature"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // The worktree still holds the merged content under the renamed path.
    assert_eq!(
        std::fs::read_to_string(f.repo.join("renamed.txt")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert!(!f.repo.join("a.txt").exists(), "the rename is not undone");
}
