//! `git commit`'s editor is told which index the commit is being built from.
//!
//! ```c
//! strvec_pushf(&env, "GIT_INDEX_FILE=%s", index_file);
//! if (launch_editor(git_path_commit_editmsg(), NULL, env.v)) {
//! ```
//!
//! (builtin/commit.c:1119-1127.) `index_file` is what `prepare_index()` returned:
//! `.git/index` for a plain commit, the absolute `index.lock` it holds for `-a`
//! (builtin/commit.c:443-451), and the absolute `next-index-<pid>.lock` for a
//! partial commit (builtin/commit.c:541-554) — each already written, so a `git`
//! the editor runs sees the index being committed.
//!
//! zvcs gave the editor no `GIT_INDEX_FILE` at all.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Records the index it was given and what is staged in it, relative to HEAD.
const EDITOR: &str = "f() { echo \"$GIT_INDEX_FILE\" >&2; git diff --cached --name-only >&2; echo msg >\"$1\"; }; f";

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
        let root = std::env::temp_dir().join(format!("zvcs-commit-editor-index-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&f.work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        std::fs::write(f.work.join("sub/g"), "a\n").unwrap();
        f.git(&f.work, &["add", "."]);
        f.git(&f.work, &["commit", "-q", "-m", "base"]);
        f
    }

    /// stderr with the work tree spelled `<W>` and the pid in `next-index-<pid>`
    /// spelled `PID`, and the status.
    fn git(&self, dir: &Path, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("PATH", format!("{}:/usr/bin:/bin", Path::new(BIN).parent().unwrap().display()))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("GIT_EDITOR", EDITOR)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).replace(self.work.to_str().unwrap(), "<W>");
        let err = match (err.find("next-index-"), err.find(".lock")) {
            (Some(a), Some(b)) if a < b => format!("{}next-index-PID{}", &err[..a], &err[b..]),
            _ => err,
        };
        (err, out.status.code().expect("no signal"))
    }
}

#[test]
fn a_plain_commit_names_the_index_as_setup_spells_it() {
    let f = Fixture::new("plain");
    std::fs::write(f.work.join("sub/f"), "b\n").unwrap();
    f.git(&f.work, &["add", "sub/f"]);
    assert_eq!(f.git(&f.work.join("sub"), &["commit", "-q"]), (".git/index\nsub/f\n".into(), 0));
}

#[test]
fn commit_a_names_the_held_index_lock() {
    let f = Fixture::new("all");
    std::fs::write(f.work.join("sub/f"), "b\n").unwrap();
    assert_eq!(
        f.git(&f.work.join("sub"), &["commit", "-q", "-a"]),
        ("<W>/.git/index.lock\nsub/f\n".into(), 0)
    );
    assert!(!f.work.join(".git/index.lock").exists());
}

#[test]
fn a_partial_commit_names_the_next_index_lock() {
    let f = Fixture::new("partial");
    std::fs::write(f.work.join("sub/f"), "b\n").unwrap();
    std::fs::write(f.work.join("sub/g"), "b\n").unwrap();
    f.git(&f.work, &["add", "sub/g"]);
    assert_eq!(
        f.git(&f.work.join("sub"), &["commit", "-q", "f"]),
        ("<W>/.git/next-index-PID.lock\nsub/f\n".into(), 0)
    );
}
