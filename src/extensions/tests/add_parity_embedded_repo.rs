//! `git add` of an embedded repository: how `-v`/`-n` name it, and what `-N`
//! does with one whose HEAD is unborn.
//!
//! An embedded repository reaches `add_files()` as a directory entry, carrying
//! the trailing `/` `treat_directory()` gives it, and `read_directory()` sorts
//! the entries by that name. `add_to_index()` reports the name verbatim —
//! `printf("add '%s'\n", path)` (read-cache.c:805-806) — so the report reads
//! `add 'emb/'` and sorts after `emb-1` and `emb.txt`.
//!
//! `-N` takes `add_to_index()`'s `intent_only` arm, which never calls
//! `index_path()` (read-cache.c:775-781): the embedded repository's HEAD is not
//! resolved, so an unborn one is not the `does not have a commit checked out`
//! failure, and the gitlink carries the empty blob
//! `set_object_name_for_intent_to_add_entry()` gives every intent-to-add entry.
//! `check_embedded_repo()` still warns (builtin/add.c:318-340, :368).
//!
//! zvcs printed `add 'emb'`, and `add -N` over a repository without a commit
//! died `fatal: adding files failed` without touching the index.
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
            .join(format!("zvcs-add-embedded-repo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    /// An embedded repository at `name`, with a commit when `born`.
    fn embed(&self, name: &str, born: bool) {
        self.git(&["init", "-q", name]);
        if born {
            self.git(&["-C", name, "commit", "-q", "--allow-empty", "-m", "e"]);
        }
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
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
fn verbose_and_dry_run_name_the_repository_with_its_slash_and_sort_by_it() {
    let f = Fixture::new("verbose");
    f.embed("emb", true);
    std::fs::write(f.work.join("emb.txt"), "x\n").unwrap();
    std::fs::write(f.work.join("emb-1"), "y\n").unwrap();
    let expected = "add 'emb-1'\nadd 'emb.txt'\nadd 'emb/'\n";
    let (out, _, code) = f.git(&["-c", "advice.addEmbeddedRepo=false", "add", "-n", "."]);
    assert_eq!((out.as_str(), code), (expected, 0), "-n");
    let (out, _, code) = f.git(&["-c", "advice.addEmbeddedRepo=false", "add", "-v", "emb", "emb.txt", "emb-1"]);
    assert_eq!((out.as_str(), code), (expected, 0), "-v");
}

#[test]
fn intent_to_add_records_a_repository_without_a_commit() {
    let f = Fixture::new("intent");
    f.embed("emb2", false);
    let (out, err, code) = f.git(&["add", "-N", "emb2"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(err.starts_with("warning: adding embedded git repository: emb2\n"), "{err}");
    assert!(!err.contains("does not have a commit checked out"), "{err}");
    let (ls, _, _) = f.git(&["ls-files", "-s"]);
    assert_eq!(
        ls,
        "100644 78981922613b2afb6025042ff6bd878ac1994e85 0\ta\n\
         160000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\temb2\n"
    );
}
