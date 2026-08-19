//! `git status` with `HEAD` detached onto a *tree* object.
//!
//! git resolves the last-resort `.gitmodules` lookup as `HEAD:.gitmodules`
//! (`config_from_gitmodules()`, submodule-config.c:798-806), and that path goes through
//! `read_object_with_reference(…, OBJ_TREE, …)`, so a `HEAD` that already is a tree is as
//! good as one pointing at a commit — and a miss is simply "no submodule configuration",
//! never an error. Reading it by peeling `HEAD` to a *commit* instead turned the whole of
//! `status` into a hard failure on such a repository, which git reports as an ordinary
//! detached state.
//!
//! Expectations are stock git 2.55.0's, measured on the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitsystem"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A one-commit repository whose `HEAD` file has been rewritten to the root tree's id.
/// `checkout` refuses a non-commit, so the detach is done by writing the file, which is
/// exactly the state git tolerates in `status`.
fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-status-tree-head-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let fx = Fixture { repo, home };
    fx.ok(&["init", "-q", "-b", "main", "."]);
    fx.ok(&["config", "user.email", "t@e.co"]);
    fx.ok(&["config", "user.name", "t"]);
    std::fs::write(fx.repo.join("a.txt"), "a\n").unwrap();
    fx.ok(&["add", "a.txt"]);
    fx.ok(&["commit", "-q", "-m", "one"]);

    let tree = fx.ok(&["rev-parse", "HEAD^{tree}"]).trim().to_string();
    assert_eq!(tree.len(), 40, "expected a full tree id, got {tree:?}");
    std::fs::write(fx.repo.join(".git").join("HEAD"), format!("{tree}\n")).unwrap();
    fx
}

fn git_dir_head(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".git").join("HEAD")).unwrap()
}

#[test]
fn long_status_reports_a_tree_head_as_detached() {
    let fx = fixture("long");
    let out = fx.run(&["status"]);
    assert!(
        out.status.success(),
        "status on a tree HEAD must not fail; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Not currently on any branch.\nnothing to commit, working tree clean\n"
    );
    // The command must not have "fixed" HEAD on the way past.
    assert_eq!(git_dir_head(&fx.repo).len(), 41);
}

#[test]
fn porcelain_and_short_status_read_a_tree_head() {
    let fx = fixture("porcelain");
    std::fs::write(fx.repo.join("b.txt"), "b\n").unwrap();

    assert_eq!(fx.ok(&["status", "--porcelain"]), "?? b.txt\n");
    assert_eq!(fx.ok(&["status", "-sb"]), "## HEAD (no branch)\n?? b.txt\n");
}

#[test]
fn a_worktree_change_under_a_tree_head_is_still_reported() {
    let fx = fixture("modified");
    std::fs::write(fx.repo.join("a.txt"), "a\nmodified\n").unwrap();

    assert_eq!(fx.ok(&["status", "--porcelain"]), " M a.txt\n");
    let long = fx.ok(&["status"]);
    assert!(
        long.starts_with("Not currently on any branch.\n"),
        "unexpected header: {long}"
    );
    assert!(long.contains("\tmodified:   a.txt\n"), "unexpected body: {long}");
}
