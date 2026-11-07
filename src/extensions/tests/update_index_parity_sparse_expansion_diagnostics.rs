//! What `ensure_full_index()` prints when `update-index` expands a sparse index.
//!
//! `expand_index(istate, NULL)` fails `if (pl && !pl->use_cone_patterns)` and so takes
//! the else branch: `cache_tree_update(istate, 0)` over the index while it is still
//! collapsed (sparse-index.c:351-367), then the expansion, then a second
//! `cache_tree_update(istate, 0)` over the full index (:469-470). Neither carries
//! `WRITE_TREE_SILENT`, so a conflicted sparse index prints `verify_cache()`'s
//! `unmerged` lines twice on every expansion. Each sparse directory that is not
//! `CE_SKIP_WORKTREE` is warned about (:424-426), and one whose tree cannot be read is
//! `repo_parse_tree()`'s `Could not read` inside `read_tree_at()`, its entry dropped.
//! zvcs printed none of it.
//!
//! Every case runs stock git and zvcs on twin copies of one fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

impl Twin {
    fn new(tag: &str, stock: &str, setup: impl Fn(&dyn Fn(&[&str]), &Path)) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uisparse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stock_dir = root.join("stock");
        std::fs::create_dir_all(&stock_dir).unwrap();
        let run = |args: &[&str]| {
            let (_, _, _) = git(stock, &root, &stock_dir, args);
        };
        run(&["init", "-q", "-b", "main", "."]);
        setup(&run, &stock_dir);
        let status = Command::new("cp").arg("-Rp").arg(&stock_dir).arg(root.join("zvcs")).status().unwrap();
        assert!(status.success());
        Twin { root }
    }

    fn both(&self, stock: &str, args: &[&str]) -> [(String, String, i32); 2] {
        [
            git(stock, &self.root, &self.root.join("stock"), args),
            git(BIN, &self.root, &self.root.join("zvcs"), args),
        ]
    }

    fn loose_objects(&self, which: &str) -> Vec<String> {
        let mut out = Vec::new();
        for dir in std::fs::read_dir(self.root.join(which).join(".git/objects")).unwrap() {
            let dir = dir.unwrap();
            let name = dir.file_name().into_string().unwrap();
            if name.len() != 2 {
                continue;
            }
            for f in std::fs::read_dir(dir.path()).unwrap() {
                out.push(format!("{name}{}", f.unwrap().file_name().into_string().unwrap()));
            }
        }
        out.sort();
        out
    }
}

/// A cone-mode sparse index with `out/` collapsed and `c` in conflict.
fn conflicted_sparse(run: &dyn Fn(&[&str]), dir: &Path) {
    std::fs::create_dir_all(dir.join("in")).unwrap();
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("in/a"), "1\n").unwrap();
    std::fs::write(dir.join("out/b"), "1\n").unwrap();
    std::fs::write(dir.join("c"), "base\n").unwrap();
    let c = "-c";
    let auto = "maintenance.auto=false";
    run(&["add", "-A"]);
    run(&[c, auto, "commit", "-q", "-m", "base"]);
    run(&["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("c"), "side\n").unwrap();
    run(&[c, auto, "commit", "-q", "-a", "-m", "side"]);
    run(&["checkout", "-q", "main"]);
    std::fs::write(dir.join("c"), "main\n").unwrap();
    run(&[c, auto, "commit", "-q", "-a", "-m", "main"]);
    run(&["sparse-checkout", "set", "--sparse-index", "in"]);
    run(&[c, auto, "merge", "-q", "side"]);
}

#[test]
fn a_conflicted_sparse_index_reports_its_unmerged_entries_on_both_updates() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("conflict", stock, conflicted_sparse);
    let [s, z] = t.both(stock, &["-c", "advice.sparseIndexExpanded=false", "update-index", "--chmod=+x", "out/b"]);
    assert_eq!(s.1.lines().filter(|l| l.ends_with(")") && l.contains(": unmerged (")).count(), 6, "{s:?}");
    assert_eq!(z, s);
    let [s, z] = t.both(stock, &["ls-files", "-s", "--sparse"]);
    assert_eq!(z, s);
}

fn committed(run: &dyn Fn(&[&str]), dir: &Path) {
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(&["add", "-A"]);
    run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
}

#[test]
fn a_directory_entry_outside_a_sparse_checkout_is_warned_about_and_expanded() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("notsparse", stock, committed);
    let tree = "040000,4b825dc642cb6eb9a060e54bf8d69288fbee4904,tr";
    let [s, z] = t.both(stock, &["update-index", "--add", "--cacheinfo", tree]);
    assert_eq!(z, s);
    let [s, z] = t.both(stock, &["update-index", "--force-remove", "tr"]);
    assert!(s.1.contains("warning: index entry is a directory, but not sparse (00000000)"), "{s:?}");
    assert_eq!(z, s);
    // The update over the collapsed index wrote the tree that still names `tr`.
    assert_eq!(t.loose_objects("zvcs"), t.loose_objects("stock"));
    let [s, z] = t.both(stock, &["ls-files", "-s"]);
    assert_eq!(z, s);
}

#[test]
fn a_directory_entry_naming_a_missing_tree_cannot_be_read() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("missing", stock, committed);
    let tree = "040000,1111111111111111111111111111111111111111,tr";
    let [s, z] = t.both(stock, &["update-index", "--add", "--cacheinfo", tree]);
    assert_eq!(z, s);
    let [s, z] = t.both(stock, &["write-tree"]);
    assert!(s.1.contains("error: Could not read 1111111111111111111111111111111111111111"), "{s:?}");
    assert_eq!(z, s);
}
