//! `update-index --add <directory>`: which of the two refusals git prints.
//!
//! ```c
//! /* Inexact match: is there perhaps a subdirectory match? */
//! pos = -pos-1;
//! while (pos < the_repository->index->cache_nr) {
//!         const struct cache_entry *ce = the_repository->index->cache[pos++];
//!
//!         if (strncmp(ce->name, path, len))
//!                 break;
//!         if (ce->name[len] > '/')
//!                 break;
//!         if (ce->name[len] < '/')
//!                 continue;
//!
//!         /* Subdirectory match - error out */
//!         return error("%s: is a directory - add individual files instead", path);
//! }
//!
//! /* No match - should we add it as a gitlink? */
//! if (!repo_resolve_gitlink_ref(the_repository, path, "HEAD", &oid))
//!         return add_one_path(NULL, path, len, st);
//!
//! /* Error out. */
//! return error("%s: is a directory - add files inside instead", path);
//! ```
//!
//! (`process_directory()`, builtin/update-index.c:357-378.) Two different
//! messages for two different situations, and the scan that tells them apart is
//! easy to leave out because both end the command the same way. The scan starts
//! at the directory name's insertion point and runs forward over every entry
//! that still carries it as a byte prefix; only an entry whose next byte is
//! exactly `/` counts, which is why `d2-file` and `d2.file` — sorting on either
//! side of `d2/` — must not be mistaken for contents of `d2`.
//!
//! The scan also runs *before* the gitlink check, so a directory the index
//! already has files under is refused even when it is a repository in its own
//! right.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
        let root = std::env::temp_dir().join(format!("zvcs-uidir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn fails(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        assert!(!out.status.success(), "`git {args:?}` unexpectedly succeeded: {out:?}");
        (out.status.code().unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn file(&self, path: &str, body: &str) {
        let full = self.work.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
}

#[test]
fn a_directory_the_index_knows_nothing_about_says_add_files_inside() {
    let f = Fixture::new("unknown");
    f.file("d/c", "c\n");

    let (code, err) = f.fails(&["update-index", "--add", "d"]);
    assert_eq!(code, 128);
    assert_eq!(err, "error: d: is a directory - add files inside instead\nfatal: Unable to process path d\n");
}

#[test]
fn a_directory_with_tracked_contents_says_add_individual_files() {
    let f = Fixture::new("tracked");
    f.file("d/c", "c\n");
    f.git(&["update-index", "--add", "d/c"]);

    let (code, err) = f.fails(&["update-index", "--add", "d"]);
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "error: d: is a directory - add individual files instead\nfatal: Unable to process path d\n"
    );
}

/// `d2-file` sorts after `d2/` and `d2.file` before it, so a prefix test that
/// forgets to require the `/` would call either one a subdirectory match.
#[test]
fn sibling_names_around_the_slash_are_not_subdirectory_matches() {
    let f = Fixture::new("siblings");
    f.file("d2-file", "x\n");
    f.file("d2.file", "y\n");
    f.git(&["update-index", "--add", "d2-file", "d2.file"]);
    std::fs::create_dir_all(f.work.join("d2")).unwrap();
    f.file("d2/x", "x\n");

    let (code, err) = f.fails(&["update-index", "--add", "d2"]);
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "error: d2: is a directory - add files inside instead\nfatal: Unable to process path d2\n",
        "neither sibling lives inside d2"
    );

    // And once something really does, the other message takes over.
    f.git(&["update-index", "--add", "d2/x"]);
    let (_, err) = f.fails(&["update-index", "--add", "d2"]);
    assert_eq!(
        err,
        "error: d2: is a directory - add individual files instead\nfatal: Unable to process path d2\n"
    );
}
