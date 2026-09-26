//! `git worktree move --[no-]relative-paths`, and `worktree.useRelativePaths` for `move`.
//!
//! `move_worktree()` declares `OPT_BOOL(0, "relative-paths", &use_relative_paths, …)`
//! (builtin/worktree.c:1253) over the value `git_worktree_config()` read from
//! `worktree.useRelativePaths` (:141-142), and hands it to
//! `update_worktree_location()` (worktree.c:432-457), which rewrites both
//! `<wt>/.git` and `worktrees/<id>/gitdir` through
//! `write_worktree_linking_files()` — relative (and `extensions.relativeWorktrees`
//! set, format 1) or absolute, whichever was asked for *now*. zvcs refused the
//! option as unknown and always wrote absolute links.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-worktree-move-relative-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
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
}

impl Fixture {
    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.work.join(rel)).unwrap()
    }

    fn links(&self, wt: &str) -> (String, String) {
        (self.read(&format!("{wt}/.git")), self.read(".git/worktrees/a/gitdir"))
    }

    fn abs(&self, rel: &str) -> String {
        std::fs::canonicalize(self.work.join(rel)).unwrap().display().to_string()
    }
}

#[test]
fn relative_paths_rewrites_both_links_relative() {
    let f = Fixture::new("flag");
    assert_eq!(f.run(&["worktree", "add", "-q", "--detach", "../a"]).2, 0);
    let (out, err, code) = f.run(&["worktree", "move", "--relative-paths", "../a", "../b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    // The admin dir is .git/worktrees/a: four levels below the common parent.
    assert_eq!(
        f.links("../b"),
        ("gitdir: ../repo/.git/worktrees/a\n".to_string(), "../../../../b/.git\n".to_string())
    );
    assert_eq!(f.run(&["config", "extensions.relativeWorktrees"]).0, "true\n");
    assert_eq!(f.run(&["config", "core.repositoryformatversion"]).0, "1\n");
    assert_eq!(f.run(&["-C", "../b", "rev-parse", "--git-dir"]).0, format!("{}\n", f.abs(".git/worktrees/a")));

    // Without the option the links go back to absolute.
    assert_eq!(f.run(&["worktree", "move", "../b", "../c"]).2, 0);
    assert_eq!(
        f.links("../c"),
        (
            format!("gitdir: {}\n", f.abs(".git/worktrees/a")),
            format!("{}/.git\n", f.abs("../c"))
        )
    );
}

#[test]
fn the_config_is_the_default_and_the_flag_overrides_it() {
    let f = Fixture::new("config");
    assert_eq!(f.run(&["worktree", "add", "-q", "--detach", "../a"]).2, 0);
    f.run(&["config", "worktree.useRelativePaths", "true"]);
    std::fs::create_dir(f.root.join("sub")).unwrap();
    // Into an existing directory: the destination is sub/a.
    assert_eq!(f.run(&["worktree", "move", "../a", "../sub"]), (String::new(), String::new(), 0));
    assert_eq!(
        f.links("../sub/a"),
        ("gitdir: ../../repo/.git/worktrees/a\n".to_string(), "../../../../sub/a/.git\n".to_string())
    );

    assert_eq!(f.run(&["worktree", "move", "--no-relative-paths", "../sub/a", "../e"]).2, 0);
    assert_eq!(
        f.links("../e"),
        (
            format!("gitdir: {}\n", f.abs(".git/worktrees/a")),
            format!("{}/.git\n", f.abs("../e"))
        )
    );
    assert_eq!(f.run(&["-C", "../e", "status", "--porcelain"]), (String::new(), String::new(), 0));
}
