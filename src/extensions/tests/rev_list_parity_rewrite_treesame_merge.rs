//! `rewrite_parents()` stops at a TREESAME merge it cannot collapse.
//!
//! `rewrite_one_1()` walks a parent back through TREESAME commits, but when
//! `one_relevant_parent()` finds no single relevant parent it returns
//! `rewrite_one_ok` and leaves the merge in place (revision.c:4035-4054); only
//! a TREESAME *root* is dropped (`rewrite_one_noparents`). zvcs dropped the
//! merge too, so under `--simplify-merges -- <path>` the commit after a merge
//! whose two sides made the same change printed no parent at all.
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
    /// `base` → `x1` (p=1) and `y1` (p=1, q=y) → `merge` (TREESAME to both over
    /// `p`) → `after` (p=2).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-rewrite-merge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("p", "0\n");
        f.write("q", "0\n");
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "x"]);
        f.write("p", "1\n");
        f.run(&["commit", "-q", "-am", "x1"]);
        f.run(&["checkout", "-q", "-b", "y", "main"]);
        f.write("p", "1\n");
        f.write("q", "y\n");
        f.run(&["commit", "-q", "-am", "y1"]);
        f.run(&["checkout", "-q", "x"]);
        f.run(&["merge", "-q", "--no-ff", "y", "-m", "merge"]);
        f.write("p", "2\n");
        f.run(&["commit", "-q", "-am", "after"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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

const AFTER: &str = "e2e5657c84bd77de97b014562909da819266d9a0";
const MERGE: &str = "f84cadfc4ac44d6e495f67b5d223ce06d865dd3d";
const X1: &str = "957c52291a6aed8eef232fc45650db37e2b9b179";
const Y1: &str = "b38b1225a40b51953a83e0deb0326e413c206328";
const BASE: &str = "40aaa79bfc5e94394d10abf670efe88fc9e55d47";

#[test]
fn the_uncollapsible_merge_stays_a_parent() {
    let f = Fixture::new("merge");
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0.trim(), AFTER);
    let want = format!(
        "{AFTER} {MERGE}\n{MERGE} {X1} {Y1}\n{Y1} {BASE}\n{X1} {BASE}\n{BASE}\n"
    );
    for tips in [&["HEAD"][..], &["--all"]] {
        let mut args = vec!["rev-list", "--simplify-merges", "--parents"];
        args.extend_from_slice(tips);
        args.extend_from_slice(&["--", "p"]);
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0), "{tips:?}");
    }
}
