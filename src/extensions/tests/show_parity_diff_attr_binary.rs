//! The diffstat's binary test honours the `diff` attribute. `builtin_diffstat()`
//! asks `diff_filespec_is_binary()` of each side (diff.c:4213-4223), and that takes
//! the path's diff driver before it sniffs the bytes (diff.c:3712-3734): `-diff` is
//! `driver_false` (binary), a set `diff` is `driver_true` (text), and a named
//! driver's `diff.<name>.binary` decides (userdiff.c:372-380, 536-546).
//!
//! `show`/`log` and the `diff-tree`/`diff-index` count formats only sniffed for a
//! NUL, so a `-diff` text file was counted in lines; `diff-files --stat` printed a
//! bare `Bin` because the blob pipeline keeps only the size of binary content; and
//! the blob pipeline itself treated a set `diff` as "no opinion", so a NUL-bearing
//! file marked `diff` was `Bin` in `diff --stat` and `Binary files differ` in `-p`.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Commit 1 → commit 2 changes five paths, one per attribute shape: `-diff` text
/// modified (`a.dat`), deleted (`d.dat`) and added (`e.dat`); a set `diff` on a
/// NUL-bearing file (`b.bin`); and a named driver with `binary = true` (`c.drv`).
/// The worktree then modifies `a.dat` and `c.drv` again.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-statattr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fx = Fixture { dir: dir.canonicalize().unwrap() };
        fx.ok(&["init", "-q", "-b", "main"]);
        fx.ok(&["config", "diff.bd.binary", "true"]);
        fx.write(".gitattributes", b"*.dat -diff\n*.bin diff\n*.drv diff=bd\n");
        fx.write("a.dat", b"one\n");
        fx.write("b.bin", b"x\0y\n");
        fx.write("c.drv", b"t\n");
        fx.write("d.dat", b"gone\n");
        fx.ok(&["add", "."]);
        fx.commit("1");
        fx.write("a.dat", b"one\ntwo\n");
        fx.write("b.bin", b"x\0y\nz\n");
        fx.write("c.drv", b"t\nu\n");
        fx.ok(&["rm", "-q", "d.dat"]);
        fx.write("e.dat", b"new\n");
        fx.ok(&["add", "."]);
        fx.commit("2");
        fx.write("a.dat", b"one\ntwo\nthree\n");
        fx.write("c.drv", b"t\nu\nv\n");
        fx
    }

    fn write(&self, name: &str, body: &[u8]) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.ok(&["-c", "user.name=t", "-c", "user.email=t@e.x", "commit", "-q", "-m", msg]);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("COLUMNS", "80")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const COMMIT_STAT: &str = " a.dat | Bin 4 -> 8 bytes
 b.bin |   1 +
 c.drv | Bin 2 -> 4 bytes
 d.dat | Bin 5 -> 0 bytes
 e.dat | Bin 0 -> 4 bytes
 5 files changed, 1 insertion(+)
";

const COMMIT_NUMSTAT: &str = "-\t-\ta.dat\n1\t0\tb.bin\n-\t-\tc.drv\n-\t-\td.dat\n-\t-\te.dat\n";

const WORKTREE_STAT: &str = " a.dat | Bin 8 -> 14 bytes
 c.drv | Bin 4 -> 6 bytes
 2 files changed, 0 insertions(+), 0 deletions(-)
";

#[test]
fn show_and_log_count_formats_ask_the_driver() {
    let fx = Fixture::new("log");
    assert_eq!(fx.ok(&["show", "--stat", "--format="]), COMMIT_STAT);
    assert_eq!(fx.ok(&["show", "--numstat", "--format="]), COMMIT_NUMSTAT);
    assert_eq!(fx.ok(&["log", "-1", "--stat", "--format="]), COMMIT_STAT);
    assert_eq!(fx.ok(&["log", "-1", "--numstat", "--format="]), COMMIT_NUMSTAT);
}

#[test]
fn plumbing_count_formats_ask_the_driver() {
    let fx = Fixture::new("plumb");
    assert_eq!(fx.ok(&["diff-tree", "--numstat", "HEAD~", "HEAD"]), COMMIT_NUMSTAT);
    assert_eq!(fx.ok(&["diff-tree", "--stat", "HEAD~", "HEAD"]), COMMIT_STAT);
    assert_eq!(fx.ok(&["diff-index", "--stat", "HEAD"]), WORKTREE_STAT);
    assert_eq!(fx.ok(&["diff-index", "--numstat", "HEAD"]), "-\t-\ta.dat\n-\t-\tc.drv\n");
    assert_eq!(fx.ok(&["diff-files", "--stat"]), WORKTREE_STAT);
}

#[test]
fn a_set_diff_attribute_makes_a_nul_file_text() {
    let fx = Fixture::new("text");
    assert_eq!(fx.ok(&["diff", "--stat", "HEAD~", "HEAD"]), COMMIT_STAT);
    let patch = fx.ok(&["diff", "HEAD~", "HEAD", "--", "b.bin"]);
    assert!(patch.ends_with("@@ -1 +1,2 @@\n x\0y\n+z\n"), "{patch:?}");
}
