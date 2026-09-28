//! `show -O<file>` and `diff.orderFile`.
//!
//! `cmd_show` sets `rev.diff`, so every commit it prints goes through
//! `diffcore_std()`, whose last step `diffcore_order()` (diff.c:7519-7520) sorts
//! the queue by the order file (diffcore-order.c:112-127). The file is read at
//! the first non-empty queue — `-s` included — and a file that cannot be read
//! dies there (diffcore-order.c:24-26), after whatever blob or tree was shown
//! before it. A clean merge's combined queue is empty, so it never reads the
//! file. zvcs refused `-O` and ignored `diff.orderFile`.
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
        let root = std::env::temp_dir().join(format!("zvcs-show-orderfile-{tag}-{}", std::process::id()));
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

/// A: x, y. B: x, y. `.git/order` puts y first.
fn history(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    std::fs::write(f.work.join("x"), "x1\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y1\n", "A");
    std::fs::write(f.work.join("x"), "x2\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y2\n", "B");
    std::fs::write(f.work.join(".git/order"), "y\nx\n").unwrap();
    f
}

const FATAL: &str = "fatal: failed to read orderfile '.git/nope': No such file or directory\n";

#[test]
fn the_queue_follows_the_order_file() {
    let f = history("order");
    let want = "B\n\n y | 2 +-\n x | 2 +-\n 2 files changed, 2 insertions(+), 2 deletions(-)\n";
    assert_eq!(
        f.run(&["show", "-O.git/order", "--format=%s", "--stat", "main"]),
        (want.to_string(), String::new(), 0)
    );
    assert_eq!(
        f.run(&["-c", "diff.orderFile=.git/order", "show", "--format=%s", "--stat", "main"]),
        (want.to_string(), String::new(), 0)
    );
    assert_eq!(
        f.run(&["show", "-O", ".git/order", "--format=%s", "--name-only", "main"]),
        ("B\n\ny\nx\n".to_string(), String::new(), 0)
    );
}

#[test]
fn an_unreadable_file_dies_at_the_first_queue() {
    let f = history("unreadable");
    assert_eq!(f.run(&["show", "-s", "-O.git/nope", "main"]), (String::new(), FATAL.to_string(), 128));
    assert_eq!(f.run(&["show", "-O.git/nope", "main:x", "main"]), ("x2\n".to_string(), FATAL.to_string(), 128));
}

#[test]
fn a_clean_merge_reads_nothing() {
    let f = history("merge");
    f.run(&["checkout", "-q", "-b", "side", "main~1"]);
    f.commit("z", "z\n", "S");
    f.run(&["checkout", "-q", "main"]);
    f.run(&["merge", "-q", "--no-edit", "side", "-m", "M"]);
    // `diff_tree_combined()` still writes the header and its blank line.
    assert_eq!(
        f.run(&["show", "-O.git/nope", "--format=%s", "main"]),
        ("M\n\n".to_string(), String::new(), 0)
    );
}
