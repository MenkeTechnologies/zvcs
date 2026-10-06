//! `git shortlog <rev> <path>`: a positional that is not a revision starts the
//! pathspec when no `--` is on the line.
//!
//! `setup_revisions()` (revision.c:3117-3132) hands an operand that
//! `handle_revision_arg()` refuses to `verify_filename()`; when the operand names
//! a file, it and every positional after it become the prune data, each of the
//! later ones checked with `diagnose_misspelt_rev` off, so a missing path, or a
//! revision, after it is `no such path in the working tree`, and a pseudo-option
//! after it is `must come before non-option arguments`. zvcs died on the first
//! path with git's `ambiguous argument` block.
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
    /// `base` adds `a` and `b`; `a2` changes `a` alone.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-shortlog-pathspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a"), "a2\n").unwrap();
        f.run(&["commit", "-q", "-am", "a2"]);
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
fn a_path_after_the_revision_limits_the_walk() {
    let f = Fixture::new("limit");
    assert_eq!(f.run(&["shortlog", "-s", "HEAD", "a"]), ("     2\tA U Thor\n".into(), String::new(), 0));
    assert_eq!(f.run(&["shortlog", "-s", "HEAD", "b"]), ("     1\tA U Thor\n".into(), String::new(), 0));
    assert_eq!(f.run(&["shortlog", "-s", "HEAD~1..HEAD", "b", "a"]), ("     1\tA U Thor\n".into(), String::new(), 0));
}

#[test]
fn the_tail_after_the_first_path_must_be_paths() {
    let f = Fixture::new("tail");
    let missing = |name: &str| {
        format!(
            "fatal: {name}: no such path in the working tree.\n\
             Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n"
        )
    };
    assert_eq!(f.run(&["shortlog", "-s", "HEAD", "a", "nosuch"]), (String::new(), missing("nosuch"), 128));
    assert_eq!(f.run(&["shortlog", "-s", "HEAD", "a", "HEAD~1"]), (String::new(), missing("HEAD~1"), 128));
    assert_eq!(
        f.run(&["shortlog", "-s", "a", "--all"]),
        (String::new(), "fatal: option '--all' must come before non-option arguments\n".into(), 128)
    );
}
