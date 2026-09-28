//! Automatic tag following for a tag on a commit the repository already has.
//!
//! `find_non_local_tags()` (builtin/fetch.c:355-406) puts a remote tag in the
//! ref map when its object, or the object it peels to, is already local or
//! is being fetched, and `fetch_refs()` then wants every ref whose object is
//! missing. So an annotated tag created upstream on a commit the clone
//! already has is fetched by a plain `git fetch`. zvcs left every implicit
//! tag to the server's `include-tag`, which only sends tags pointing into the
//! pack: the tag object never came, and the tag was silently dropped.
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
    /// `up` has one commit, `work` clones it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-follow-local-tag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        f.up(&["init", "-q", "-b", "main", "."]);
        f.up(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        std::fs::create_dir_all(dir).unwrap();
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn up(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.root.join("up"), args)
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }
}

#[test]
fn an_annotated_tag_on_a_known_commit_is_fetched() {
    let f = Fixture::new("annotated");
    f.up(&["tag", "-a", "-m", "m", "ann"]);
    f.up(&["tag", "light"]);
    let (out, err, code) = f.run(&["fetch"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(
        err.ends_with(
            " * [new tag]         ann        -> ann\n * [new tag]         light      -> light\n"
        ),
        "{err}"
    );
    let tag = f.up(&["rev-parse", "ann"]).0;
    assert_eq!(f.run(&["rev-parse", "ann"]).0, tag);
    assert_eq!(f.run(&["cat-file", "-t", "ann"]).0, "tag\n");
}

#[test]
fn a_tag_outside_the_fetched_history_is_still_left_alone() {
    let f = Fixture::new("outside");
    f.run(&["config", "remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main"]);
    f.up(&["checkout", "-q", "-b", "side"]);
    f.up(&["commit", "-q", "--allow-empty", "-m", "s"]);
    f.up(&["tag", "-a", "-m", "x", "annout"]);
    f.up(&["checkout", "-q", "main"]);
    f.up(&["tag", "-a", "-m", "y", "annin"]);
    let (_, err, code) = f.run(&["fetch"]);
    assert_eq!(code, 0);
    assert!(err.ends_with("\n * [new tag]         annin      -> annin\n"), "{err}");
    assert_eq!(f.run(&["tag"]).0, "annin\n");
}
