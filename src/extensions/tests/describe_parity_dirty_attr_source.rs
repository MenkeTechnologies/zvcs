//! `git describe --dirty` refreshes the index in-process before it diffs:
//!
//! ```c
//! } else if (dirty) {
//!         ...
//!         repo_read_index(the_repository);
//!         refresh_index(the_repository->index, REFRESH_QUIET|REFRESH_UNMERGED,
//!                       NULL, NULL, NULL);
//! ```
//! (`builtin/describe.c:756-766`, v2.56.0)
//!
//! An entry whose stat no longer vouches for it is hashed by `ce_compare_data()`,
//! and that hash asks attributes first — so a `GIT_ATTR_SOURCE` naming no
//! tree-ish dies in `compute_default_attr_source()` (`attr.c:1222-1226`) with
//! `fatal: bad --attr-source or GIT_ATTR_SOURCE` and exit 128, before anything is
//! described. The port answered `<hash>` and exit 0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

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
    /// One commit of `f`, whose mtime is then moved so the index's stat data no
    /// longer matches it while its size and content still do.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-describe-attrsrc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "one\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        let file = std::fs::File::options().write(true).open(f.work.join("f")).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(3600)).unwrap();
        f
    }

    fn run(&self, attr_source: &str, args: &[&str]) -> (String, String, i32) {
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
            .env("GIT_ATTR_SOURCE", attr_source)
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

    fn git(&self, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env_remove("GIT_ATTR_SOURCE")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }
}

#[test]
fn dirty_refresh_dies_on_an_unresolvable_attr_source() {
    let f = Fixture::new("bad");
    for source in ["", "no-such-rev"] {
        let (out, err, code) = f.run(source, &["describe", "--dirty", "--always"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: bad --attr-source or GIT_ATTR_SOURCE\n", 128),
            "GIT_ATTR_SOURCE={source:?}"
        );
    }
}

/// A source that resolves is fine, and without `--dirty` nothing refreshes.
#[test]
fn a_resolvable_source_or_no_refresh_describes() {
    let f = Fixture::new("good");
    let (out, err, code) = f.run("HEAD", &["describe", "--dirty", "--always"]);
    assert_eq!((err.as_str(), code), ("", 0));
    // Only the stat moved, so the refresh finds the content unchanged: no mark.
    assert!(!out.is_empty() && !out.contains("-dirty"), "{out}");
    let (plain, err, code) = f.run("", &["describe", "--always"]);
    assert_eq!((plain, err.as_str(), code), (out, "", 0));
}
