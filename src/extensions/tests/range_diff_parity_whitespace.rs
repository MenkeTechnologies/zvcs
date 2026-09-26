//! `git range-diff -w` / `-b` / `--ignore-space-at-eol` / `--ignore-cr-at-eol`.
//!
//! `add_diff_options()` binds the whole `git diff` table to range-diff's
//! `diffopt` (builtin/range-diff.c:83), so the four whitespace `OPT_BIT_F`s
//! (diff.c:6196-6207) set `xpp.flags` for the *outer* diff — the diff-of-diffs
//! `patch_diff()` runs over two patch texts (range-diff.c:491-498). A matched
//! pair whose patches differ only in whitespace then prints fewer outer hunk
//! lines, the context records come from the post-image (`xdl_emit_diff()` emits
//! every context line from `xe->xdf2`), and the stat group counts under the same
//! flags (`builtin_diffstat()`, diff.c:4241-4250). The flags combine the way
//! `xdl_recmatch()` tests them — `-w` beats `-b` whatever the order
//! (xdiff/xutils.c:173-222). zvcs stopped with `fatal: unsupported flag "-w"`.
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
    /// Two one-commit series off `main` that insert the same three lines, the
    /// first with inner whitespace changed, the third with a space dropped.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("1\n2\n3\n4\n5\n6\n");
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "a"]);
        f.write("1\nfoo bar\n2\nzap\n3\nx y\n4\n5\n6\n");
        f.run(&["commit", "-q", "-am", "c1"]);
        f.run(&["checkout", "-q", "-b", "b", "main"]);
        f.write("1\nfoo  bar \n2\nzip\n3\nxy\n4\n5\n6\n");
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

    /// `range-diff --creation-factor=200 <extra> main..a main..b`, which pairs
    /// the two commits, asserting a clean exit.
    fn range_diff(&self, extra: &[&str]) -> String {
        let mut argv = vec!["range-diff", "--creation-factor=200"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["main..a", "main..b"]);
        let (out, err, code) = self.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "{argv:?}");
        out
    }
}

const HEADER: &str = "1:  364f62a ! 1:  2c89d5d c1\n";

#[test]
fn ignore_all_space_drops_whitespace_only_lines_and_keeps_post_image_context() {
    let f = Fixture::new("w");
    let want = format!(
        "{HEADER}    @@ f\n      1\n     +foo  bar \n      2\n    -+zap\n    ++zip\n      3\n     +xy\n      4\n"
    );
    assert_eq!(f.range_diff(&["-w"]), want);
    // `-w` outranks `-b` in `xdl_recmatch()`, whichever is spelled last.
    assert_eq!(f.range_diff(&["-b", "-w"]), want);
    assert_eq!(f.range_diff(&["-w", "-b"]), want);
    assert_eq!(f.range_diff(&["--ignore-all-space"]), want);
}

#[test]
fn ignore_space_change_still_sees_a_dropped_space() {
    let f = Fixture::new("b");
    assert_eq!(
        f.range_diff(&["-b"]),
        format!(
            "{HEADER}    @@ f\n      1\n     +foo  bar \n      2\n    -+zap\n    ++zip\n      3\n    -+x y\n    ++xy\n      4\n      5\n      6\n"
        )
    );
}

#[test]
fn ignore_space_at_eol_leaves_inner_whitespace_alone() {
    let f = Fixture::new("eol");
    let full = format!(
        "{HEADER}    @@ Commit message\n      ## f ##\n     @@\n      1\n    -+foo bar\n    ++foo  bar \n      2\n    -+zap\n    ++zip\n      3\n    -+x y\n    ++xy\n      4\n      5\n      6\n"
    );
    assert_eq!(f.range_diff(&["--ignore-space-at-eol"]), full);
    assert_eq!(f.range_diff(&["--ignore-cr-at-eol"]), full);
}

#[test]
fn numstat_counts_under_the_same_flags() {
    let f = Fixture::new("numstat");
    assert_eq!(f.range_diff(&["--numstat"]), format!("{HEADER}    3\t3\ta => b\n"));
    assert_eq!(f.range_diff(&["--numstat", "-w"]), format!("{HEADER}    1\t1\ta => b\n"));
}
