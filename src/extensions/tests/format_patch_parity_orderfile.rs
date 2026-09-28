//! `format-patch -O<file>` and `diff.orderFile`.
//!
//! Every patch's queue goes through `diffcore_std()`, whose `diffcore_order()`
//! (diff.c:7519-7520) sorts it by the order file before the diffstat and the
//! patch are written (diffcore-order.c:112-127). The file is read at the first
//! non-empty queue and a file that cannot be read dies there
//! (diffcore-order.c:24-26) — with a cover letter, that is its diffstat, after
//! the headers and the shortlog are already out. zvcs refused `-O` and ignored
//! `diff.orderFile`.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fp-orderfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// A: x, y. B: x, y. C: y. D: x. `.git/order` puts y first.
fn history(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    std::fs::write(f.work.join("x"), "x1\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y1\n", "A");
    std::fs::write(f.work.join("x"), "x2\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y2\n", "B");
    f.commit("y", "y3\n", "C");
    f.commit("x", "x4\n", "D");
    std::fs::write(f.work.join(".git/order"), "y\nx\n").unwrap();
    f
}

fn body(out: &str) -> &str {
    let start = out.find("\n---\n").expect("a diffstat");
    &out[start..]
}

#[test]
fn the_diffstat_and_the_patch_follow_the_order_file() {
    let f = history("order");
    for args in [
        &["format-patch", "--stdout", "-1", "-O.git/order", "main~2"][..],
        &["-c", "diff.orderFile=.git/order", "format-patch", "--stdout", "-1", "main~2"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        let b = body(&out);
        assert!(b.starts_with("\n---\n y | 2 +-\n x | 2 +-\n"), "{args:?}: {out}");
        assert!(b.find("diff --git a/y b/y").unwrap() < b.find("diff --git a/x b/x").unwrap(), "{out}");
    }
}

#[test]
fn an_unreadable_file_dies_at_the_cover_letter_diffstat() {
    let f = history("unreadable");
    let (out, err, code) = f.run(&["format-patch", "--stdout", "--cover-letter", "-O.git/nope", "-2", "main"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: failed to read orderfile '.git/nope': No such file or directory\n");
    assert!(out.ends_with("*** BLURB HERE ***\n\nA U Thor (2):\n  C\n  D\n\n"), "{out}");
    let (out, err, code) = f.run(&["format-patch", "--stdout", "-O.git/nope", "-1", "main"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: failed to read orderfile '.git/nope': No such file or directory\n");
}

#[test]
fn the_patch_file_is_opened_before_the_diff_dies() {
    let f = history("file");
    let (out, err, code) = f.run(&["format-patch", "-o", "out", "-O.git/nope", "-1", "main"]);
    assert_eq!((out.as_str(), code), ("out/0001-D.patch\n", 128));
    assert_eq!(err, "fatal: failed to read orderfile '.git/nope': No such file or directory\n");
    assert_eq!(std::fs::read(f.work.join("out/0001-D.patch")).unwrap(), b"");
}
