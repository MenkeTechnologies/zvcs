//! `log -g` is positional.
//!
//! `add_pending_object_with_path()` hands a commit to `add_reflog_for_walk()`
//! only once `revs->reflog_info` exists (revision.c:305-318). An argument read
//! before `-g` is pended as an ordinary commit: never walked, because
//! `get_revision_1()` takes nothing but reflog entries (revision.c:4386-4388),
//! and — when negated — UNINTERESTING, which makes the walk limited and hides
//! the entries it reaches (revision.c:431-435). One read after `-g` and negated
//! dies in `add_reflog_for_walk()` (reflog-walk.c:165-166) the moment it is
//! pended, in argument order, so `-g --not --tags main` names the tag. zvcs
//! walked a reflog for every revision wherever `-g` stood, refused a negated one
//! even before `-g`, and checked the revisions before the ref-set options.
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
        let root = std::env::temp_dir().join(format!("zvcs-log-g-positional-{tag}-{}", std::process::id()));
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

/// `main`'s reflog: one, two (tagged v1), three, reset back to two, four.
fn reflog_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f.run(&["tag", "v1"]);
    f.commit("a", "three\n", "three");
    f.run(&["reset", "-q", "--hard", "HEAD~1"]);
    f.commit("b", "four\n", "four");
    f
}

const WHOLE: &str = "main@{0}:four\nmain@{1}:two\nmain@{2}:three\nmain@{3}:two\nmain@{4}:one\n";

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn a_ref_set_before_the_revision_is_refused_first() {
    let f = reflog_fixture("order");
    assert_eq!(
        f.run(&["log", "-g", "--not", "--tags", "main"]),
        (String::new(), "fatal: cannot walk reflogs for v1\n".to_string(), 128)
    );
}

#[test]
fn a_revision_before_g_walks_no_reflog() {
    let f = reflog_fixture("before");
    assert_eq!(f.run(&["log", "--format=%gd:%s", "main", "-g"]), ok(""));
    assert_eq!(f.run(&["log", "--format=%gd:%s", "--all", "-g", "main"]), ok(WHOLE));
    assert_eq!(f.run(&["log", "--format=%gd:%s", "--tags", "-g", "main"]), ok(WHOLE));
}

#[test]
fn an_exclusion_before_g_hides_what_it_reaches() {
    let f = reflog_fixture("exclude");
    assert_eq!(
        f.run(&["log", "--format=%gd:%s", "^main~1", "-g", "main"]),
        ok("main@{0}:four\nmain@{2}:three\n")
    );
}
