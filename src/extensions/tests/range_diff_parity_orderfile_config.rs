//! `diff.orderFile` reaches the `git log` that `range-diff` reads each range with.
//!
//! `read_patches()` runs a `git log -p` child (range-diff.c:44-81), which reads
//! `diff.orderFile` through `git_diff_ui_config()` (diff.c:442-445) and whose
//! `diffcore_std()` reorders each commit's queue with `diffcore_order()`
//! (diff.c:7519-7520, diffcore-order.c:112-127). The ` ## <path> ##` sections
//! of every patch therefore follow the order file, and a file that cannot be
//! read kills that log at its first non-empty queue (`prepare_order()`,
//! diffcore-order.c:24-26), which `range-diff` reports as `could not parse
//! log` and exits 255. zvcs ignored the key.
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
    /// `t1` and `t2` both edit `a` and `b` off `main`; they differ only in `a`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-orderfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "1\n2\n3\n4\n5\n");
        f.write("b", "one\ntwo\nthree\n");
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "t1"]);
        f.write("a", "1\n2\nX\n4\n5\n");
        f.write("b", "one\nTWO\nthree\n");
        f.run(&["commit", "-q", "-am", "change"]);
        f.run(&["checkout", "-q", "-b", "t2", "main"]);
        f.write("a", "1\n2\nY\n4\n5\n");
        f.write("b", "one\nTWO\nthree\n");
        f.run(&["commit", "-q", "-am", "change"]);
        std::fs::create_dir_all(f.work.join("sub")).unwrap();
        f.write("ord", "b\n");
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
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
}

/// With `b` ordered first, `a` is the last section, so no blank separator line
/// trails its hunk.
const ORDERED: &str = "\
1:  c7b4047 ! 1:  18a4f10 change
    @@ a
      1
      2
     -3
    -+X
    ++Y
      4
      5
";

#[test]
fn the_order_file_reorders_each_patch() {
    let f = Fixture::new("order");
    let (plain, _, _) = f.run(&["range-diff", "main..t1", "main..t2"]);
    assert_eq!(plain, format!("{ORDERED}     \n"));
    let (out, err, code) = f.run(&["-c", "diff.orderFile=ord", "range-diff", "main..t1", "main..t2"]);
    assert_eq!((out.as_str(), err.as_str(), code), (ORDERED, "", 0));
    // A relative name is opened from the top of the work tree, where the child
    // log stands.
    let (out, _, code) =
        f.run_in(&f.work.join("sub"), &["-c", "diff.orderFile=ord", "range-diff", "main..t1", "main..t2"]);
    assert_eq!((out.as_str(), code), (ORDERED, 0));
}

#[test]
fn an_unreadable_order_file_fails_the_first_log() {
    let f = Fixture::new("missing");
    let (out, err, code) =
        f.run(&["-c", "diff.orderFile=missing", "range-diff", "main..t1", "main..t2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: failed to read orderfile 'missing': No such file or directory\n\
             error: could not parse log for 'main..t1'\n",
            255
        )
    );
    // An `:(optional)` name that does not exist is no order file at all.
    let (out, err, code) =
        f.run(&["-c", "diff.orderFile=:(optional)missing", "range-diff", "main..t1", "main..t2"]);
    assert_eq!((out, err.as_str(), code), (format!("{ORDERED}     \n"), "", 0));
}
