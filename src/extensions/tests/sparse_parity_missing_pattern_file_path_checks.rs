//! `mv` and `add` with `core.sparseCheckout` set but no `info/sparse-checkout` file.
//!
//! `path_in_sparse_checkout()` loads the pattern file lazily and, when that fails, answers "inside"
//! for every path, so nothing is reported as outside the definition. An *empty* file is a
//! definition that includes nothing, and still refuses. zvcs read a missing file as an empty one
//! and refused both commands. Expectations measured from stock git 2.56.0.

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
        let root = std::env::temp_dir().join(format!("zvcs-sparse-missing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["config", "core.sparseCheckout", "true"]);
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
fn a_missing_pattern_file_puts_every_path_inside() {
    let f = Fixture::new("missing");
    assert_eq!(f.run(&["mv", "a", "q"]), (String::new(), 0));
    std::fs::write(f.root.join("new"), "n\n").unwrap();
    assert_eq!(f.run(&["add", "new"]), (String::new(), 0));
}

#[test]
fn an_empty_pattern_file_includes_nothing() {
    let f = Fixture::new("empty");
    std::fs::write(f.root.join(".git/info/sparse-checkout"), "").unwrap();
    let (stderr, code) = f.run(&["mv", "a", "q"]);
    assert_eq!(code, 1);
    assert!(stderr.starts_with("The following paths and/or pathspecs matched paths that exist\n"), "{stderr}");
}
