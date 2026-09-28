//! `log -O<file>` and `diff.orderFile`.
//!
//! `diffcore_order()`, the last step of `diffcore_std()` (diff.c:7519-7520),
//! stably sorts each commit's queue by the first order-file pattern its path
//! matches (diffcore-order.c:112-127), for every output format. The file is read
//! by `prepare_order()` the first time a non-empty queue reaches it, and a file
//! that cannot be read dies there (diffcore-order.c:24-26) — before `show_log()`
//! has printed that record, and not at all when no diff is computed. zvcs
//! refused `-O` as unsupported and ignored `diff.orderFile`.
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
        let root = std::env::temp_dir().join(format!("zvcs-log-orderfile-{tag}-{}", std::process::id()));
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

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn every_queue_follows_the_order_file() {
    let f = history("order");
    assert_eq!(
        f.run(&["log", "--name-only", "--format=%s", "-O.git/order", "main"]),
        ok("D\n\nx\nC\n\ny\nB\n\ny\nx\nA\n\ny\nx\n")
    );
    assert_eq!(
        f.run(&["-c", "diff.orderFile=.git/order", "log", "--stat", "--format=%s", "-1", "main~2"]),
        ok("B\n\n y | 2 +-\n x | 2 +-\n 2 files changed, 2 insertions(+), 2 deletions(-)\n")
    );
    let (out, _, code) = f.run(&["log", "-p", "-1", "-O", ".git/order", "main~2"]);
    assert_eq!(code, 0);
    let (y, x) = (out.find("diff --git a/y b/y").unwrap(), out.find("diff --git a/x b/x").unwrap());
    assert!(y < x, "{out}");
}

#[test]
fn an_unreadable_file_dies_only_where_a_queue_needs_it() {
    let f = history("unreadable");
    assert_eq!(
        f.run(&["log", "-p", "-O.git/nope", "main"]),
        (
            String::new(),
            "fatal: failed to read orderfile '.git/nope': No such file or directory\n".to_string(),
            128
        )
    );
    assert_eq!(f.run(&["log", "--format=%s", "-O.git/nope", "main"]), ok("D\nC\nB\nA\n"));
}
