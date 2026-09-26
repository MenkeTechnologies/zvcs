//! `git range-diff --anchored=<text>`.
//!
//! `diff_opt_anchored()` (diff.c:5544-5555) switches the outer diff to patience
//! and appends an anchor, which `xdl_do_patience_diff()` keeps unchanged: a
//! post-image record starting with the text is forced to be common. Because
//! range-diff's outer diff runs over two patch texts, the anchor is matched
//! against patch lines such as `+3`. `diff_opt_patience()` drops every anchor given
//! before it (diff.c:5838-5857), while `--diff-algorithm=patience` keeps them and
//! `--histogram` makes them inert. zvcs stopped with `fatal: unsupported flag`.
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
    /// `p` appends `1 2 3` to `f`, `q` appends `3 1 2`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-anchored-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a\nb\nc\n");
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "p"]);
        f.write("a\nb\nc\n1\n2\n3\n");
        f.run(&["commit", "-q", "-am", "c1"]);
        f.run(&["checkout", "-q", "-b", "q", "main"]);
        f.write("a\nb\nc\n3\n1\n2\n");
        f.run(&["commit", "-q", "-am", "c1"]);
        f
    }

    fn write(&self, body: &str) {
        std::fs::write(self.work.join("f"), body).unwrap();
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

    /// `range-diff --creation-factor=300 <extra> main..p main..q`, asserting a
    /// clean exit.
    fn range_diff(&self, extra: &[&str]) -> String {
        let mut argv = vec!["range-diff", "--creation-factor=300"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["main..p", "main..q"]);
        let (out, err, code) = self.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "{argv:?}");
        out
    }
}

const PLAIN: &str = "1:  6ff2f52 ! 1:  4e2180a c1\n    @@ f\n      a\n      b\n      c\n    ++3\n     +1\n     +2\n    -+3\n";
const ANCHORED: &str = "1:  6ff2f52 ! 1:  4e2180a c1\n    @@ f\n      a\n      b\n      c\n    -+1\n    -+2\n     +3\n    ++1\n    ++2\n";

#[test]
fn an_anchor_keeps_the_matching_patch_line_common() {
    let f = Fixture::new("anchor");
    assert_eq!(f.range_diff(&[]), PLAIN);
    assert_eq!(f.range_diff(&["--anchored=+3"]), ANCHORED);
    assert_eq!(f.range_diff(&["--anchored", "+3"]), ANCHORED);
    // `+1` and `+3` stand in opposite orders on the two sides, so both cannot
    // stay common; the longest run of unique common lines wins, as without them.
    assert_eq!(f.range_diff(&["--anchored=+1", "--anchored=+3"]), PLAIN);
}

#[test]
fn patience_forgets_earlier_anchors_but_diff_algorithm_does_not() {
    let f = Fixture::new("patience");
    assert_eq!(f.range_diff(&["--anchored=+3", "--patience"]), PLAIN);
    assert_eq!(f.range_diff(&["--patience", "--anchored=+3"]), ANCHORED);
    assert_eq!(f.range_diff(&["--anchored=+3", "--diff-algorithm=patience"]), ANCHORED);
    // Anchors only steer the patience algorithm.
    assert_eq!(f.range_diff(&["--anchored=+3", "--histogram"]), PLAIN);
}
