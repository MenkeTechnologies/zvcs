//! File operands of `commit-tree -F` and `interpret-trailers` are opened from the work tree top.
//!
//! Neither builtin runs its operand through `prefix_filename()`, and `setup_git_directory()` has
//! already changed into the top of the work tree, so from a subdirectory a relative operand
//! names a file below the top. zvcs opened it relative to the directory it was started in, and
//! reported a file that is right there as missing. Expectations measured from stock git 2.56.0.

use std::path::PathBuf;
use std::process::{Command, Output};

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
        let root = std::env::temp_dir().join(format!("zvcs-top-operands-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("msg.txt"), "from the top\n").unwrap();
        std::fs::write(root.join("p.patch"), "Subject: x\n\nbody\n").unwrap();
        std::fs::write(root.join("src/p.patch"), "Subject: nested\n\nbody\n").unwrap();
        let f = Fixture { root };
        f.run(".", &["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, cwd: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.root.join(cwd))
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
            .unwrap()
    }
}

#[test]
fn commit_tree_message_file_is_read_from_the_top() {
    let f = Fixture::new("commit-tree");
    let tree = f.run(".", &["hash-object", "-t", "tree", "-w", "--stdin"]);
    let tree = String::from_utf8(tree.stdout).unwrap();
    let tree = tree.trim();
    let out = f.run("src", &["commit-tree", "-F", "msg.txt", tree]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let id = String::from_utf8(out.stdout).unwrap();
    let body = f.run(".", &["cat-file", "commit", id.trim()]);
    assert!(String::from_utf8(body.stdout).unwrap().ends_with("\nfrom the top\n"));
}

#[test]
fn interpret_trailers_files_are_read_and_edited_from_the_top() {
    let f = Fixture::new("trailers");
    let out = f.run("src", &["interpret-trailers", "--trailer", "Acked-by: X", "p.patch"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Subject: x\n\nbody\n\nAcked-by: X\n");
    // The file next to the working directory is not the one named.
    let out = f.run("src", &["interpret-trailers", "--in-place", "--trailer", "Acked-by: X", "p.patch"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(f.root.join("p.patch")).unwrap(), "Subject: x\n\nbody\n\nAcked-by: X\n");
    assert_eq!(std::fs::read_to_string(f.root.join("src/p.patch")).unwrap(), "Subject: nested\n\nbody\n");
}
