//! `git add` stages the *converted* bytes, not the worktree copy.
//!
//! The pipeline on the way into the object database is `clean` drivers,
//! `working-tree-encoding`, `ident` and the EOL normalization `text` /
//! `core.autocrlf` ask for. Staging the verbatim file writes a different blob
//! than git does in any repository that normalizes line endings — the whole
//! history diverges from there, and `git status` never settles because the
//! worktree copy and the stored one disagree by design.
//!
//! Every expectation here is the object id stock git writes for the same input.
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
        let root = std::env::temp_dir().join(format!("zvcs-addfilter-{tag}-{}", std::process::id()));
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
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// The blob id staged for `path`.
    fn staged_id(&self, path: &str) -> String {
        self.run(&["rev-parse", &format!(":{path}")]).1.trim().to_string()
    }

    fn staged_content(&self, path: &str) -> String {
        self.run(&["show", &format!(":{path}")]).1
    }
}

/// `* text=auto` normalizes CRLF on the way in: the stored blob has LF, and the
/// conversion is announced the way git announces it.
#[test]
fn text_auto_normalizes_and_warns() {
    let f = Fixture::new("text-auto");
    f.write(".gitattributes", b"* text=auto\n");
    f.git(&["add", ".gitattributes"]);
    f.git(&["commit", "-q", "-m", "attrs"]);
    f.write("w.txt", b"l1\r\nl2\r\n");

    let (code, out, err) = f.run(&["add", "w.txt"]);
    assert_eq!(code, 0, "add failed: {out}{err}");
    assert_eq!(
        err,
        "warning: in the working copy of 'w.txt', CRLF will be replaced by LF the next time Git touches it\n",
        "stderr: {err}"
    );
    // The blob stock git writes for `l1\nl2\n`.
    assert_eq!(f.staged_id("w.txt"), "3582182a1f64c9f6ec59ce981bfe0a59e85f1301");
    assert_eq!(f.staged_content("w.txt"), "l1\nl2\n");
}

/// `core.safecrlf=true` turns the same conversion into a refusal: git names the
/// path, stages nothing and exits 128.
#[test]
fn safecrlf_refuses_and_stages_nothing() {
    let f = Fixture::new("safecrlf");
    f.write(".gitattributes", b"* text=auto\n");
    f.git(&["add", ".gitattributes"]);
    f.git(&["commit", "-q", "-m", "attrs"]);
    f.write("w.txt", b"l1\r\nl2\r\n");

    let (code, out, err) = f.run(&["-c", "core.safecrlf=true", "add", "w.txt"]);
    assert_eq!(code, 128, "wrong exit: {out}{err}");
    assert_eq!(err, "fatal: CRLF would be replaced by LF in w.txt\n", "stderr: {err}");
    assert_eq!(f.run(&["diff", "--cached", "--name-only"]).1, "", "nothing may be staged");
}

/// `core.autocrlf` reaches the same pipeline without any attributes file.
#[test]
fn autocrlf_normalizes_without_attributes() {
    let f = Fixture::new("autocrlf");
    f.git(&["config", "core.autocrlf", "true"]);
    f.write("f.txt", b"x\r\ny\r\n");

    let (code, out, err) = f.run(&["add", "f.txt"]);
    assert_eq!(code, 0, "add failed: {out}{err}");
    // The blob stock git writes for `x\ny\n`.
    assert_eq!(f.staged_id("f.txt"), "b77b4eb1d946f923f61785536da9ca5af6909f06");
}

/// An external `clean` driver runs too — the blob is its output, not the file.
#[test]
fn a_clean_driver_produces_the_blob() {
    let f = Fixture::new("clean");
    f.git(&["config", "filter.demo.clean", "tr a-z A-Z"]);
    f.write(".gitattributes", b"f.txt filter=demo\n");
    f.write("f.txt", b"hello\n");

    let (code, out, err) = f.run(&["add", ".gitattributes", "f.txt"]);
    assert_eq!(code, 0, "add failed: {out}{err}");
    assert_eq!(f.staged_content("f.txt"), "HELLO\n");
    // The blob stock git writes for `HELLO\n`.
    assert_eq!(f.staged_id("f.txt"), "e427984d4a2c1904681f2e2ee5980f37640d353f");
}

/// A symlink's target is stored verbatim: no pipeline runs on it.
#[test]
fn a_symlink_target_is_not_filtered() {
    let f = Fixture::new("symlink");
    f.git(&["config", "core.autocrlf", "true"]);
    f.write("target.txt", b"t\n");
    std::os::unix::fs::symlink("target.txt", f.work.join("link")).unwrap();

    let (code, out, err) = f.run(&["add", "link"]);
    assert_eq!(code, 0, "add failed: {out}{err}");
    assert_eq!(f.staged_content("link"), "target.txt");
}
