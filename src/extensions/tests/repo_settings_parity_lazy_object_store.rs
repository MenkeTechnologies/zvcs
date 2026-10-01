//! `prepare_repo_settings()` is reached by the first object access, and only by it.
//!
//! git prepares the settings lazily. Most commands not covered by an explicit
//! `prepare_repo_settings()` call get there through the object database: the
//! first lookup, write or count prepares the packed source, which calls
//! `prepare_multi_pack_index_one()`, which calls `prepare_repo_settings()`
//! (odb/source-packed.c:832-842, midx.c:741-745, v2.56.0). A bad value in a
//! key the settings read then dies in `branch --merged` but not `branch`,
//! in `for-each-ref` but not `for-each-ref --format=%(refname)`, and in
//! `hash-object -w` but not `hash-object`. zvcs exited 0 for every one of
//! them.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const HEAD: &str = "14d5db65fc01e7fd0b3577571213e1725e5e53f0";
const REFUSAL: &str = "fatal: bad boolean config value 'abc' for 'core.commitgraph'\n";

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-lazy-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f
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

    fn bad(&self, args: &[&str]) -> (String, String, i32) {
        let mut full = vec!["-c", "core.commitGraph=abc"];
        full.extend_from_slice(args);
        self.run(&full)
    }
}

#[test]
fn commands_that_open_an_object_die() {
    let f = Fixture::new("die");
    for args in [
        &["branch", "--merged"][..],
        &["branch", "-v"],
        &["for-each-ref"],
        &["tag", "--contains", "HEAD"],
        &["hash-object", "-w", "file"],
    ] {
        assert_eq!(f.bad(args), (String::new(), REFUSAL.to_string(), 128), "git {args:?}");
    }
}

#[test]
fn update_ref_dies_before_writing_the_ref() {
    let f = Fixture::new("update-ref");
    assert_eq!(
        f.bad(&["update-ref", "refs/heads/other", "HEAD"]),
        (String::new(), REFUSAL.to_string(), 128)
    );
    assert_eq!(f.run(&["show-ref"]).0, format!("{HEAD} refs/heads/main\n"));
}

#[test]
fn commands_that_never_open_an_object_run() {
    let f = Fixture::new("run");
    let cases: &[(&[&str], String)] = &[
        (&["branch"], "* main\n".to_string()),
        (&["for-each-ref", "--format=%(refname)"], "refs/heads/main\n".to_string()),
        (&["for-each-ref", "--format=%(objectname)"], format!("{HEAD}\n")),
        (&["hash-object", "file"], "df967b96a579e45a18b8251732d16804b2e56a55\n".to_string()),
        (&["symbolic-ref", "HEAD"], "refs/heads/main\n".to_string()),
    ];
    for (args, want) in cases {
        assert_eq!(f.bad(args), (want.clone(), String::new(), 0), "git {args:?}");
    }
}
