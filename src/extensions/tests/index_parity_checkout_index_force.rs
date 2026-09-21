//! What `git checkout-index -f` is allowed to destroy on its way to writing an
//! entry.
//!
//! ```c
//! if (S_ISDIR(st.st_mode)) {
//!         /* If it is a gitlink, leave it alone! */
//!         if (S_ISGITLINK(ce->ce_mode))
//!                 return 0;
//!         …
//!         remove_subtree(&path);
//! } else if (unlink(path.buf))
//!         return error_errno("unable to unlink old '%s'", path.buf);
//! ```
//!
//! (`checkout_entry_ca()`, entry.c:557-577, with `remove_subtree()` at :60-84.)
//! The port used `remove_dir()`, which only ever removes an *empty* directory,
//! so a `path0/` holding a file survived and the create that followed failed
//! with `File exists` where git overwrites — t2000-conflict-when-checking-files-
//! out.sh's case 4.
//!
//! The leading path has the matching rule in `create_directories()`
//! (entry.c:19-58): a component that is not a directory is unlinked and remade
//! under `--force`, and `has_dirs_only_path()` tests it with `lstat()` beyond
//! `base_dir_len`, so a *symlink to* a directory does not count as one. The port
//! called `create_dir_all()`, which follows such a symlink and writes through
//! it, leaving `path3 -> path2` in place where git replaces it with a real
//! directory — that file's case 6, and t2003-checkout-cache-mkdir.sh's case 8
//! for the `--prefix` spelling, where `base_dir_len` is a byte length and not a
//! component count.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    /// An index holding the file `path0` and the file `path1/file1`.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-ciforce-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("path0"), "zero\n").unwrap();
        std::fs::create_dir(f.work.join("path1")).unwrap();
        std::fs::write(f.work.join("path1/file1"), "one\n").unwrap();
        f.git(&["update-index", "--add", "path0", "path1/file1"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn is_real_dir(&self, rel: &str) -> bool {
        std::fs::symlink_metadata(self.work.join(rel))
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    fn is_real_file(&self, rel: &str) -> bool {
        std::fs::symlink_metadata(self.work.join(rel))
            .map(|m| m.is_file())
            .unwrap_or(false)
    }
}

/// t2000's cases 3 and 4: without `-f` the D/F conflict is reported and nothing
/// is written; with it the non-empty directory is removed and the file appears.
#[test]
fn index_parity_checkout_index_force_replaces_a_nonempty_directory() {
    let f = Fixture::new("dconflict");
    std::fs::remove_file(f.work.join("path0")).unwrap();
    std::fs::remove_dir_all(f.work.join("path1")).unwrap();
    // `path0` is now a directory with something in it, `path1` a plain file —
    // both the wrong type for their index entries.
    std::fs::create_dir(f.work.join("path0")).unwrap();
    std::fs::write(f.work.join("path0/file0"), "in the way\n").unwrap();
    std::fs::write(f.work.join("path1"), "in the way\n").unwrap();

    let (err, code) = f.run(&["checkout-index", "-a"]);
    assert_ne!(code, 0, "the conflicting checkout succeeded: {err}");
    assert!(
        err.contains("path0 already exists, no checkout"),
        "missing the refusal: {err}"
    );
    assert!(f.is_real_dir("path0"), "a refused checkout still wrote");

    let (err, code) = f.run(&["checkout-index", "-f", "-a"]);
    assert_eq!(code, 0, "checkout-index -f failed: {err}");
    assert!(f.is_real_file("path0"), "the directory was not removed");
    assert!(f.is_real_dir("path1"), "the file was not removed");
    assert!(f.is_real_file("path1/file1"));
}

/// t2000's case 6: a symlink standing where a *leading* directory belongs is
/// replaced, not followed.
#[test]
fn index_parity_checkout_index_force_replaces_a_symlinked_leading_path() {
    let f = Fixture::new("leading");
    std::fs::remove_dir_all(f.work.join("path1")).unwrap();
    std::fs::create_dir(f.work.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink("elsewhere", f.work.join("path1")).unwrap();

    let (err, code) = f.run(&["checkout-index", "-f", "-a"]);
    assert_eq!(code, 0, "checkout-index -f failed: {err}");
    assert!(f.is_real_dir("path1"), "the leading symlink was followed");
    assert!(f.is_real_file("path1/file1"));
    assert!(
        !f.work.join("elsewhere/file1").exists(),
        "the entry was written through the symlink"
    );
}

/// t2003's case 8: `--prefix` is a byte prefix, not a path component, so
/// `tmp-path1` lies outside `base_dir_len` and its symlink is replaced.
#[test]
fn index_parity_checkout_index_prefix_is_a_byte_prefix_for_the_symlink_rule() {
    let f = Fixture::new("prefix");
    std::fs::create_dir(f.work.join("tmp1")).unwrap();
    std::os::unix::fs::symlink("tmp1", f.work.join("tmp-path1")).unwrap();

    let (err, code) = f.run(&["checkout-index", "--prefix=tmp-", "-f", "-a"]);
    assert_eq!(code, 0, "prefixed checkout-index -f failed: {err}");
    assert!(f.is_real_file("tmp-path0"));
    assert!(f.is_real_dir("tmp-path1"), "the prefixed symlink was followed");
    assert!(f.is_real_file("tmp-path1/file1"));
    assert!(!f.work.join("tmp1/file1").exists());
}

/// The gitlink exemption: a directory in the way of a `160000` entry is left
/// exactly as it is, and the entry counts as done.
#[test]
fn index_parity_checkout_index_force_leaves_a_gitlink_directory_alone() {
    let f = Fixture::new("gitlink");
    let oid = "0123456789012345678901234567890123456789";
    let spec = format!("160000,{oid},sub");
    f.git(&["update-index", "--add", "--cacheinfo", &spec]);
    std::fs::create_dir(f.work.join("sub")).unwrap();
    std::fs::write(f.work.join("sub/keep"), "kept\n").unwrap();

    let (err, code) = f.run(&["checkout-index", "-f", "-a"]);
    assert_eq!(code, 0, "checkout-index -f failed on a gitlink: {err}");
    assert_eq!(
        std::fs::read_to_string(f.work.join("sub/keep")).unwrap(),
        "kept\n",
        "the submodule directory was emptied"
    );
}
