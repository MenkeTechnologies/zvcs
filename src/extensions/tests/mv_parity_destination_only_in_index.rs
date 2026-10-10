//! `git mv` asks the file system, not the index, whether the destination exists.
//!
//! The "destination exists" refusal is `lstat(dst) == 0`. A destination that is tracked but gone
//! from disk (deleted, or outside the sparse-checkout definition) is no obstacle: the move goes
//! on, and a sparse one is then reported by the sparse gate (exit 1 with the advice block).
//! zvcs also refused when the index merely listed the destination. Under `--sparse` the sparse
//! entry itself is the obstacle, spelled `destination exists in the index`. Expectations measured
//! from stock git 2.56.0.

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
        let root = std::env::temp_dir().join(format!("zvcs-mv-dst-index-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (path, text) in [("README.md", "r\n"), ("inside/x", "i\n"), ("outside/drop.txt", "o\n"), ("b", "b\n")] {
            std::fs::write(f.root.join(path), text).unwrap();
        }
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "one"]);
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
fn a_tracked_destination_missing_from_disk_is_replaced() {
    let f = Fixture::new("deleted");
    std::fs::remove_file(f.root.join("b")).unwrap();
    assert_eq!(f.run(&["mv", "README.md", "b"]), (String::new(), 0));
    assert!(f.root.join("b").is_file());
}

#[test]
fn a_sparse_destination_is_left_to_the_sparse_gate() {
    let f = Fixture::new("sparse");
    f.run(&["sparse-checkout", "set", "inside"]);
    let (stderr, code) = f.run(&["mv", "README.md", "outside/drop.txt"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("The following paths and/or pathspecs matched paths that exist\n"), "{stderr}");
    assert_eq!(
        f.run(&["mv", "--sparse", "README.md", "outside/drop.txt"]),
        ("fatal: destination exists in the index, source=README.md, destination=outside/drop.txt\n".to_string(), 128)
    );
}
