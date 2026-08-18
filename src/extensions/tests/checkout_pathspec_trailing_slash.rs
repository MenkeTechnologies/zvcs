//! `git checkout -- <dir>/` — a pathspec that ends at a directory boundary.
//!
//! git normalises a pathspec before matching, so `sub/`, `sub//`, `sub/.` and
//! `sub/./` all name `sub`, and a `..` pops a component (`top.txt/..` names the
//! whole tree). What survives normalisation is whether the spec *ended* on a
//! slash, a `.` or a `..`: that makes it a directory spec, so `top.txt/` must
//! NOT match the file `top.txt` even though `top.txt` does.
//!
//! The port previously required a further `/` after the spec's own trailing one,
//! so every directory spec written with a trailing slash matched nothing and
//! exited 1 with `did not match any file(s)` — leaving the files dirty. Shell
//! tab-completion appends that slash, so `git checkout -- src/` was the common
//! way to hit it, while `git restore src/` worked.
//!
//! Every expectation below was measured against stock git 2.55.0 first.
#![cfg(unix)]

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
    /// `top.txt`, `sub/a.txt`, `sub/deep/c.txt`, all committed, then all dirtied.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-coslash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("top.txt", "top\n");
        f.write("sub/a.txt", "one\n");
        f.write("sub/deep/c.txt", "deep\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// Dirty every tracked file, so a successful restore is visible.
    fn dirty(&self) {
        self.write("top.txt", "DIRTY\n");
        self.write("sub/a.txt", "DIRTY\n");
        self.write("sub/deep/c.txt", "DIRTY\n");
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }

    fn checkout(&self, spec: &str) -> (String, i32) {
        self.git(&["checkout", "--", spec])
    }
}

#[test]
fn a_directory_spec_restores_whatever_spelling_it_is_written_in() {
    let f = Fixture::new("dir");
    // Every spelling git reduces to `sub`. `top.txt` must be left alone by all of
    // them, which is what proves the spec was scoped rather than widened to root.
    for spec in ["sub", "sub/", "sub//", "sub///", "sub/.", "sub/./", "./sub", "./sub/"] {
        f.dirty();
        let (out, rc) = f.checkout(spec);
        assert_eq!(rc, 0, "spec {spec:?} should succeed, got: {out}");
        assert_eq!(f.read("sub/a.txt"), "one\n", "spec {spec:?} should restore sub/a.txt");
        assert_eq!(f.read("sub/deep/c.txt"), "deep\n", "spec {spec:?} should restore sub/deep/c.txt");
        assert_eq!(f.read("top.txt"), "DIRTY\n", "spec {spec:?} must not reach top.txt");
    }
}

#[test]
fn a_trailing_slash_still_demands_a_directory() {
    let f = Fixture::new("file");
    // `top.txt` is a file, so a spec that ends at a directory boundary cannot name
    // it. Stock reports the pathspec verbatim, exits 1, and restores nothing.
    for spec in ["top.txt/", "top.txt/."] {
        f.dirty();
        let (out, rc) = f.checkout(spec);
        assert_eq!(rc, 1, "spec {spec:?} should fail, got: {out}");
        assert!(
            out.contains(&format!("pathspec '{spec}' did not match any file(s) known to git")),
            "spec {spec:?} wrong message: {out}"
        );
        assert_eq!(f.read("top.txt"), "DIRTY\n", "spec {spec:?} must restore nothing");
    }
    // And the bare name still works, which is the half a blind slash-strip breaks.
    f.dirty();
    let (out, rc) = f.checkout("top.txt");
    assert_eq!(rc, 0, "bare top.txt should succeed, got: {out}");
    assert_eq!(f.read("top.txt"), "top\n");
    assert_eq!(f.read("sub/a.txt"), "DIRTY\n", "top.txt must not reach sub/");
}

#[test]
fn a_parent_component_pops_and_can_reach_the_whole_tree() {
    let f = Fixture::new("dotdot");
    // `sub/deep/..` reduces to `sub`; `top.txt/..` reduces to the root, so it
    // restores everything — including top.txt itself.
    f.dirty();
    let (out, rc) = f.checkout("sub/deep/..");
    assert_eq!(rc, 0, "sub/deep/.. should succeed, got: {out}");
    assert_eq!(f.read("sub/a.txt"), "one\n");
    assert_eq!(f.read("top.txt"), "DIRTY\n", "sub/deep/.. must not reach top.txt");

    f.dirty();
    let (out, rc) = f.checkout("top.txt/..");
    assert_eq!(rc, 0, "top.txt/.. should succeed, got: {out}");
    assert_eq!(f.read("top.txt"), "top\n");
    assert_eq!(f.read("sub/a.txt"), "one\n", "top.txt/.. names the whole tree");
}

#[test]
fn a_missing_directory_is_still_an_error() {
    let f = Fixture::new("missing");
    f.dirty();
    let (out, rc) = f.checkout("nosuch/");
    assert_eq!(rc, 1, "nosuch/ should fail, got: {out}");
    assert!(out.contains("pathspec 'nosuch/' did not match any file(s) known to git"), "{out}");
    assert_eq!(f.read("sub/a.txt"), "DIRTY\n", "a failed pathspec restores nothing");
}
