//! A line that starts with a URL is not a trailer.
//!
//! 2.56's `find_separator()` refuses a `:` that opens `://` straight after the
//! key (trailer.c:638-642, "avoid accidental URL matches"). Before it,
//! `https://example.com/x` parsed as the trailer `https` with value
//! `//example.com/x`, so a message whose last paragraph was a link got
//! `interpret-trailers` rewriting it to `https: //example.com/x` and appending to
//! it, and `cherry-pick -x` treated that paragraph as a footer and glued its
//! `(cherry picked from commit …)` line on without a blank line.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-trailer-url-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main"]);
        f
    }

    fn repo(&self) -> PathBuf {
        self.root.join("r")
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.command(&self.repo(), args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn trailers(&self, args: &[&str], input: &str) -> String {
        let mut child = self
            .command(&self.repo(), &[&["interpret-trailers"], args].concat())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn a_trailing_link_is_body_text_not_a_trailer_block() {
    let f = Fixture::new("add");
    let msg = "subject\n\nhttps://example.com/x\n";
    // The link is left as written and the new trailer starts its own block.
    assert_eq!(
        f.trailers(&["--trailer", "Acked-by: X"], msg),
        "subject\n\nhttps://example.com/x\n\nAcked-by: X\n"
    );
    assert_eq!(f.trailers(&["--parse"], msg), "");
}

#[test]
fn a_link_inside_a_trailer_block_is_not_one_of_its_trailers() {
    let f = Fixture::new("parse");
    // `Signed-off-by:` is a git-generated prefix, so the block still counts; the
    // link line is its non-trailer line, while a link as a *value* is fine.
    assert_eq!(
        f.trailers(
            &["--parse"],
            "subject\n\nbody\n\nSee: https://example.com/y\nhttps://example.com/x\nSigned-off-by: A <a@x>\n",
        ),
        "See: https://example.com/y\nSigned-off-by: A <a@x>\n"
    );
    // Whitespace before the separator still makes it a key, whatever follows.
    assert_eq!(
        f.trailers(&["--parse"], "subject\n\nKey : https://example.com/k\n"),
        "Key: https://example.com/k\n"
    );
}

#[test]
fn cherry_pick_x_sets_its_line_off_from_a_trailing_link() {
    let f = Fixture::new("cherry-pick");
    std::fs::write(f.repo().join("a"), "a\n").unwrap();
    f.git(&["add", "a"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    std::fs::write(f.repo().join("b"), "b\n").unwrap();
    f.git(&["add", "b"]);
    f.git(&["commit", "-q", "-m", "pick me\n\nhttps://example.com/z"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["cherry-pick", "-x", "side"]);
    assert_eq!(
        f.git(&["log", "-1", "--format=%B"]),
        "pick me\n\nhttps://example.com/z\n\n\
         (cherry picked from commit e7485a68a945c3ddeb03bbcdae73dff5a9be30ec)\n\n"
    );
}
