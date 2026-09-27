//! `includeIf "gitdir:…"` inside a linked worktree.
//!
//! `repo_read_config()` sets `opts.git_dir = repo_get_git_dir(repo)`
//! (config.c:1685) and `include_by_gitdir()` (:238-295) matches the condition
//! against that directory — a linked worktree's own
//! `<common>/worktrees/<name>`, not the common directory the shared config
//! lives in. So from the worktree `gitdir:<common>/worktrees/wt` holds and
//! `gitdir:<common>` (no trailing slash) does not. gitoxide handed its
//! condition context the common directory, which inverted both.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// `r/` with a linked worktree `wt/`, and two conditional includes in the
    /// shared config: one keyed on the common directory, one on the worktree's.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-include-if-gitdir-worktree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        let r = f.root.join("r");
        std::fs::write(f.root.join("c.cfg"), "[x]\n\ty = common\n").unwrap();
        std::fs::write(f.root.join("p.cfg"), "[x]\n\ty = private\n").unwrap();
        f.run(&r, &["init", "-q", "-b", "main", "."]);
        std::fs::write(r.join("a"), "a\n").unwrap();
        f.run(&r, &["add", "a"]);
        f.run(&r, &["commit", "-q", "-m", "i"]);
        let common = r.join(".git");
        f.run(
            &r,
            &[
                "config",
                &format!("includeIf.gitdir:{}.path", common.display()),
                &f.root.join("c.cfg").display().to_string(),
            ],
        );
        f.run(
            &r,
            &[
                "config",
                &format!("includeIf.gitdir:{}.path", common.join("worktrees/wt").display()),
                &f.root.join("p.cfg").display().to_string(),
            ],
        );
        f.run(&r, &["worktree", "add", "-q", "../wt"]);
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C")
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
fn the_worktree_matches_its_own_git_directory() {
    let f = Fixture::new("match");
    let get = ["config", "--get-all", "x.y"];
    assert_eq!(f.run(&f.root.join("r"), &get), ("common\n".into(), String::new(), 0));
    assert_eq!(f.run(&f.root.join("wt"), &get), ("private\n".into(), String::new(), 0));
    // The same through a command-line override, which rebuilds the snapshot.
    assert_eq!(
        f.run(&f.root.join("wt"), &["-c", "core.abbrev=9", "config", "--get-all", "x.y"]),
        ("private\n".into(), String::new(), 0)
    );
}
