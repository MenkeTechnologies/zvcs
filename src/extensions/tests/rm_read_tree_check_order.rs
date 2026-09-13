//! Which refusal `rm` and `read-tree` report when more than one applies.
//!
//! * `rm` parses every pathspec (builtin/rm.c:280-282) before it checks
//!   `--pathspec-file-nul` (:291-293) and before `setup_work_tree()` (:298-299),
//!   so in a bare repository a bad element is reported as a pathspec error rather
//!   than as the missing work tree.
//! * `read-tree` reads the index and refuses an unmerged one (builtin/read-tree.c:202-204)
//!   before it resolves a single tree-ish (:208-216); `--reset` reads past it.
//!
//! Every expectation here was taken from stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
        let root = std::env::temp_dir().join(format!("zvcs-rmrtorder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        // getcwd() is symlink-resolved (`/var` is `/private/var` on macOS), and
        // the bare-repository hint is built from it.
        let root = std::fs::canonicalize(&root).unwrap();
        Fixture { root }
    }

    fn run(&self, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.root.join("home"))
            .env("ZVCS_HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("PWD")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn binary");
        child.stdin.take().unwrap().write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        child.wait_with_output().expect("run binary")
    }

    fn fails(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.run(cwd, args, None);
        assert_eq!(o.status.code(), Some(128), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    fn bare(&self) -> PathBuf {
        let bare = self.root.join("bare.git");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(self.run(&bare, &["init", "-q", "--bare"], None).status.success());
        bare
    }

    /// One commit holding `a.txt`, then `a.txt` replaced in the index by stages 1 and 2.
    fn unmerged_worktree(&self) -> PathBuf {
        let work = self.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.co"],
            &["config", "user.name", "t"],
        ] {
            assert!(self.run(&work, args, None).status.success());
        }
        std::fs::write(work.join("a.txt"), "a\n").unwrap();
        assert!(self.run(&work, &["add", "a.txt"], None).status.success());
        assert!(self.run(&work, &["commit", "-q", "-m", "one"], None).status.success());
        let blob = self.run(&work, &["rev-parse", "HEAD:a.txt"], None);
        let blob = String::from_utf8(blob.stdout).unwrap();
        let blob = blob.trim();
        let info = format!(
            "0 {zero}\ta.txt\n100644 {blob} 1\ta.txt\n100644 {blob} 2\ta.txt\n",
            zero = "0".repeat(blob.len())
        );
        assert!(self.run(&work, &["update-index", "--index-info"], Some(&info)).status.success());
        let staged = self.run(&work, &["ls-files", "-u"], None);
        assert_eq!(String::from_utf8_lossy(&staged.stdout).lines().count(), 2);
        work
    }
}

#[test]
fn rm_in_bare_repository_reports_the_pathspec_before_the_work_tree() {
    let fx = Fixture::new("bare");
    let bare = fx.bare();
    let hint = format!("{}/.", bare.display());

    for args in [&["rm", "../x"][..], &["rm", "--cached", "../x"]] {
        assert_eq!(
            fx.fails(&bare, args),
            format!("fatal: ../x: '../x' is outside repository at '{hint}'\n"),
            "{args:?}"
        );
    }
    // An absolute element is always outside when there is no work tree
    // (`abspath_part_inside_repo()` returns -1, setup.c:56-60).
    let abs = format!("{}/x", bare.display());
    assert_eq!(
        fx.fails(&bare, &["rm", &abs]),
        format!("fatal: {abs}: '{abs}' is outside repository at '{hint}'\n")
    );
    assert_eq!(
        fx.fails(&bare, &["rm", ":(bogus)x"]),
        "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n"
    );
    // The argv pathspec is parsed before the --pathspec-from-file conflict is.
    assert_eq!(
        fx.fails(&bare, &["rm", "--pathspec-from-file=/dev/null", ":(bogus)x"]),
        "fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'\n"
    );
    // A clean pathspec still reaches setup_work_tree().
    assert_eq!(fx.fails(&bare, &["rm", "x"]), "fatal: this operation must be run in a work tree\n");
}

#[test]
fn rm_pathspec_file_nul_without_a_file_is_refused_before_anything_is_removed() {
    let fx = Fixture::new("nul");
    let work = fx.unmerged_worktree();
    std::fs::write(work.join("b.txt"), "b\n").unwrap();
    assert!(fx.run(&work, &["add", "b.txt"], None).status.success());

    assert_eq!(
        fx.fails(&work, &["rm", "--cached", "--pathspec-file-nul", "b.txt"]),
        "fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'\n"
    );
    let listed = fx.run(&work, &["ls-files", "b.txt"], None);
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "b.txt\n");
}

#[test]
fn read_tree_refuses_an_unmerged_index_before_resolving_trees() {
    let fx = Fixture::new("unmerged");
    let work = fx.unmerged_worktree();
    const RESOLVE: &str = "fatal: You need to resolve your current index first\n";

    for args in [
        &["read-tree", "-m", "nope"][..],
        &["read-tree", "--prefix=p/", "nope"],
        &["read-tree", "-m", "-u", "-i", "HEAD"],
        &["read-tree", "-m"],
    ] {
        assert_eq!(fx.fails(&work, args), RESOLVE, "{args:?}");
    }
    // `--reset` is excluded from the refusal, so the tree-ish is what fails.
    assert_eq!(fx.fails(&work, &["read-tree", "--reset", "nope"]), "fatal: Not a valid object name nope\n");
}
