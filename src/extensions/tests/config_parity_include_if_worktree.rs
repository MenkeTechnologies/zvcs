//! `includeIf "worktree:…"` and `"worktree/i:…"`, new in git 2.56.
//!
//! `include_condition_is_true()` (config.c:403-408) runs the same
//! `include_by_path()` as `gitdir:` against `repo_get_work_tree()`: the pattern
//! is `~`-expanded, `**/`-prefixed unless absolute or `./`-relative to the
//! including file, `**`-completed after a trailing slash, and wildmatched with
//! `WM_PATHNAME` against the work tree's realpath and then its absolute path. A
//! repository without a work tree — bare, or declared bare by `core.bare` — never
//! matches. The work tree is the one setup settles on, so `core.worktree`
//! retargets the condition.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// `r/` (with `sub/`) and its linked worktree `linked/`, a bare `bare.git/`,
    /// and a global config whose conditional includes key on work trees.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-include-if-worktree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        std::fs::write(f.root.join("inc.cfg"), "[x]\n\tv = inc\n").unwrap();
        std::fs::write(f.root.join("inc2.cfg"), "[y]\n\tv = inc2\n").unwrap();
        let d = f.root.display();
        std::fs::write(
            f.root.join("glob.cfg"),
            format!(
                "[includeIf \"worktree:**/r\"]\n\tpath = {d}/inc.cfg\n\
                 [includeIf \"worktree/i:**/LINKED\"]\n\tpath = {d}/inc2.cfg\n\
                 [includeIf \"worktree:{d}/r/sub/\"]\n\tpath = {d}/inc2.cfg\n"
            ),
        )
        .unwrap();
        f.run(&f.root, &["init", "-q", "-b", "main", "r"]);
        f.run(&f.root.join("r"), &["commit", "-q", "--allow-empty", "-m", "i"]);
        f.run(&f.root.join("r"), &["worktree", "add", "-q", "../linked"]);
        f.run(&f.root, &["init", "-q", "--bare", "bare.git"]);
        std::fs::create_dir_all(f.root.join("r/sub")).unwrap();
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", self.root.join("glob.cfg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// `git config <key>` from `dir`: the value, or `None` when unset.
    fn get(&self, dir: &str, key: &str) -> Option<String> {
        let (out, err, code) = self.run(&self.root.join(dir), &["config", key]);
        assert!(code == 0 || code == 1, "{dir} {key}: {err}");
        (code == 0).then(|| out.trim_end().to_owned())
    }
}

#[test]
fn the_condition_matches_the_work_tree_not_the_current_directory() {
    let f = Fixture::new("match");
    assert_eq!(f.get("r", "x.v").as_deref(), Some("inc"));
    // The work tree is still `r` from inside it, so `r/sub/` (which only matches
    // paths below `r/sub`) does not hold.
    assert_eq!(f.get("r/sub", "x.v").as_deref(), Some("inc"));
    assert_eq!(f.get("r/sub", "y.v"), None);
    // `worktree/i:` folds case; a linked worktree is its own work tree.
    assert_eq!(f.get("linked", "y.v").as_deref(), Some("inc2"));
    assert_eq!(f.get("linked", "x.v"), None);
    // No work tree, no match: a bare repository, or the git directory itself.
    assert_eq!(f.get("bare.git", "x.v"), None);
    assert_eq!(f.get("r/.git", "x.v"), None);
}

#[test]
fn the_work_tree_is_the_one_setup_settles_on() {
    let f = Fixture::new("settled");
    let r = f.root.join("r");
    let (_, _, code) = f.run(&r, &["--work-tree=/", "config", "x.v"]);
    assert_eq!(code, 1, "--work-tree moves the work tree away from `r`");

    f.run(&r, &["config", "core.worktree", &f.root.join("linked").display().to_string()]);
    assert_eq!(f.get("r", "y.v").as_deref(), Some("inc2"));
    assert_eq!(f.get("r", "x.v"), None);
    f.run(&r, &["config", "--unset", "core.worktree"]);

    f.run(&r, &["config", "core.bare", "true"]);
    assert_eq!(f.get("r", "x.v"), None);
}

#[test]
fn a_relative_pattern_is_relative_to_the_including_file() {
    let f = Fixture::new("relative");
    let d = f.root.display();
    std::fs::write(f.root.join("glob.cfg"), format!("[includeIf \"worktree:./r\"]\n\tpath = {d}/inc.cfg\n")).unwrap();
    assert_eq!(f.get("r", "x.v").as_deref(), Some("inc"));
    assert_eq!(f.get("linked", "x.v"), None);

    // From the command line there is no file to be relative to — unless there is
    // no work tree, which answers false before the pattern is looked at.
    std::fs::write(f.root.join("glob.cfg"), "").unwrap();
    let inc = format!("includeIf.worktree:./r.path={d}/inc.cfg");
    let (out, err, code) = f.run(&f.root.join("r"), &["-c", &inc, "config", "x.v"]);
    assert_eq!((out.as_str(), code), ("inc\n", 0));
    assert_eq!(err, "error: relative config include conditionals must come from files\n");
    let (out, err, code) = f.run(&f.root.join("bare.git"), &["-c", &inc, "config", "x.v"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 1));
}
